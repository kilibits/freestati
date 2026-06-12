//! Statistical procedures for FreeStati.
//!
//! Implements the first batch of the roadmap's analysis features on top of the
//! in-memory Polars `DataFrame`:
//!   * Descriptive statistics  — Descriptives, Frequencies
//!   * Compare means           — one-sample / independent / paired t-tests, one-way ANOVA
//!   * Correlation & regression — Pearson/Spearman correlation, OLS linear regression
//!   * Nonparametric tests     — Mann-Whitney U, Wilcoxon signed-rank, Kruskal-Wallis,
//!                               chi-square goodness of fit
//!
//! Every procedure returns a generic [`Analysis`] (a titled list of [`OutTable`]s)
//! so the renderer's Output viewer can display any result without procedure-specific
//! knowledge. `run_analysis` dispatches on a procedure name + JSON params object,
//! keeping the Tauri surface to a single command.
//!
//! p-values come from special functions (regularized incomplete beta/gamma, erf)
//! implemented here rather than a heavy stats dependency, keeping the binary small.

use std::collections::BTreeMap;

use polars::prelude::*;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::engine::Engine;

// ── Output model ─────────────────────────────────────────────────────────────

/// One rendered table: a title, column headers, and rows of heterogeneous cells
/// (strings for labels, numbers for statistics, null for "not applicable").
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutTable {
    pub title: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<JsonValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footnote: Option<String>,
}

impl OutTable {
    fn new(title: impl Into<String>, columns: Vec<&str>) -> Self {
        OutTable {
            title: title.into(),
            columns: columns.into_iter().map(String::from).collect(),
            rows: Vec::new(),
            footnote: None,
        }
    }
    fn footnote(mut self, note: impl Into<String>) -> Self {
        self.footnote = Some(note.into());
        self
    }
}

/// A complete procedure result: a heading and one or more tables.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub title: String,
    pub tables: Vec<OutTable>,
}

type SResult<T> = Result<T, String>;

// ── Cell helpers ─────────────────────────────────────────────────────────────

/// Numeric cell; non-finite values become null so the JSON stays valid.
fn num(x: f64) -> JsonValue {
    serde_json::Number::from_f64(x)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}
fn int(n: i64) -> JsonValue {
    JsonValue::Number(n.into())
}
fn text(s: impl Into<String>) -> JsonValue {
    JsonValue::String(s.into())
}

// ── Param helpers ────────────────────────────────────────────────────────────

fn p_str(params: &JsonValue, key: &str) -> SResult<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| format!("Missing parameter '{key}'"))
}

fn p_strs(params: &JsonValue, key: &str) -> SResult<Vec<String>> {
    let arr = params
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("Missing parameter '{key}'"))?;
    let out: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    if out.is_empty() {
        return Err(format!("Select at least one variable for '{key}'"));
    }
    Ok(out)
}

fn p_f64(params: &JsonValue, key: &str) -> SResult<f64> {
    params
        .get(key)
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .ok_or_else(|| format!("Missing or invalid number '{key}'"))
}

fn p_str_opt(params: &JsonValue, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(String::from)
}

// ── Data extraction ──────────────────────────────────────────────────────────

/// Column values aligned by row, `None` for null/non-finite. Length == n rows.
fn col_opt(df: &DataFrame, name: &str) -> SResult<Vec<Option<f64>>> {
    let s = df
        .column(name)
        .map_err(|_| format!("No such variable: {name}"))?
        .as_materialized_series()
        .cast(&DataType::Float64)
        .map_err(|_| format!("'{name}' is not numeric"))?;
    let ca = s.f64().map_err(|e| e.to_string())?;
    Ok(ca
        .into_iter()
        .map(|o| o.filter(|v| v.is_finite()))
        .collect())
}

/// A single numeric column with nulls dropped (valid cases only).
fn col_valid(df: &DataFrame, name: &str) -> SResult<Vec<f64>> {
    Ok(col_opt(df, name)?.into_iter().flatten().collect())
}

/// Canonical string label per row for a grouping/categorical column. Integral
/// numerics render without a trailing ".0" so user-typed group values match.
fn col_labels(df: &DataFrame, name: &str) -> SResult<Vec<Option<String>>> {
    let series = df
        .column(name)
        .map_err(|_| format!("No such variable: {name}"))?
        .as_materialized_series();
    if series.dtype().is_numeric() {
        let ca = series.cast(&DataType::Float64).map_err(|e| e.to_string())?;
        let f = ca.f64().map_err(|e| e.to_string())?;
        Ok(f.into_iter()
            .map(|o| {
                o.filter(|v| v.is_finite()).map(|v| {
                    if v.fract() == 0.0 {
                        format!("{}", v as i64)
                    } else {
                        format!("{v}")
                    }
                })
            })
            .collect())
    } else {
        let ca = series.cast(&DataType::String).map_err(|e| e.to_string())?;
        let s = ca.str().map_err(|e| e.to_string())?;
        Ok(s.into_iter().map(|o| o.map(String::from)).collect())
    }
}

// ── Descriptive helpers ──────────────────────────────────────────────────────

struct Desc {
    n: usize,
    mean: f64,
    sd: f64,
    variance: f64,
    sem: f64,
    min: f64,
    max: f64,
}

fn describe(x: &[f64]) -> Desc {
    let n = x.len();
    if n == 0 {
        return Desc {
            n: 0,
            mean: f64::NAN,
            sd: f64::NAN,
            variance: f64::NAN,
            sem: f64::NAN,
            min: f64::NAN,
            max: f64::NAN,
        };
    }
    let nf = n as f64;
    let mean = x.iter().sum::<f64>() / nf;
    let mut min = x[0];
    let mut max = x[0];
    let mut m2 = 0.0;
    for &v in x {
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
        let d = v - mean;
        m2 += d * d;
    }
    let variance = if n > 1 { m2 / (nf - 1.0) } else { f64::NAN };
    let sd = variance.sqrt();
    let sem = if n > 1 { sd / nf.sqrt() } else { f64::NAN };

    Desc {
        n,
        mean,
        sd,
        variance,
        sem,
        min,
        max,
    }
}

/// Average ranks with ties resolved by the midrank method.
fn ranks(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| x[a].partial_cmp(&x[b]).unwrap_or(std::cmp::Ordering::Equal));
    let mut r = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && x[idx[j + 1]] == x[idx[i]] {
            j += 1;
        }
        let avg = ((i + 1) + (j + 1)) as f64 / 2.0;
        for k in i..=j {
            r[idx[k]] = avg;
        }
        i = j + 1;
    }
    r
}

/// Σ(t³ − t) over tie groups — the correction term for rank-based variances.
fn tie_correction(x: &[f64]) -> f64 {
    let mut sorted = x.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut c = 0.0;
    let mut i = 0;
    let n = sorted.len();
    while i < n {
        let mut j = i;
        while j + 1 < n && sorted[j + 1] == sorted[i] {
            j += 1;
        }
        let t = (j - i + 1) as f64;
        c += t * t * t - t;
        i = j + 1;
    }
    c
}

// ── Special functions (p-values without external deps) ───────────────────────

/// Regularized lower incomplete gamma P(a, x).
fn gammp(a: f64, x: f64) -> f64 {
    if x < 0.0 || a <= 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return 0.0;
    }
    if x < a + 1.0 {
        // Series representation.
        let mut ap = a;
        let mut sum = 1.0 / a;
        let mut del = sum;
        for _ in 0..200 {
            ap += 1.0;
            del *= x / ap;
            sum += del;
            if del.abs() < sum.abs() * 1e-15 {
                break;
            }
        }
        sum * (-x + a * x.ln() - ln_gamma(a)).exp()
    } else {
        1.0 - gammq_cf(a, x)
    }
}

/// Regularized upper incomplete gamma Q(a, x) = 1 − P(a, x).
fn gammq(a: f64, x: f64) -> f64 {
    if x < 0.0 || a <= 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return 1.0;
    }
    if x < a + 1.0 {
        1.0 - gammp(a, x)
    } else {
        gammq_cf(a, x)
    }
}

/// Continued-fraction evaluation of Q(a, x), valid for x ≥ a + 1.
fn gammq_cf(a: f64, x: f64) -> f64 {
    let tiny = 1e-300;
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / tiny;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..200 {
        let an = -(i as f64) * (i as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < tiny {
            d = tiny;
        }
        c = b + an / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-15 {
            break;
        }
    }
    (-x + a * x.ln() - ln_gamma(a)).exp() * h
}

/// Lanczos approximation of ln Γ(x).
fn ln_gamma(x: f64) -> f64 {
    const COF: [f64; 6] = [
        76.18009172947146,
        -86.50532032941677,
        24.01409824083091,
        -1.231739572450155,
        0.1208650973866179e-2,
        -0.5395239384953e-5,
    ];
    let mut y = x;
    let tmp = (x + 5.5) - (x + 0.5) * (x + 5.5).ln();
    let mut ser = 1.000000000190015;
    for c in COF.iter() {
        y += 1.0;
        ser += c / y;
    }
    -tmp + (2.5066282746310005 * ser / x).ln()
}

/// Regularized incomplete beta I_x(a, b).
fn betai(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let bt = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b)
        + a * x.ln()
        + b * (1.0 - x).ln())
    .exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * betacf(a, b, x) / a
    } else {
        1.0 - bt * betacf(b, a, 1.0 - x) / b
    }
}

fn betacf(a: f64, b: f64, x: f64) -> f64 {
    let tiny = 1e-300;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < tiny {
        d = tiny;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..200 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < tiny {
            d = tiny;
        }
        c = 1.0 + aa / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < tiny {
            d = tiny;
        }
        c = 1.0 + aa / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-15 {
            break;
        }
    }
    h
}

/// Two-tailed p-value for Student's t with `df` degrees of freedom.
fn t_sig_2tailed(t: f64, df: f64) -> f64 {
    if df <= 0.0 || !t.is_finite() {
        return f64::NAN;
    }
    betai(df / 2.0, 0.5, df / (df + t * t))
}

/// Upper-tail p-value for F(df1, df2).
fn f_sig(f: f64, df1: f64, df2: f64) -> f64 {
    if f <= 0.0 || df1 <= 0.0 || df2 <= 0.0 {
        return f64::NAN;
    }
    betai(df2 / 2.0, df1 / 2.0, df2 / (df2 + df1 * f))
}

/// Upper-tail p-value for chi-square with `df` degrees of freedom.
fn chi2_sig(x: f64, df: f64) -> f64 {
    if x <= 0.0 || df <= 0.0 {
        return 1.0;
    }
    gammq(df / 2.0, x / 2.0)
}

/// Standard normal CDF Φ(z).
fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))
}

fn erf(x: f64) -> f64 {
    if x >= 0.0 {
        gammp(0.5, x * x)
    } else {
        -gammp(0.5, x * x)
    }
}

/// Two-tailed normal p-value for a z statistic.
fn z_sig_2tailed(z: f64) -> f64 {
    2.0 * (1.0 - normal_cdf(z.abs()))
}

// ── Procedure dispatch ───────────────────────────────────────────────────────

impl Engine {
    /// Run a named procedure with a JSON params object. The single entry point
    /// behind the `run_analysis` Tauri command.
    pub fn run_analysis(&self, procedure: &str, params: &JsonValue) -> SResult<Analysis> {
        let df = self.df.as_ref().ok_or("No dataset loaded")?;
        match procedure {
            "descriptives" => self.descriptives(df, &p_strs(params, "vars")?),
            "frequencies" => self.frequencies(df, &p_strs(params, "vars")?),
            "ttest_one_sample" => {
                self.ttest_one_sample(df, &p_strs(params, "vars")?, p_f64(params, "testValue")?)
            }
            "ttest_independent" => self.ttest_independent(
                df,
                &p_strs(params, "vars")?,
                &p_str(params, "group")?,
                &p_str(params, "group1")?,
                &p_str(params, "group2")?,
            ),
            "ttest_paired" => self.ttest_paired(df, &pairs(params)?),
            "anova_oneway" => {
                self.anova_oneway(df, &p_strs(params, "vars")?, &p_str(params, "factor")?)
            }
            "correlate" => self.correlate(
                df,
                &p_strs(params, "vars")?,
                &p_str_opt(params, "method").unwrap_or_else(|| "pearson".into()),
            ),
            "regression_linear" => self.regression_linear(
                df,
                &p_str(params, "dependent")?,
                &p_strs(params, "independents")?,
            ),
            "mann_whitney" => self.mann_whitney(
                df,
                &p_strs(params, "vars")?,
                &p_str(params, "group")?,
                &p_str(params, "group1")?,
                &p_str(params, "group2")?,
            ),
            "wilcoxon" => self.wilcoxon(df, &pairs(params)?),
            "kruskal_wallis" => {
                self.kruskal_wallis(df, &p_strs(params, "vars")?, &p_str(params, "factor")?)
            }
            "chi_square" => self.chi_square(df, &p_strs(params, "vars")?),
            other => Err(format!("Unknown procedure: {other}")),
        }
    }

    /// A column's display label (variable label if set, else its name).
    fn display(&self, name: &str) -> String {
        self.var_meta
            .get(name)
            .and_then(|m| m.label.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.to_string())
    }

    // ── Descriptive statistics ───────────────────────────────────────────────

    fn descriptives(&self, df: &DataFrame, vars: &[String]) -> SResult<Analysis> {
        let mut t = OutTable::new(
            "Descriptive Statistics",
            vec![
                "",
                "N",
                "Minimum",
                "Maximum",
                "Mean",
                "Std. Deviation",
                "Variance",
            ],
        );
        // Listwise N: rows where every selected variable is present.
        let cols: Vec<Vec<Option<f64>>> =
            vars.iter().map(|v| col_opt(df, v)).collect::<SResult<_>>()?;
        let nrows = df.height();
        let listwise = (0..nrows)
            .filter(|&i| cols.iter().all(|c| c[i].is_some()))
            .count();

        for v in vars {
            let x = col_valid(df, v)?;
            let d = describe(&x);
            t.rows.push(vec![
                text(self.display(v)),
                int(d.n as i64),
                num(d.min),
                num(d.max),
                num(d.mean),
                num(d.sd),
                num(d.variance),
            ]);
        }
        t.rows.push(vec![
            text("Valid N (listwise)"),
            int(listwise as i64),
            JsonValue::Null,
            JsonValue::Null,
            JsonValue::Null,
            JsonValue::Null,
            JsonValue::Null,
        ]);
        Ok(Analysis {
            title: "Descriptives".into(),
            tables: vec![t],
        })
    }

    fn frequencies(&self, df: &DataFrame, vars: &[String]) -> SResult<Analysis> {
        let mut tables = Vec::new();
        for v in vars {
            let labels = col_labels(df, v)?;
            let value_labels = self.var_meta.get(v).and_then(|m| m.value_labels.clone());
            let total = labels.len();
            let valid: Vec<&String> = labels.iter().flatten().collect();
            let valid_n = valid.len();
            let missing = total - valid_n;

            // Tally counts, ordered: numeric values numerically, else lexically.
            let mut counts: BTreeMap<OrderKey, usize> = BTreeMap::new();
            for s in &valid {
                *counts.entry(OrderKey::from(s.as_str())).or_insert(0) += 1;
            }

            let mut t = OutTable::new(
                self.display(v),
                vec![
                    "Value",
                    "Frequency",
                    "Percent",
                    "Valid Percent",
                    "Cumulative Percent",
                ],
            );
            let mut cum = 0.0;
            for (key, &freq) in &counts {
                let raw = key.label();
                let shown = value_labels
                    .as_ref()
                    .and_then(|m| m.get(&raw).cloned())
                    .unwrap_or(raw);
                let pct = 100.0 * freq as f64 / total as f64;
                let valid_pct = 100.0 * freq as f64 / valid_n as f64;
                cum += valid_pct;
                t.rows.push(vec![
                    text(shown),
                    int(freq as i64),
                    num(pct),
                    num(valid_pct),
                    num(cum),
                ]);
            }
            if missing > 0 {
                t.rows.push(vec![
                    text("Missing"),
                    int(missing as i64),
                    num(100.0 * missing as f64 / total as f64),
                    JsonValue::Null,
                    JsonValue::Null,
                ]);
            }
            t.rows.push(vec![
                text("Total"),
                int(total as i64),
                num(100.0),
                JsonValue::Null,
                JsonValue::Null,
            ]);
            tables.push(t);
        }
        Ok(Analysis {
            title: "Frequencies".into(),
            tables,
        })
    }

    // ── Compare means ─────────────────────────────────────────────────────────

    fn ttest_one_sample(
        &self,
        df: &DataFrame,
        vars: &[String],
        test_value: f64,
    ) -> SResult<Analysis> {
        let mut stats = OutTable::new(
            "One-Sample Statistics",
            vec!["", "N", "Mean", "Std. Deviation", "Std. Error Mean"],
        );
        let mut test = OutTable::new(
            "One-Sample Test",
            vec![
                "",
                "t",
                "df",
                "Sig. (2-tailed)",
                "Mean Difference",
            ],
        )
        .footnote(format!("Test Value = {test_value}"));

        for v in vars {
            let x = col_valid(df, v)?;
            let d = describe(&x);
            stats.rows.push(vec![
                text(self.display(v)),
                int(d.n as i64),
                num(d.mean),
                num(d.sd),
                num(d.sem),
            ]);
            let df_t = d.n as f64 - 1.0;
            let t = (d.mean - test_value) / d.sem;
            test.rows.push(vec![
                text(self.display(v)),
                num(t),
                num(df_t),
                num(t_sig_2tailed(t, df_t)),
                num(d.mean - test_value),
            ]);
        }
        Ok(Analysis {
            title: "One-Sample T Test".into(),
            tables: vec![stats, test],
        })
    }

    fn ttest_independent(
        &self,
        df: &DataFrame,
        vars: &[String],
        group: &str,
        g1: &str,
        g2: &str,
    ) -> SResult<Analysis> {
        let labels = col_labels(df, group)?;
        let mut stats = OutTable::new(
            "Group Statistics",
            vec!["", group, "N", "Mean", "Std. Deviation", "Std. Error Mean"],
        );
        let mut test = OutTable::new(
            "Independent Samples Test",
            vec![
                "",
                "",
                "t",
                "df",
                "Sig. (2-tailed)",
                "Mean Difference",
                "Std. Error Difference",
            ],
        );

        for v in vars {
            let xs = col_opt(df, v)?;
            let mut a = Vec::new();
            let mut b = Vec::new();
            for (i, lab) in labels.iter().enumerate() {
                let Some(val) = xs[i] else { continue };
                match lab.as_deref() {
                    Some(l) if l == g1 => a.push(val),
                    Some(l) if l == g2 => b.push(val),
                    _ => {}
                }
            }
            if a.len() < 2 || b.len() < 2 {
                return Err(format!(
                    "Need at least 2 valid cases per group for '{v}' (group {g1}: {}, group {g2}: {})",
                    a.len(),
                    b.len()
                ));
            }
            let da = describe(&a);
            let db = describe(&b);
            stats.rows.push(vec![
                text(self.display(v)),
                text(g1),
                int(da.n as i64),
                num(da.mean),
                num(da.sd),
                num(da.sem),
            ]);
            stats.rows.push(vec![
                text(""),
                text(g2),
                int(db.n as i64),
                num(db.mean),
                num(db.sd),
                num(db.sem),
            ]);

            let (n1, n2) = (da.n as f64, db.n as f64);
            let mean_diff = da.mean - db.mean;

            // Equal variances assumed (pooled).
            let sp2 = ((n1 - 1.0) * da.variance + (n2 - 1.0) * db.variance) / (n1 + n2 - 2.0);
            let se_pool = (sp2 * (1.0 / n1 + 1.0 / n2)).sqrt();
            let t_pool = mean_diff / se_pool;
            let df_pool = n1 + n2 - 2.0;

            // Equal variances not assumed (Welch–Satterthwaite).
            let se_welch = (da.variance / n1 + db.variance / n2).sqrt();
            let t_welch = mean_diff / se_welch;
            let df_welch = (da.variance / n1 + db.variance / n2).powi(2)
                / ((da.variance / n1).powi(2) / (n1 - 1.0)
                    + (db.variance / n2).powi(2) / (n2 - 1.0));

            test.rows.push(vec![
                text(self.display(v)),
                text("Equal variances assumed"),
                num(t_pool),
                num(df_pool),
                num(t_sig_2tailed(t_pool, df_pool)),
                num(mean_diff),
                num(se_pool),
            ]);
            test.rows.push(vec![
                text(""),
                text("Equal variances not assumed"),
                num(t_welch),
                num(df_welch),
                num(t_sig_2tailed(t_welch, df_welch)),
                num(mean_diff),
                num(se_welch),
            ]);
        }
        Ok(Analysis {
            title: "Independent-Samples T Test".into(),
            tables: vec![stats, test],
        })
    }

    fn ttest_paired(&self, df: &DataFrame, pairs: &[(String, String)]) -> SResult<Analysis> {
        let mut stats = OutTable::new(
            "Paired Samples Statistics",
            vec!["", "Mean", "N", "Std. Deviation", "Std. Error Mean"],
        );
        let mut test = OutTable::new(
            "Paired Samples Test",
            vec![
                "",
                "Mean",
                "Std. Deviation",
                "Std. Error Mean",
                "t",
                "df",
                "Sig. (2-tailed)",
            ],
        );
        for (i, (a, b)) in pairs.iter().enumerate() {
            let xa = col_opt(df, a)?;
            let xb = col_opt(df, b)?;
            let mut da = Vec::new();
            let mut db = Vec::new();
            let mut diff = Vec::new();
            for k in 0..xa.len() {
                if let (Some(va), Some(vb)) = (xa[k], xb[k]) {
                    da.push(va);
                    db.push(vb);
                    diff.push(va - vb);
                }
            }
            if diff.len() < 2 {
                return Err(format!("Pair {} has fewer than 2 valid cases", i + 1));
            }
            let sa = describe(&da);
            let sb = describe(&db);
            let sd = describe(&diff);
            stats.rows.push(vec![
                text(self.display(a)),
                num(sa.mean),
                int(sa.n as i64),
                num(sa.sd),
                num(sa.sem),
            ]);
            stats.rows.push(vec![
                text(self.display(b)),
                num(sb.mean),
                int(sb.n as i64),
                num(sb.sd),
                num(sb.sem),
            ]);
            let df_t = sd.n as f64 - 1.0;
            let t = sd.mean / sd.sem;
            test.rows.push(vec![
                text(format!("{} - {}", self.display(a), self.display(b))),
                num(sd.mean),
                num(sd.sd),
                num(sd.sem),
                num(t),
                num(df_t),
                num(t_sig_2tailed(t, df_t)),
            ]);
        }
        Ok(Analysis {
            title: "Paired-Samples T Test".into(),
            tables: vec![stats, test],
        })
    }

    fn anova_oneway(&self, df: &DataFrame, vars: &[String], factor: &str) -> SResult<Analysis> {
        let labels = col_labels(df, factor)?;
        let mut tables = Vec::new();
        for v in vars {
            let xs = col_opt(df, v)?;
            // Group values by factor level.
            let mut groups: BTreeMap<String, Vec<f64>> = BTreeMap::new();
            for (i, lab) in labels.iter().enumerate() {
                if let (Some(l), Some(val)) = (lab, xs[i]) {
                    groups.entry(l.clone()).or_default().push(val);
                }
            }
            let groups: Vec<Vec<f64>> = groups
                .into_values()
                .filter(|g| !g.is_empty())
                .collect();
            if groups.len() < 2 {
                return Err(format!("'{factor}' must have at least 2 groups for '{v}'"));
            }
            let grand_n: usize = groups.iter().map(|g| g.len()).sum();
            let grand_sum: f64 = groups.iter().flat_map(|g| g.iter()).sum();
            let grand_mean = grand_sum / grand_n as f64;

            let mut ss_between = 0.0;
            let mut ss_within = 0.0;
            for g in &groups {
                let gn = g.len() as f64;
                let gmean = g.iter().sum::<f64>() / gn;
                ss_between += gn * (gmean - grand_mean).powi(2);
                for &val in g {
                    ss_within += (val - gmean).powi(2);
                }
            }
            let df_between = groups.len() as f64 - 1.0;
            let df_within = grand_n as f64 - groups.len() as f64;
            let ms_between = ss_between / df_between;
            let ms_within = ss_within / df_within;
            let f = ms_between / ms_within;
            let sig = f_sig(f, df_between, df_within);
            let ss_total = ss_between + ss_within;

            let mut t = OutTable::new(
                format!("ANOVA — {}", self.display(v)),
                vec!["", "Sum of Squares", "df", "Mean Square", "F", "Sig."],
            );
            t.rows.push(vec![
                text("Between Groups"),
                num(ss_between),
                num(df_between),
                num(ms_between),
                num(f),
                num(sig),
            ]);
            t.rows.push(vec![
                text("Within Groups"),
                num(ss_within),
                num(df_within),
                num(ms_within),
                JsonValue::Null,
                JsonValue::Null,
            ]);
            t.rows.push(vec![
                text("Total"),
                num(ss_total),
                num(df_between + df_within),
                JsonValue::Null,
                JsonValue::Null,
                JsonValue::Null,
            ]);
            tables.push(t);
        }
        Ok(Analysis {
            title: "One-Way ANOVA".into(),
            tables,
        })
    }

    // ── Correlation & regression ─────────────────────────────────────────────

    fn correlate(&self, df: &DataFrame, vars: &[String], method: &str) -> SResult<Analysis> {
        let spearman = method.eq_ignore_ascii_case("spearman");
        let cols: Vec<Vec<Option<f64>>> =
            vars.iter().map(|v| col_opt(df, v)).collect::<SResult<_>>()?;
        let nrows = df.height();

        let label = if spearman {
            "Spearman Correlation"
        } else {
            "Pearson Correlation"
        };
        let mut headers = vec![""];
        headers.extend(vars.iter().map(|s| s.as_str()));
        let mut t = OutTable::new(
            if spearman {
                "Correlations (Spearman's rho)"
            } else {
                "Correlations (Pearson)"
            },
            headers,
        );

        for (i, vi) in vars.iter().enumerate() {
            // Each variable spans three rows: the coefficient, its 2-tailed
            // significance, and the (pairwise) N — mirroring SPSS's layout.
            let mut row_r: Vec<JsonValue> = vec![text(format!("{} — {}", self.display(vi), label))];
            let mut row_sig: Vec<JsonValue> = vec![text("  Sig. (2-tailed)")];
            let mut row_n: Vec<JsonValue> = vec![text("  N")];

            for j in 0..vars.len() {
                // Pairwise (listwise per pair) deletion.
                let mut a = Vec::new();
                let mut b = Vec::new();
                for k in 0..nrows {
                    if let (Some(va), Some(vb)) = (cols[i][k], cols[j][k]) {
                        a.push(va);
                        b.push(vb);
                    }
                }
                let n = a.len();
                let (xa, xb) = if spearman {
                    (ranks(&a), ranks(&b))
                } else {
                    (a, b)
                };
                let r = pearson(&xa, &xb);
                let sig = if i == j {
                    f64::NAN
                } else if n > 2 {
                    let tstat = r * ((n as f64 - 2.0) / (1.0 - r * r)).sqrt();
                    t_sig_2tailed(tstat, n as f64 - 2.0)
                } else {
                    f64::NAN
                };
                row_r.push(num(r));
                row_sig.push(if i == j { JsonValue::Null } else { num(sig) });
                row_n.push(int(n as i64));
            }
            t.rows.push(row_r);
            t.rows.push(row_sig);
            t.rows.push(row_n);
        }
        Ok(Analysis {
            title: "Correlations".into(),
            tables: vec![t],
        })
    }

    fn regression_linear(
        &self,
        df: &DataFrame,
        dependent: &str,
        independents: &[String],
    ) -> SResult<Analysis> {
        let y_all = col_opt(df, dependent)?;
        let x_all: Vec<Vec<Option<f64>>> = independents
            .iter()
            .map(|v| col_opt(df, v))
            .collect::<SResult<_>>()?;
        let nrows = df.height();
        let p = independents.len();

        // Listwise deletion across y and all predictors.
        let mut y = Vec::new();
        let mut x: Vec<Vec<f64>> = Vec::new(); // rows of [1, x1, x2, ...]
        for i in 0..nrows {
            let Some(yi) = y_all[i] else { continue };
            if x_all.iter().any(|c| c[i].is_none()) {
                continue;
            }
            y.push(yi);
            let mut row = vec![1.0];
            for c in &x_all {
                row.push(c[i].unwrap());
            }
            x.push(row);
        }
        let n = y.len();
        if n <= p + 1 {
            return Err("Not enough valid cases for the number of predictors".into());
        }
        let k = p + 1; // params including intercept

        // Normal equations: (XᵀX) β = Xᵀy.
        let mut xtx = vec![vec![0.0; k]; k];
        let mut xty = vec![0.0; k];
        for i in 0..n {
            for a in 0..k {
                xty[a] += x[i][a] * y[i];
                for b in 0..k {
                    xtx[a][b] += x[i][a] * x[i][b];
                }
            }
        }
        let inv = invert(xtx).ok_or("Predictors are collinear (singular matrix)")?;
        let beta: Vec<f64> = (0..k)
            .map(|a| (0..k).map(|b| inv[a][b] * xty[b]).sum())
            .collect();

        // Residuals and sums of squares.
        let ybar = y.iter().sum::<f64>() / n as f64;
        let mut ss_res = 0.0;
        let mut ss_tot = 0.0;
        for i in 0..n {
            let yhat: f64 = (0..k).map(|a| beta[a] * x[i][a]).sum();
            ss_res += (y[i] - yhat).powi(2);
            ss_tot += (y[i] - ybar).powi(2);
        }
        let ss_reg = ss_tot - ss_res;
        let df_reg = p as f64;
        let df_res = (n - k) as f64;
        let ms_reg = ss_reg / df_reg;
        let mse = ss_res / df_res;
        let f = ms_reg / mse;
        let r2 = if ss_tot > 0.0 { ss_reg / ss_tot } else { f64::NAN };
        let adj_r2 = 1.0 - (1.0 - r2) * (n as f64 - 1.0) / df_res;
        let r = r2.sqrt();
        let std_err_est = mse.sqrt();

        // SDs for standardized coefficients (betas).
        let y_sd = describe(&y).sd;
        let x_sd: Vec<f64> = (0..p)
            .map(|j| {
                let col: Vec<f64> = x.iter().map(|r| r[j + 1]).collect();
                describe(&col).sd
            })
            .collect();

        // Model summary.
        let mut summary = OutTable::new(
            "Model Summary",
            vec!["R", "R Square", "Adjusted R Square", "Std. Error of the Estimate"],
        );
        summary.rows.push(vec![
            num(r),
            num(r2),
            num(adj_r2),
            num(std_err_est),
        ]);

        // ANOVA.
        let mut anova = OutTable::new(
            "ANOVA",
            vec!["", "Sum of Squares", "df", "Mean Square", "F", "Sig."],
        );
        anova.rows.push(vec![
            text("Regression"),
            num(ss_reg),
            num(df_reg),
            num(ms_reg),
            num(f),
            num(f_sig(f, df_reg, df_res)),
        ]);
        anova.rows.push(vec![
            text("Residual"),
            num(ss_res),
            num(df_res),
            num(mse),
            JsonValue::Null,
            JsonValue::Null,
        ]);
        anova.rows.push(vec![
            text("Total"),
            num(ss_tot),
            num(df_reg + df_res),
            JsonValue::Null,
            JsonValue::Null,
            JsonValue::Null,
        ]);

        // Coefficients.
        let mut coef = OutTable::new(
            "Coefficients",
            vec!["", "B", "Std. Error", "Beta", "t", "Sig."],
        );
        for a in 0..k {
            let se = (mse * inv[a][a]).sqrt();
            let tstat = beta[a] / se;
            let sig = t_sig_2tailed(tstat, df_res);
            let (label, beta_std) = if a == 0 {
                ("(Constant)".to_string(), JsonValue::Null)
            } else {
                let std = if y_sd > 0.0 {
                    num(beta[a] * x_sd[a - 1] / y_sd)
                } else {
                    JsonValue::Null
                };
                (self.display(&independents[a - 1]), std)
            };
            coef.rows.push(vec![
                text(label),
                num(beta[a]),
                num(se),
                beta_std,
                num(tstat),
                num(sig),
            ]);
        }

        Ok(Analysis {
            title: format!("Linear Regression — {}", self.display(dependent)),
            tables: vec![summary, anova, coef],
        })
    }

    // ── Nonparametric tests ──────────────────────────────────────────────────

    fn mann_whitney(
        &self,
        df: &DataFrame,
        vars: &[String],
        group: &str,
        g1: &str,
        g2: &str,
    ) -> SResult<Analysis> {
        let labels = col_labels(df, group)?;
        let mut ranks_tbl = OutTable::new(
            "Ranks",
            vec!["", group, "N", "Mean Rank", "Sum of Ranks"],
        );
        let mut test = OutTable::new(
            "Test Statistics",
            vec!["", "Mann-Whitney U", "Wilcoxon W", "Z", "Asymp. Sig. (2-tailed)"],
        );
        for v in vars {
            let xs = col_opt(df, v)?;
            let mut combined = Vec::new();
            let mut which = Vec::new(); // true = group1
            for (i, lab) in labels.iter().enumerate() {
                let Some(val) = xs[i] else { continue };
                match lab.as_deref() {
                    Some(l) if l == g1 => {
                        combined.push(val);
                        which.push(true);
                    }
                    Some(l) if l == g2 => {
                        combined.push(val);
                        which.push(false);
                    }
                    _ => {}
                }
            }
            let n1 = which.iter().filter(|&&w| w).count();
            let n2 = which.len() - n1;
            if n1 == 0 || n2 == 0 {
                return Err(format!("Both groups must be non-empty for '{v}'"));
            }
            let rk = ranks(&combined);
            let r1: f64 = rk.iter().zip(&which).filter(|(_, &w)| w).map(|(r, _)| r).sum();
            let r2: f64 = rk.iter().zip(&which).filter(|(_, &w)| !w).map(|(r, _)| r).sum();
            let (n1f, n2f) = (n1 as f64, n2 as f64);
            let u1 = r1 - n1f * (n1f + 1.0) / 2.0;
            let u2 = r2 - n2f * (n2f + 1.0) / 2.0;
            let u = u1.min(u2);
            let w = r1.min(r2);
            let mu = n1f * n2f / 2.0;
            let nt = n1f + n2f;
            let tie = tie_correction(&combined);
            let sigma = ((n1f * n2f / 12.0)
                * ((nt + 1.0) - tie / (nt * (nt - 1.0))))
            .sqrt();
            let z = (u - mu) / sigma;
            ranks_tbl.rows.push(vec![
                text(self.display(v)),
                text(g1),
                int(n1 as i64),
                num(r1 / n1f),
                num(r1),
            ]);
            ranks_tbl.rows.push(vec![
                text(""),
                text(g2),
                int(n2 as i64),
                num(r2 / n2f),
                num(r2),
            ]);
            test.rows.push(vec![
                text(self.display(v)),
                num(u),
                num(w),
                num(z),
                num(z_sig_2tailed(z)),
            ]);
        }
        Ok(Analysis {
            title: "Mann-Whitney U Test".into(),
            tables: vec![ranks_tbl, test],
        })
    }

    fn wilcoxon(&self, df: &DataFrame, pairs: &[(String, String)]) -> SResult<Analysis> {
        let mut test = OutTable::new(
            "Test Statistics",
            vec!["", "N (non-zero)", "Z", "Asymp. Sig. (2-tailed)"],
        );
        for (idx, (a, b)) in pairs.iter().enumerate() {
            let xa = col_opt(df, a)?;
            let xb = col_opt(df, b)?;
            let mut diffs = Vec::new();
            for k in 0..xa.len() {
                if let (Some(va), Some(vb)) = (xa[k], xb[k]) {
                    let d = vb - va; // positive = increase
                    if d != 0.0 {
                        diffs.push(d);
                    }
                }
            }
            let n = diffs.len();
            if n < 1 {
                return Err(format!("Pair {} has no non-zero differences", idx + 1));
            }
            let abs: Vec<f64> = diffs.iter().map(|d| d.abs()).collect();
            let rk = ranks(&abs);
            let w_plus: f64 = rk
                .iter()
                .zip(&diffs)
                .filter(|(_, &d)| d > 0.0)
                .map(|(r, _)| r)
                .sum();
            let nf = n as f64;
            let mean_w = nf * (nf + 1.0) / 4.0;
            let tie = tie_correction(&abs);
            let var_w = nf * (nf + 1.0) * (2.0 * nf + 1.0) / 24.0 - tie / 48.0;
            let z = (w_plus - mean_w) / var_w.sqrt();
            test.rows.push(vec![
                text(format!("{} - {}", self.display(b), self.display(a))),
                int(n as i64),
                num(z),
                num(z_sig_2tailed(z)),
            ]);
        }
        Ok(Analysis {
            title: "Wilcoxon Signed-Rank Test".into(),
            tables: vec![test],
        })
    }

    fn kruskal_wallis(
        &self,
        df: &DataFrame,
        vars: &[String],
        factor: &str,
    ) -> SResult<Analysis> {
        let labels = col_labels(df, factor)?;
        let mut ranks_tbl =
            OutTable::new("Ranks", vec!["", factor, "N", "Mean Rank"]);
        let mut test = OutTable::new(
            "Test Statistics",
            vec!["", "Chi-Square", "df", "Asymp. Sig."],
        );
        for v in vars {
            let xs = col_opt(df, v)?;
            let mut combined = Vec::new();
            let mut grp = Vec::new();
            for (i, lab) in labels.iter().enumerate() {
                if let (Some(l), Some(val)) = (lab, xs[i]) {
                    combined.push(val);
                    grp.push(l.clone());
                }
            }
            // Distinct levels in sorted order.
            let mut levels: Vec<String> = grp.clone();
            levels.sort();
            levels.dedup();
            if levels.len() < 2 {
                return Err(format!("'{factor}' must have at least 2 groups for '{v}'"));
            }
            let rk = ranks(&combined);
            let nt = combined.len() as f64;
            let mut h = 0.0;
            for lev in &levels {
                let (mut sum, mut cnt) = (0.0, 0.0);
                for (i, g) in grp.iter().enumerate() {
                    if g == lev {
                        sum += rk[i];
                        cnt += 1.0;
                    }
                }
                let mean_rank = sum / cnt;
                h += cnt * (sum / cnt).powi(2);
                ranks_tbl.rows.push(vec![
                    if lev == &levels[0] {
                        text(self.display(v))
                    } else {
                        text("")
                    },
                    text(lev),
                    int(cnt as i64),
                    num(mean_rank),
                ]);
            }
            h = 12.0 / (nt * (nt + 1.0)) * h - 3.0 * (nt + 1.0);
            // Tie correction.
            let tie = tie_correction(&combined);
            let h = h / (1.0 - tie / (nt * nt * nt - nt));
            let dfree = levels.len() as f64 - 1.0;
            test.rows.push(vec![
                text(self.display(v)),
                num(h),
                num(dfree),
                num(chi2_sig(h, dfree)),
            ]);
        }
        Ok(Analysis {
            title: "Kruskal-Wallis Test".into(),
            tables: vec![ranks_tbl, test],
        })
    }

    fn chi_square(&self, df: &DataFrame, vars: &[String]) -> SResult<Analysis> {
        let mut tables = Vec::new();
        let mut test = OutTable::new(
            "Test Statistics",
            vec!["", "Chi-Square", "df", "Asymp. Sig."],
        );
        for v in vars {
            let labels = col_labels(df, v)?;
            let value_labels = self.var_meta.get(v).and_then(|m| m.value_labels.clone());
            let mut counts: BTreeMap<OrderKey, usize> = BTreeMap::new();
            for lab in labels.iter().flatten() {
                *counts.entry(OrderKey::from(lab.as_str())).or_insert(0) += 1;
            }
            let n: usize = counts.values().sum();
            let categories = counts.len();
            if categories < 2 {
                return Err(format!("'{v}' must have at least 2 categories"));
            }
            let expected = n as f64 / categories as f64;
            let mut chi = 0.0;
            let mut cat_tbl = OutTable::new(
                self.display(v),
                vec!["Value", "Observed N", "Expected N", "Residual"],
            );
            for (key, &obs) in &counts {
                let raw = key.label();
                let shown = value_labels
                    .as_ref()
                    .and_then(|m| m.get(&raw).cloned())
                    .unwrap_or(raw);
                chi += (obs as f64 - expected).powi(2) / expected;
                cat_tbl.rows.push(vec![
                    text(shown),
                    int(obs as i64),
                    num(expected),
                    num(obs as f64 - expected),
                ]);
            }
            tables.push(cat_tbl);
            let dfree = categories as f64 - 1.0;
            test.rows.push(vec![
                text(self.display(v)),
                num(chi),
                num(dfree),
                num(chi2_sig(chi, dfree)),
            ]);
        }
        tables.push(test);
        Ok(Analysis {
            title: "Chi-Square Test".into(),
            tables,
        })
    }
}

// ── Shared numeric helpers ───────────────────────────────────────────────────

/// Pearson correlation of two equal-length slices.
fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len();
    if n < 2 {
        return f64::NAN;
    }
    let nf = n as f64;
    let ma = a.iter().sum::<f64>() / nf;
    let mb = b.iter().sum::<f64>() / nf;
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let da = a[i] - ma;
        let db = b[i] - mb;
        cov += da * db;
        va += da * da;
        vb += db * db;
    }
    if va == 0.0 || vb == 0.0 {
        return f64::NAN;
    }
    cov / (va.sqrt() * vb.sqrt())
}

/// In-place Gauss-Jordan inversion; returns None if singular.
fn invert(mut a: Vec<Vec<f64>>) -> Option<Vec<Vec<f64>>> {
    let n = a.len();
    let mut inv = (0..n)
        .map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    for col in 0..n {
        // Partial pivot.
        let mut pivot = col;
        for r in (col + 1)..n {
            if a[r][col].abs() > a[pivot][col].abs() {
                pivot = r;
            }
        }
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        inv.swap(col, pivot);
        let d = a[col][col];
        for j in 0..n {
            a[col][j] /= d;
            inv[col][j] /= d;
        }
        for r in 0..n {
            if r == col {
                continue;
            }
            let factor = a[r][col];
            for j in 0..n {
                a[r][j] -= factor * a[col][j];
                inv[r][j] -= factor * inv[col][j];
            }
        }
    }
    Some(inv)
}

/// Parse a `pairs` parameter: an array of `[first, second]` variable-name arrays.
fn pairs(params: &JsonValue) -> SResult<Vec<(String, String)>> {
    let arr = params
        .get("pairs")
        .and_then(|v| v.as_array())
        .ok_or("Missing parameter 'pairs'")?;
    let mut out = Vec::new();
    for p in arr {
        let pa = p.as_array().filter(|a| a.len() == 2);
        let (Some(a), Some(b)) = (
            pa.and_then(|a| a[0].as_str()),
            pa.and_then(|a| a[1].as_str()),
        ) else {
            return Err("Each pair must be a [first, second] array".into());
        };
        out.push((a.to_string(), b.to_string()));
    }
    if out.is_empty() {
        return Err("Add at least one pair".into());
    }
    Ok(out)
}

/// Ordering key for frequency tables: numeric values sort numerically, strings
/// lexically, with numbers before strings. Preserves a canonical display label.
#[derive(PartialEq)]
enum OrderKey {
    Num(f64),
    Str(String),
}

impl OrderKey {
    fn from(s: &str) -> Self {
        match s.parse::<f64>() {
            Ok(n) if n.is_finite() => OrderKey::Num(n),
            _ => OrderKey::Str(s.to_string()),
        }
    }
    fn label(&self) -> String {
        match self {
            OrderKey::Num(n) => {
                if n.fract() == 0.0 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            OrderKey::Str(s) => s.clone(),
        }
    }
}

impl Eq for OrderKey {}
impl PartialOrd for OrderKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OrderKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (OrderKey::Num(a), OrderKey::Num(b)) => {
                a.partial_cmp(b).unwrap_or(Ordering::Equal)
            }
            (OrderKey::Num(_), OrderKey::Str(_)) => Ordering::Less,
            (OrderKey::Str(_), OrderKey::Num(_)) => Ordering::Greater,
            (OrderKey::Str(a), OrderKey::Str(b)) => a.cmp(b),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "expected {b}, got {a}");
    }

    #[test]
    fn normal_cdf_known() {
        close(normal_cdf(0.0), 0.5, 1e-6);
        close(normal_cdf(1.96), 0.975, 1e-3);
        close(z_sig_2tailed(1.959964), 0.05, 1e-3);
    }

    #[test]
    fn student_t_pvalue() {
        // scipy: 2 * t.sf(2.0, 10) ≈ 0.07339
        close(t_sig_2tailed(2.0, 10.0), 0.07339, 1e-3);
        // t critical at 0.05 two-tailed, df=10 is 2.2281 → p ≈ 0.05
        close(t_sig_2tailed(2.228139, 10.0), 0.05, 1e-3);
    }

    #[test]
    fn chi_square_pvalue() {
        // 95th percentile of chi2(1) is 3.8415 → upper tail 0.05
        close(chi2_sig(3.84146, 1.0), 0.05, 1e-3);
        // chi2(4) 95th pct is 9.4877
        close(chi2_sig(9.48773, 4.0), 0.05, 1e-3);
    }

    #[test]
    fn f_pvalue() {
        // F critical (1,1) at 0.05 is 161.45
        close(f_sig(161.448, 1.0, 1.0), 0.05, 2e-3);
        // F critical (3,12) at 0.05 is 3.4903
        close(f_sig(3.4903, 3.0, 12.0), 0.05, 2e-3);
    }

    #[test]
    fn pearson_perfect() {
        let a = [1.0, 2.0, 3.0, 4.0, 5.0];
        let b = [2.0, 4.0, 6.0, 8.0, 10.0];
        close(pearson(&a, &b), 1.0, 1e-9);
        let c = [5.0, 4.0, 3.0, 2.0, 1.0];
        close(pearson(&a, &c), -1.0, 1e-9);
    }

    #[test]
    fn ranks_with_ties() {
        // values 1,2,2,3 → ranks 1, 2.5, 2.5, 4
        let r = ranks(&[1.0, 2.0, 2.0, 3.0]);
        assert_eq!(r, vec![1.0, 2.5, 2.5, 4.0]);
    }

    #[test]
    fn describe_basic() {
        let d = describe(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        close(d.mean, 5.0, 1e-9);
        // sample variance = 32/7 ≈ 4.5714, sd ≈ 2.1381
        close(d.variance, 32.0 / 7.0, 1e-9);
        close(d.sd, (32.0f64 / 7.0).sqrt(), 1e-9);
        assert_eq!(d.n, 8);
        close(d.min, 2.0, 1e-9);
        close(d.max, 9.0, 1e-9);
    }

    fn sample_engine() -> Engine {
        let df = df![
            "y"     => [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
            "x"     => [2.0, 1.0, 4.0, 3.0, 6.0, 5.0, 8.0, 7.0],
            "grp"   => [1i64, 1, 1, 1, 2, 2, 2, 2],
        ]
        .unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        eng
    }

    #[test]
    fn procedures_run_without_panic() {
        let eng = sample_engine();
        let run = |proc: &str, params: serde_json::Value| {
            eng.run_analysis(proc, &params)
                .unwrap_or_else(|e| panic!("{proc} failed: {e}"))
        };

        let d = run("descriptives", serde_json::json!({ "vars": ["y", "x"] }));
        assert_eq!(d.tables[0].rows.len(), 3); // y, x, Valid N

        run("frequencies", serde_json::json!({ "vars": ["grp"] }));
        run("ttest_one_sample", serde_json::json!({ "vars": ["y"], "testValue": 4.0 }));
        run(
            "ttest_independent",
            serde_json::json!({ "vars": ["y"], "group": "grp", "group1": "1", "group2": "2" }),
        );
        run(
            "ttest_paired",
            serde_json::json!({ "pairs": [["y", "x"]] }),
        );
        run("anova_oneway", serde_json::json!({ "vars": ["y"], "factor": "grp" }));
        let c = run("correlate", serde_json::json!({ "vars": ["y", "x"], "method": "pearson" }));
        assert_eq!(c.tables[0].rows.len(), 6); // 2 vars × 3 rows
        run("regression_linear", serde_json::json!({ "dependent": "y", "independents": ["x"] }));
        run(
            "mann_whitney",
            serde_json::json!({ "vars": ["y"], "group": "grp", "group1": "1", "group2": "2" }),
        );
        run("wilcoxon", serde_json::json!({ "pairs": [["y", "x"]] }));
        run("kruskal_wallis", serde_json::json!({ "vars": ["y"], "factor": "grp" }));
        run("chi_square", serde_json::json!({ "vars": ["grp"] }));
    }

    #[test]
    fn regression_recovers_slope() {
        // y = 2 + 3x exactly → intercept 2, slope 3, R² = 1.
        let df = df![
            "y" => [5.0, 8.0, 11.0, 14.0, 17.0],
            "x" => [1.0, 2.0, 3.0, 4.0, 5.0],
        ]
        .unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        let a = eng
            .run_analysis(
                "regression_linear",
                &serde_json::json!({ "dependent": "y", "independents": ["x"] }),
            )
            .unwrap();
        // Coefficients table: (Constant) then x; column 1 is B.
        let coef = &a.tables[2];
        let b_const = coef.rows[0][1].as_f64().unwrap();
        let b_x = coef.rows[1][1].as_f64().unwrap();
        close(b_const, 2.0, 1e-6);
        close(b_x, 3.0, 1e-6);
        // Model summary R Square ≈ 1.
        close(a.tables[0].rows[0][1].as_f64().unwrap(), 1.0, 1e-9);
    }

    #[test]
    fn invert_identity() {
        let m = vec![vec![4.0, 7.0], vec![2.0, 6.0]];
        let inv = invert(m).unwrap();
        // inverse of [[4,7],[2,6]] = [[0.6,-0.7],[-0.2,0.4]]
        close(inv[0][0], 0.6, 1e-9);
        close(inv[0][1], -0.7, 1e-9);
        close(inv[1][0], -0.2, 1e-9);
        close(inv[1][1], 0.4, 1e-9);
    }
}
