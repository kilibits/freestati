//! FreeStati data engine — Polars (Rust) port of the former Python engine.
//!
//! Holds the active dataset as a Polars `DataFrame` plus per-column metadata,
//! and serves the same operations the renderer used over IPC before:
//! load_file, get_page, get_variables, set_variable_meta, update_cell,
//! save_file, new_dataset.
//!
//! Format coverage in this first Rust port: .tab / .tsv / .csv (read + write).
//! .xlsx / .sav / .dta / .sas7bdat are not yet ported (return a clear error).

use std::collections::HashMap;
use std::path::Path;

use polars::prelude::*;
use serde::Serialize;
use serde_json::Value as JsonValue;

/// Inference scans only the first N rows — a display heuristic, not statistics —
/// so load time stays O(cols) instead of O(rows × cols) on tall datasets.
const SAMPLE_N: usize = 10_000;

/// Per-column metadata overrides (mirrors the Python `_var_meta`).
#[derive(Default, Clone)]
pub struct VarMeta {
    pub label: Option<String>,
    pub var_type: Option<String>,
    pub width: Option<u32>,
    pub decimals: Option<u32>,
    pub columns: Option<u32>,
    pub align: Option<String>,
    pub value_labels: Option<HashMap<String, String>>,
    pub measure_level: Option<String>,
    pub role: Option<String>,
}

/// The engine state, guarded by a Mutex in the Tauri `AppState`.
#[derive(Default)]
pub struct Engine {
    pub df: Option<DataFrame>,
    pub var_meta: HashMap<String, VarMeta>,
    pub path: Option<String>,
    pub float_cols: Vec<String>,
    cached_variables: Option<Vec<Variable>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub label: String,
    #[serde(rename = "type")]
    pub var_type: String,
    pub width: u32,
    pub decimals: u32,
    pub columns: u32,
    pub align: String,
    pub value_labels: HashMap<String, String>,
    pub missing_values: Vec<JsonValue>,
    pub measure_level: String,
    pub role: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadResult {
    pub row_count: usize,
    pub col_count: usize,
    pub variables: Vec<Variable>,
    pub filename: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct PageResult {
    /// Pre-serialized JSON array of row objects; the renderer parses it with
    /// V8's native JSON.parse (mirrors the old `rows_raw` fast path).
    pub rows_raw: String,
    pub total: usize,
}

type EResult<T> = Result<T, String>;

fn map_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ── Inference helpers ───────────────────────────────────────────────────────

fn series_type(dtype: &DataType) -> &'static str {
    if dtype.is_temporal() {
        "date"
    } else if dtype.is_numeric() {
        "numeric"
    } else {
        "string"
    }
}

fn infer_measure(series: &Series) -> String {
    if !series.dtype().is_numeric() {
        return "nominal".into();
    }
    let head = series.head(Some(SAMPLE_N));
    match head.n_unique() {
        Ok(n) if n <= 10 => "ordinal".into(),
        _ => "scale".into(),
    }
}

fn infer_decimals(series: &Series) -> u32 {
    let dtype = series.dtype();
    if !dtype.is_numeric() {
        return 0;
    }
    // Integer dtypes never need decimals — no scan required.
    if dtype.is_integer() {
        return 0;
    }
    // Float: sample and check whether every non-null value is integral.
    let head = series.head(Some(SAMPLE_N));
    if let Ok(ca) = head.f64() {
        let all_integral = ca.into_iter().flatten().all(|v| v.fract() == 0.0);
        return if all_integral { 0 } else { 2 };
    }
    2
}

// ── Engine implementation ───────────────────────────────────────────────────

impl Engine {
    fn reset(&mut self) {
        self.df = None;
        self.var_meta.clear();
        self.path = None;
        self.float_cols.clear();
        self.cached_variables = None;
    }

    pub fn new_dataset(&mut self) -> LoadResult {
        self.reset();
        self.df = Some(DataFrame::empty());
        LoadResult {
            row_count: 0,
            col_count: 0,
            variables: vec![],
            filename: String::new(),
            path: String::new(),
        }
    }

    pub fn load_file(&mut self, path: &str) -> EResult<LoadResult> {
        self.reset();
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let df = match ext.as_str() {
            "tab" | "tsv" => read_delimited(path, b'\t')?,
            "csv" => read_delimited(path, b',')?,
            "xlsx" | "xls" | "sav" | "dta" | "sas7bdat" => {
                return Err(format!(
                    ".{ext} is not yet supported by the native engine (only .tab/.tsv/.csv). \
                     This format will be restored in a later release."
                ));
            }
            other => return Err(format!("Unsupported file type: .{other}")),
        };

        self.float_cols = df
            .get_columns()
            .iter()
            .filter(|c| matches!(c.dtype(), DataType::Float32 | DataType::Float64))
            .map(|c| c.name().to_string())
            .collect();

        let row_count = df.height();
        let col_count = df.width();
        self.df = Some(df);
        self.path = Some(path.to_string());

        let filename = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        Ok(LoadResult {
            row_count,
            col_count,
            variables: self.variables(),
            filename,
            path: path.to_string(),
        })
    }

    pub fn variables(&mut self) -> Vec<Variable> {
        if let Some(cached) = &self.cached_variables {
            return cached.clone();
        }
        let Some(df) = &self.df else {
            return vec![];
        };
        let mut out = Vec::with_capacity(df.width());
        for col in df.get_columns() {
            let name = col.name().to_string();
            let series = col.as_materialized_series();
            let m = self.var_meta.get(&name);
            let inferred_type = series_type(series.dtype()).to_string();
            let var_type = m
                .and_then(|m| m.var_type.clone())
                .unwrap_or(inferred_type.clone());
            let decimals = m.and_then(|m| m.decimals).unwrap_or_else(|| {
                if var_type == "numeric" {
                    infer_decimals(series)
                } else {
                    0
                }
            });
            out.push(Variable {
                name,
                label: m.and_then(|m| m.label.clone()).unwrap_or_default(),
                var_type,
                width: m.and_then(|m| m.width).unwrap_or(8),
                decimals,
                columns: m.and_then(|m| m.columns).unwrap_or(8),
                align: m.and_then(|m| m.align.clone()).unwrap_or("left".into()),
                value_labels: m.and_then(|m| m.value_labels.clone()).unwrap_or_default(),
                missing_values: vec![],
                measure_level: m
                    .and_then(|m| m.measure_level.clone())
                    .unwrap_or_else(|| infer_measure(series)),
                role: m.and_then(|m| m.role.clone()).unwrap_or("input".into()),
            });
        }
        self.cached_variables = Some(out.clone());
        out
    }

    pub fn get_page(&self, offset: usize, limit: usize) -> EResult<PageResult> {
        let Some(df) = &self.df else {
            return Ok(PageResult { rows_raw: "[]".into(), total: 0 });
        };
        let total = df.height();
        if offset >= total {
            return Ok(PageResult { rows_raw: "[]".into(), total });
        }
        let len = limit.min(total - offset);
        let mut chunk = df.slice(offset as i64, len);

        // 1-based case number column.
        let row_nums: Vec<i64> = (0..len as i64).map(|i| offset as i64 + 1 + i).collect();
        let row_col = Series::new("__row__".into(), row_nums);
        chunk.with_column(row_col).map_err(map_err)?;

        // Replace NaN/Inf in float columns with null so the JSON is valid.
        if !self.float_cols.is_empty() {
            let exprs: Vec<Expr> = self
                .float_cols
                .iter()
                .map(|c| {
                    when(col(c.as_str()).is_finite())
                        .then(col(c.as_str()))
                        .otherwise(lit(NULL))
                        .alias(c.as_str())
                })
                .collect();
            chunk = chunk.lazy().with_columns(exprs).collect().map_err(map_err)?;
        }

        let mut buf = Vec::new();
        JsonWriter::new(&mut buf)
            .with_json_format(JsonFormat::Json)
            .finish(&mut chunk)
            .map_err(map_err)?;
        let rows_raw = String::from_utf8(buf).map_err(map_err)?;

        Ok(PageResult { rows_raw, total })
    }

    pub fn set_variable_meta(&mut self, name: &str, meta: &JsonValue) {
        let entry = self.var_meta.entry(name.to_string()).or_default();
        if let Some(v) = meta.get("label").and_then(|v| v.as_str()) {
            entry.label = Some(v.to_string());
        }
        if let Some(v) = meta.get("type").and_then(|v| v.as_str()) {
            entry.var_type = Some(v.to_string());
        }
        if let Some(v) = meta.get("width").and_then(|v| v.as_u64()) {
            entry.width = Some(v as u32);
        }
        if let Some(v) = meta.get("decimals").and_then(|v| v.as_u64()) {
            entry.decimals = Some(v as u32);
        }
        if let Some(v) = meta.get("columns").and_then(|v| v.as_u64()) {
            entry.columns = Some(v as u32);
        }
        if let Some(v) = meta.get("align").and_then(|v| v.as_str()) {
            entry.align = Some(v.to_string());
        }
        if let Some(v) = meta.get("measureLevel").and_then(|v| v.as_str()) {
            entry.measure_level = Some(v.to_string());
        }
        if let Some(v) = meta.get("role").and_then(|v| v.as_str()) {
            entry.role = Some(v.to_string());
        }
        if let Some(v) = meta.get("valueLabels").and_then(|v| v.as_object()) {
            let map = v
                .iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect();
            entry.value_labels = Some(map);
        }
        self.cached_variables = None;
    }

    /// `case_num` is the 1-based case number shown in the grid's `__row__`.
    pub fn update_cell(&mut self, case_num: usize, col: &str, value: &JsonValue) -> EResult<()> {
        let Some(df) = &mut self.df else {
            return Err("No dataset loaded".into());
        };
        let row = case_num.checked_sub(1).ok_or("Invalid row number")?;
        let series = df.column(col).map_err(map_err)?.as_materialized_series();
        if row >= series.len() {
            return Err("Row out of range".into());
        }

        // Rebuild the column with the single cell changed, preserving dtype where
        // sensible. Editing is user-paced (one cell), so O(n) is acceptable.
        let new_series: Series = match series.dtype() {
            DataType::Int64 | DataType::Int32 | DataType::UInt32 | DataType::UInt64 => {
                let mut v: Vec<Option<i64>> = series
                    .cast(&DataType::Int64)
                    .map_err(map_err)?
                    .i64()
                    .map_err(map_err)?
                    .into_iter()
                    .collect();
                v[row] = value.as_i64().or_else(|| value.as_str().and_then(|s| s.parse().ok()));
                Series::new(col.into(), v)
            }
            DataType::Float64 | DataType::Float32 => {
                let mut v: Vec<Option<f64>> = series
                    .cast(&DataType::Float64)
                    .map_err(map_err)?
                    .f64()
                    .map_err(map_err)?
                    .into_iter()
                    .collect();
                v[row] = value.as_f64().or_else(|| value.as_str().and_then(|s| s.parse().ok()));
                Series::new(col.into(), v)
            }
            DataType::Boolean => {
                let mut v: Vec<Option<bool>> =
                    series.bool().map_err(map_err)?.into_iter().collect();
                v[row] = value.as_bool();
                Series::new(col.into(), v)
            }
            _ => {
                let mut v: Vec<Option<String>> = series
                    .cast(&DataType::String)
                    .map_err(map_err)?
                    .str()
                    .map_err(map_err)?
                    .into_iter()
                    .map(|o| o.map(|s| s.to_string()))
                    .collect();
                v[row] = Some(match value {
                    JsonValue::String(s) => s.clone(),
                    JsonValue::Null => String::new(),
                    other => other.to_string(),
                });
                Series::new(col.into(), v)
            }
        };

        df.replace(col, new_series).map_err(map_err)?;
        Ok(())
    }

    pub fn save_file(&self, path: &str) -> EResult<()> {
        let Some(df) = &self.df else {
            return Err("No dataset loaded".into());
        };
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let sep = match ext.as_str() {
            "tab" | "tsv" => b'\t',
            "csv" => b',',
            other => {
                return Err(format!(
                    "Saving .{other} is not yet supported by the native engine (use .tab/.tsv/.csv)."
                ))
            }
        };
        let mut file = std::fs::File::create(path).map_err(map_err)?;
        let mut out = df.clone();
        CsvWriter::new(&mut file)
            .include_header(true)
            .with_separator(sep)
            .finish(&mut out)
            .map_err(map_err)?;
        Ok(())
    }
}

// ── File readers ────────────────────────────────────────────────────────────

fn read_delimited(path: &str, separator: u8) -> EResult<DataFrame> {
    CsvReadOptions::default()
        .with_has_header(true)
        .with_ignore_errors(true)
        .with_parse_options(CsvParseOptions::default().with_separator(separator))
        .try_into_reader_with_file_path(Some(path.into()))
        .map_err(map_err)?
        .finish()
        .map_err(map_err)
}
