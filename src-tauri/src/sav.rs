//! SPSS `.sav` / `.zsav` reader.
//!
//! Uses the pure-Rust `ambers` crate (which returns an Arrow `RecordBatch` plus
//! rich SPSS metadata) and converts the result into a Polars `DataFrame` and the
//! engine's `VarMeta` map — restoring the variable labels, value labels, measure
//! level and alignment that the old pyreadstat path provided.

use std::collections::HashMap;

use ambers::{read_sav, Alignment, Measure, MissingSpec, Value};
use arrow::array::{Array, Float64Array, StringArray};
use arrow::compute::cast;
use arrow::datatypes::DataType as ArrowType;
use polars::prelude::*;
use serde_json::{json, Value as JsonValue};

use crate::engine::VarMeta;

pub struct SavData {
    pub df: DataFrame,
    pub var_meta: HashMap<String, VarMeta>,
}

pub fn read_sav_file(path: &str) -> Result<SavData, String> {
    let (batch, meta) = read_sav(path).map_err(|e| e.to_string())?;

    // ── Columns: Float64 stays numeric; everything else is cast to UTF-8 ──────
    let schema = batch.schema();
    let mut columns: Vec<Column> = Vec::with_capacity(batch.num_columns());
    for (i, field) in schema.fields().iter().enumerate() {
        let name = field.name().as_str();
        let array = batch.column(i);
        let series = if matches!(array.data_type(), ArrowType::Float64) {
            let a = array
                .as_any()
                .downcast_ref::<Float64Array>()
                .ok_or("expected Float64 array")?;
            let v: Vec<Option<f64>> = (0..a.len())
                .map(|j| if a.is_null(j) { None } else { Some(a.value(j)) })
                .collect();
            Series::new(name.into(), v)
        } else {
            // Date32 / Timestamp / Duration / Utf8View → readable string column.
            let utf8 = cast(array, &ArrowType::Utf8).map_err(|e| e.to_string())?;
            let a = utf8
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("cast to Utf8 did not produce a StringArray")?;
            let v: Vec<Option<String>> = (0..a.len())
                .map(|j| if a.is_null(j) { None } else { Some(a.value(j).to_string()) })
                .collect();
            Series::new(name.into(), v)
        };
        columns.push(series.into_column());
    }

    let df = if columns.is_empty() {
        DataFrame::empty()
    } else {
        DataFrame::new(columns).map_err(|e| e.to_string())?
    };

    // ── Metadata → VarMeta ────────────────────────────────────────────────────
    let mut var_meta = HashMap::with_capacity(meta.variable_names.len());
    for name in &meta.variable_names {
        let mut vm = VarMeta::default();

        if let Some(label) = meta.variable_labels.get(name) {
            if !label.is_empty() {
                vm.label = Some(label.clone());
            }
        }
        if let Some(labels) = meta.variable_value_labels.get(name) {
            let map: HashMap<String, String> = labels
                .iter()
                .map(|(k, v)| (value_key(k), v.clone()))
                .collect();
            if !map.is_empty() {
                vm.value_labels = Some(map);
            }
        }
        if let Some(m) = meta.variable_measures.get(name) {
            if let Some(s) = measure_str(m) {
                vm.measure_level = Some(s.into());
            }
        }
        if let Some(a) = meta.variable_alignments.get(name) {
            if let Some(s) = align_str(a) {
                vm.align = Some(s.into());
            }
        }
        if let Some(w) = meta.variable_display_widths.get(name) {
            vm.columns = Some(*w);
        }
        if let Some(specs) = meta.variable_missing_values.get(name) {
            vm.missing_values = Some(specs.iter().map(missing_to_json).collect());
        }

        var_meta.insert(name.clone(), vm);
    }

    Ok(SavData { df, var_meta })
}

/// Value-label keys must match the grid's `String(cellValue)` lookup, so an
/// integral numeric code like 1.0 becomes "1", not "1.0".
fn value_key(v: &Value) -> String {
    match v {
        Value::Numeric(n) if n.fract() == 0.0 => format!("{}", *n as i64),
        Value::Numeric(n) => n.to_string(),
        Value::String(s) => s.clone(),
    }
}

/// Returns None for Unknown so the engine falls back to inference.
fn measure_str(m: &Measure) -> Option<&'static str> {
    match m {
        Measure::Nominal => Some("nominal"),
        Measure::Ordinal => Some("ordinal"),
        Measure::Scale => Some("scale"),
        Measure::Unknown => None,
    }
}

fn align_str(a: &Alignment) -> Option<&'static str> {
    match a {
        Alignment::Left => Some("left"),
        Alignment::Right => Some("right"),
        Alignment::Center => Some("center"),
        Alignment::Unknown => None,
    }
}

fn missing_to_json(spec: &MissingSpec) -> JsonValue {
    match spec {
        MissingSpec::Value(v) => json!(v),
        MissingSpec::StringValue(s) => json!(s),
        MissingSpec::Range { lo, hi } => json!({ "lo": lo, "hi": hi }),
    }
}
