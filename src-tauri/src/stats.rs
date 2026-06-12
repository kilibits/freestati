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

use std::collections::{BTreeMap, BTreeSet};

use ndarray::{Array1, Array2};
use polars::prelude::*;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::engine::Engine;

// ── Output model ─────────────────────────────────────────────────────────────

/// One rendered table: a title, column headers, and rows of heterogeneous cells
/// (strings for labels, numbers for statistics, null for "not applicable").
#[derive(Serialize, Deserialize)]
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
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub title: String,
    pub tables: Vec<OutTable>,
}

/// Computed chart data for the renderer's SVG charts. `payload` is kind-specific
/// (histogram bins, bar categories, scatter points, or box-plot summaries).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartData {
    pub title: String,
    pub kind: String,
    pub x_label: String,
    pub y_label: String,
    pub payload: JsonValue,
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

/// Optional string array (empty if absent) — for GLM factors/covariates.
fn p_strs_opt(params: &JsonValue, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

// ── Data extraction ──────────────────────────────────────────────────────────

/// Efficiently extract and clean multiple columns using Polars' Lazy API.
/// Performs listwise deletion (dropping any row with a null/NaN in ANY requested column).
fn prep_data(df: &DataFrame, cols: &[String]) -> SResult<DataFrame> {
    if cols.is_empty() {
        return Ok(df.clone());
    }
    let mut lazy = df.clone().lazy();
    let mut exprs = Vec::new();
    for name in cols {
        exprs.push(col(name));
    }
    lazy = lazy.select(&exprs).drop_nulls(None);
    // Only apply NaNs filter to numeric columns.
    for name in cols {
        if let Ok(c_col) = df.column(name) {
            if c_col.dtype().is_numeric() {
                lazy = lazy.filter(col(name).is_not_nan());
            }
        }
    }
    lazy.collect().map_err(|e| e.to_string())
}

/// Stringify any error into the crate's `SResult<_, String>` convention.
fn map_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

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

/// Extract a numeric column from a (listwise-cleaned) DataFrame as a dense
/// `Vec<f64>`. Returns a user-friendly error instead of panicking when the
/// variable is missing or non-numeric (e.g. a string picked as a dependent).
fn df_f64(df: &DataFrame, name: &str) -> SResult<Vec<f64>> {
    let s = df
        .column(name)
        .map_err(|_| format!("No such variable: {name}"))?
        .as_materialized_series();
    if !s.dtype().is_numeric() {
        return Err(format!("'{name}' must be a numeric variable for this analysis"));
    }
    let ca = s.cast(&DataType::Float64).map_err(map_err)?;
    Ok(ca.f64().map_err(map_err)?.into_no_null_iter().collect())
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

    // Sequential single pass: `describe` is called inside tight nested loops
    // (per group, per item, per dependent), so rayon's split/dispatch overhead
    // would cost far more than it saves at typical N.
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

/// Critical (positive) t value for a two-tailed area `alpha` and `df` — i.e. the
/// t with `t_sig_2tailed(t, df) == alpha`. Found by bisection (the p-value is
/// monotone decreasing in t for t ≥ 0). Used for confidence intervals.
fn t_crit(alpha: f64, df: f64) -> f64 {
    if df <= 0.0 || alpha <= 0.0 || alpha >= 1.0 {
        return f64::NAN;
    }
    let (mut lo, mut hi) = (0.0_f64, 1.0e6_f64);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if t_sig_2tailed(mid, df) > alpha {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Upper-tail p-value for F(df1, df2). F = 0 yields p = 1.
fn f_sig(f: f64, df1: f64, df2: f64) -> f64 {
    if df1 <= 0.0 || df2 <= 0.0 || f < 0.0 || !f.is_finite() {
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

/// Composite Simpson's rule for ∫_a^b f over `n` (even) subintervals.
fn simpson<F: Fn(f64) -> f64>(f: F, a: f64, b: f64, n: usize) -> f64 {
    let n = if n % 2 == 0 { n } else { n + 1 };
    let h = (b - a) / n as f64;
    let mut sum = f(a) + f(b);
    for i in 1..n {
        let x = a + i as f64 * h;
        sum += if i % 2 == 1 { 4.0 } else { 2.0 } * f(x);
    }
    sum * h / 3.0
}

/// CDF of the range of `k` i.i.d. standard normals at `w`:
/// k ∫ φ(z) [Φ(z) − Φ(z − w)]^(k−1) dz.
fn range_cdf(w: f64, k: f64) -> f64 {
    if w <= 0.0 {
        return 0.0;
    }
    let kf = k;
    let integrand = |z: f64| {
        let inner = normal_cdf(z) - normal_cdf(z - w);
        if inner <= 0.0 {
            0.0
        } else {
            normal_pdf(z) * inner.powf(kf - 1.0)
        }
    };
    (kf * simpson(integrand, -8.0, 8.0, 400)).clamp(0.0, 1.0)
}

fn normal_pdf(z: f64) -> f64 {
    (-0.5 * z * z).exp() / (2.0 * std::f64::consts::PI).sqrt()
}

/// CDF of the studentized range Q(q; k, df): P(studentized range ≤ q) for `k`
/// groups and `df` error degrees of freedom. Integrates `range_cdf(q·s)` over
/// the sampling distribution of s = √(χ²_df / df). Used for Tukey HSD p-values.
fn ptukey(q: f64, k: f64, df: f64) -> f64 {
    if q <= 0.0 {
        return 0.0;
    }
    if df > 25_000.0 {
        return range_cdf(q, k);
    }
    let f = df;
    let log_c = std::f64::consts::LN_2 + (f / 2.0) * (f / 2.0).ln() - ln_gamma(f / 2.0);
    let dens = |s: f64| {
        if s <= 0.0 {
            0.0
        } else {
            (log_c + (f - 1.0) * s.ln() - f * s * s / 2.0).exp()
        }
    };
    // χ²_df mass lies below f + ~10√(2f); map back to s = √(χ²/df).
    let s_max = ((f + 10.0 * (2.0 * f).sqrt()) / f).sqrt().max(1.5);
    simpson(|s| dens(s) * range_cdf(q * s, k), 0.0, s_max, 400).clamp(0.0, 1.0)
}

/// Tukey HSD p-value: upper tail of the studentized range.
fn tukey_sig(q: f64, k: f64, df: f64) -> f64 {
    (1.0 - ptukey(q, k, df)).clamp(0.0, 1.0)
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
            "anova_oneway" => self.anova_oneway(
                df,
                &p_strs(params, "vars")?,
                &p_str(params, "factor")?,
                &p_str_opt(params, "posthoc").unwrap_or_else(|| "none".into()),
            ),
            "crosstabs" => {
                self.crosstabs(df, &p_str(params, "row")?, &p_str(params, "col")?)
            }
            "factor" => self.factor(
                df,
                &p_strs(params, "vars")?,
                p_str_opt(params, "rotation").as_deref().unwrap_or("none"),
                params.get("factors").and_then(|v| v.as_u64()).map(|n| n as usize),
            ),
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
            "reliability" => self.reliability(df, &p_strs(params, "vars")?),
            "glm_univariate" => self.glm_univariate(
                df,
                &p_str(params, "dependent")?,
                &p_strs_opt(params, "factors"),
                &p_strs_opt(params, "covariates"),
            ),
            "glm_multivariate" => self.glm_multivariate(
                df,
                &p_strs(params, "dependents")?,
                &p_strs_opt(params, "factors"),
                &p_strs_opt(params, "covariates"),
            ),
            "mixed_model" => self.mixed_model(
                df,
                &p_str(params, "dependent")?,
                &p_strs(params, "randomFactors")?,
                &p_strs_opt(params, "covariates"),
            ),
            "survival_km" => self.survival_km(
                df,
                &p_str(params, "time")?,
                &p_str(params, "status")?,
                &p_str_opt(params, "eventValue").unwrap_or_else(|| "1".into()),
                p_str_opt(params, "factor").as_deref(),
            ),
            "cox_regression" => self.cox_regression(
                df,
                &p_str(params, "time")?,
                p_str_opt(params, "startTime").as_deref(),
                &p_str(params, "status")?,
                &p_str_opt(params, "eventValue").unwrap_or_else(|| "1".into()),
                &p_strs(params, "covariates")?,
            ),
            "glm_repeated" => self.glm_repeated(df, &p_strs(params, "vars")?),
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
            
            if total == 0 {
                tables.push(t);
                continue;
            }

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
                "Cohen's d",
                "95% CI Lower",
                "95% CI Upper",
            ],
        )
        .footnote(format!("Test Value = {test_value}; 95% CI of the mean difference."));

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
            let mean_diff = d.mean - test_value;
            let t = mean_diff / d.sem;
            let margin = t_crit(0.05, df_t) * d.sem;
            test.rows.push(vec![
                text(self.display(v)),
                num(t),
                num(df_t),
                num(t_sig_2tailed(t, df_t)),
                num(mean_diff),
                num(mean_diff / d.sd),
                num(mean_diff - margin),
                num(mean_diff + margin),
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
        let mut levene = OutTable::new(
            "Levene's Test for Equality of Variances",
            vec!["", "F", "Sig."],
        )
        .footnote("Based on absolute deviations from group means.");
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
                "Cohen's d",
                "95% CI Lower",
                "95% CI Upper",
            ],
        )
        .footnote("95% CI of the mean difference. Cohen's d uses the pooled standard deviation.");

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

            // Levene's test: one-way ANOVA on |xᵢⱼ − group meanᵢ|.
            let (lf, lsig) = levene_two(&a, &b, da.mean, db.mean);
            levene.rows.push(vec![text(self.display(v)), num(lf), num(lsig)]);
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

            // Cohen's d with the pooled SD; 95% CI of the mean difference.
            let cohens_d = mean_diff / sp2.sqrt();
            let margin_pool = t_crit(0.05, df_pool) * se_pool;
            let margin_welch = t_crit(0.05, df_welch) * se_welch;

            test.rows.push(vec![
                text(self.display(v)),
                text("Equal variances assumed"),
                num(t_pool),
                num(df_pool),
                num(t_sig_2tailed(t_pool, df_pool)),
                num(mean_diff),
                num(se_pool),
                num(cohens_d),
                num(mean_diff - margin_pool),
                num(mean_diff + margin_pool),
            ]);
            test.rows.push(vec![
                text(""),
                text("Equal variances not assumed"),
                num(t_welch),
                num(df_welch),
                num(t_sig_2tailed(t_welch, df_welch)),
                num(mean_diff),
                num(se_welch),
                num(cohens_d),
                num(mean_diff - margin_welch),
                num(mean_diff + margin_welch),
            ]);
        }
        Ok(Analysis {
            title: "Independent-Samples T Test".into(),
            tables: vec![stats, levene, test],
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
                "Cohen's d",
                "95% CI Lower",
                "95% CI Upper",
            ],
        )
        .footnote("95% CI of the mean difference; Cohen's d = mean difference / SD of differences.");
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
            let margin = t_crit(0.05, df_t) * sd.sem;
            test.rows.push(vec![
                text(format!("{} - {}", self.display(a), self.display(b))),
                num(sd.mean),
                num(sd.sd),
                num(sd.sem),
                num(t),
                num(df_t),
                num(t_sig_2tailed(t, df_t)),
                num(sd.mean / sd.sd),
                num(sd.mean - margin),
                num(sd.mean + margin),
            ]);
        }
        Ok(Analysis {
            title: "Paired-Samples T Test".into(),
            tables: vec![stats, test],
        })
    }

    fn anova_oneway(
        &self,
        df: &DataFrame,
        vars: &[String],
        factor: &str,
        posthoc: &str,
    ) -> SResult<Analysis> {
        let labels = col_labels(df, factor)?;
        let mut tables = Vec::new();
        for v in vars {
            let xs = col_opt(df, v)?;
            // Group values by factor level (sorted by level for a stable order).
            let mut grouped: BTreeMap<String, Vec<f64>> = BTreeMap::new();
            for (i, lab) in labels.iter().enumerate() {
                if let (Some(l), Some(val)) = (lab, xs[i]) {
                    grouped.entry(l.clone()).or_default().push(val);
                }
            }
            let level_names: Vec<String> = grouped.keys().cloned().collect();
            let groups: Vec<Vec<f64>> = grouped.into_values().collect();
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

            // Effect size (Measures of Association): η and η².
            let eta_sq = if ss_total > 0.0 { ss_between / ss_total } else { f64::NAN };
            let mut assoc = OutTable::new(
                format!("Measures of Association — {}", self.display(v)),
                vec!["Eta", "Eta Squared"],
            );
            assoc.rows.push(vec![num(eta_sq.sqrt()), num(eta_sq)]);
            tables.push(assoc);

            if !posthoc.eq_ignore_ascii_case("none") {
                tables.push(self.posthoc_table(
                    &self.display(v),
                    &level_names,
                    &groups,
                    ms_within,
                    df_within,
                    posthoc,
                )?);
            }
        }
        Ok(Analysis {
            title: "One-Way ANOVA".into(),
            tables,
        })
    }

    /// Post-hoc pairwise comparisons (LSD, Bonferroni, or Tukey HSD) using the
    /// pooled within-groups variance.
    fn posthoc_table(
        &self,
        dep: &str,
        levels: &[String],
        groups: &[Vec<f64>],
        ms_within: f64,
        df_within: f64,
        method: &str,
    ) -> SResult<OutTable> {
        let bonferroni = method.eq_ignore_ascii_case("bonferroni");
        let tukey = method.eq_ignore_ascii_case("tukey");
        let method_label = if tukey {
            "Tukey HSD"
        } else if bonferroni {
            "Bonferroni"
        } else {
            "LSD"
        };
        let means: Vec<f64> = groups
            .iter()
            .map(|g| g.iter().sum::<f64>() / g.len() as f64)
            .collect();
        let k = groups.len();
        let n_comparisons = (k * (k - 1) / 2) as f64;

        let mut t = OutTable::new(
            format!("Multiple Comparisons — {dep} ({method_label})"),
            vec![
                "(I) Group",
                "(J) Group",
                "Mean Difference (I-J)",
                "Std. Error",
                "Sig.",
            ],
        );
        for i in 0..k {
            for j in 0..k {
                if i == j {
                    continue;
                }
                let ni = groups[i].len() as f64;
                let nj = groups[j].len() as f64;
                let diff = means[i] - means[j];
                let se = (ms_within * (1.0 / ni + 1.0 / nj)).sqrt();
                let p = if tukey {
                    // Tukey-Kramer: q = |diff| / √(MSE/2·(1/nᵢ+1/nⱼ)).
                    let q = diff.abs() / (ms_within * 0.5 * (1.0 / ni + 1.0 / nj)).sqrt();
                    tukey_sig(q, k as f64, df_within)
                } else {
                    let p = t_sig_2tailed(diff / se, df_within);
                    if bonferroni {
                        (p * n_comparisons).min(1.0)
                    } else {
                        p
                    }
                };
                t.rows.push(vec![
                    text(&levels[i]),
                    text(&levels[j]),
                    num(diff),
                    num(se),
                    num(p),
                ]);
            }
        }
        Ok(t)
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
        let mut y_vec = Vec::new();
        let mut x_flat = Vec::new(); // will convert to Array2
        let mut n = 0;
        for i in 0..nrows {
            let Some(yi) = y_all[i] else { continue };
            if x_all.iter().any(|c| c[i].is_none()) {
                continue;
            }
            y_vec.push(yi);
            x_flat.push(1.0);
            for c in &x_all {
                x_flat.push(c[i].unwrap());
            }
            n += 1;
        }
        if n <= p + 1 {
            return Err("Not enough valid cases for the number of predictors".into());
        }
        let k = p + 1; // params including intercept
        let y = Array1::from_vec(y_vec);
        let x = Array2::from_shape_vec((n, k), x_flat).map_err(map_err)?;

        // Normal equations: (XᵀX) β = Xᵀy.
        let xt = x.t();
        let xtx = xt.dot(&x);
        let xty = xt.dot(&y);
        let inv = invert(&xtx).ok_or("Predictors are collinear (singular matrix)")?;
        let beta = inv.dot(&xty);

        // Residuals and sums of squares.
        let ybar = y.mean().ok_or("Cannot compute mean of empty dependent variable")?;
        let yhat = x.dot(&beta);
        let residuals = &y - &yhat;
        let ss_res = residuals.mapv(|r| r * r).sum();
        let ss_tot = y.mapv(|v| (v - ybar).powi(2)).sum();

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
        let y_sd = describe(y.as_slice().unwrap()).sd;
        let x_sd: Vec<f64> = (0..p)
            .map(|j| {
                let col = x.column(j + 1);
                describe(&col.to_vec()).sd
            })
            .collect();
        
        // Ensure y_sd is not NaN before proceeding, fallback to 1.0 to avoid division by zero
        let y_sd_safe = if y_sd.is_finite() { y_sd } else { 1.0 };
        let x_sd_safe: Vec<f64> = x_sd.iter().map(|&s| if s.is_finite() { s } else { 1.0 }).collect();

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

        // Coefficients (with 95% confidence intervals for B).
        let tc = t_crit(0.05, df_res);
        let mut coef = OutTable::new(
            "Coefficients",
            vec![
                "",
                "B",
                "Std. Error",
                "Beta",
                "t",
                "Sig.",
                "95% CI Lower",
                "95% CI Upper",
            ],
        );
        for a in 0..k {
            let se = (mse * inv[[a, a]]).sqrt();
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
                num(beta[a] - tc * se),
                num(beta[a] + tc * se),
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

    // ── Crosstabs ─────────────────────────────────────────────────────────────

    fn crosstabs(&self, df: &DataFrame, row: &str, col: &str) -> SResult<Analysis> {
        let row_labels = col_labels(df, row)?;
        let col_labels_v = col_labels(df, col)?;
        let row_vl = self.var_meta.get(row).and_then(|m| m.value_labels.clone());
        let col_vl = self.var_meta.get(col).and_then(|m| m.value_labels.clone());

        // Distinct, ordered categories for each variable.
        let row_cats = ordered_levels(&row_labels);
        let col_cats = ordered_levels(&col_labels_v);
        if row_cats.is_empty() || col_cats.is_empty() {
            return Err("Both variables need at least one category".into());
        }

        // Observed counts O[r][c].
        let mut obs = vec![vec![0usize; col_cats.len()]; row_cats.len()];
        let mut total = 0usize;
        for i in 0..row_labels.len() {
            if let (Some(r), Some(c)) = (&row_labels[i], &col_labels_v[i]) {
                let ri = row_cats.iter().position(|x| x == r).unwrap();
                let ci = col_cats.iter().position(|x| x == c).unwrap();
                obs[ri][ci] += 1;
                total += 1;
            }
        }
        let row_tot: Vec<usize> = obs.iter().map(|r| r.iter().sum()).collect();
        let col_tot: Vec<usize> = (0..col_cats.len())
            .map(|c| obs.iter().map(|r| r[c]).sum())
            .collect();

        // Crosstabulation table (counts with margins).
        let label_for = |raw: &str, vl: &Option<std::collections::HashMap<String, String>>| {
            vl.as_ref()
                .and_then(|m| m.get(raw).cloned())
                .unwrap_or_else(|| raw.to_string())
        };
        let mut headers = vec![format!("{} ╲ {}", self.display(row), self.display(col))];
        headers.extend(col_cats.iter().map(|c| label_for(c, &col_vl)));
        headers.push("Total".into());
        let header_refs: Vec<&str> = headers.iter().map(|s| s.as_str()).collect();
        let mut xt = OutTable::new("Crosstabulation (Count)", header_refs);
        for (ri, rc) in row_cats.iter().enumerate() {
            let mut cells = vec![text(label_for(rc, &row_vl))];
            for ci in 0..col_cats.len() {
                cells.push(int(obs[ri][ci] as i64));
            }
            cells.push(int(row_tot[ri] as i64));
            xt.rows.push(cells);
        }
        let mut total_row = vec![text("Total")];
        for ci in 0..col_cats.len() {
            total_row.push(int(col_tot[ci] as i64));
        }
        total_row.push(int(total as i64));
        xt.rows.push(total_row);

        // Chi-square test of independence + likelihood ratio + Cramér's V.
        let mut chi2 = 0.0;
        let mut g2 = 0.0;
        for ri in 0..row_cats.len() {
            for ci in 0..col_cats.len() {
                let e = row_tot[ri] as f64 * col_tot[ci] as f64 / total as f64;
                let o = obs[ri][ci] as f64;
                if e > 0.0 {
                    chi2 += (o - e).powi(2) / e;
                    if o > 0.0 {
                        g2 += 2.0 * o * (o / e).ln();
                    }
                }
            }
        }
        let dfree = (row_cats.len() as f64 - 1.0) * (col_cats.len() as f64 - 1.0);
        let min_dim = (row_cats.len().min(col_cats.len()) as f64 - 1.0).max(1.0);
        let cramers_v = (chi2 / (total as f64 * min_dim)).sqrt();

        let mut test = OutTable::new("Chi-Square Tests", vec!["", "Value", "df", "Asymp. Sig. (2-sided)"]);
        test.rows.push(vec![
            text("Pearson Chi-Square"),
            num(chi2),
            num(dfree),
            num(chi2_sig(chi2, dfree)),
        ]);
        test.rows.push(vec![
            text("Likelihood Ratio"),
            num(g2),
            num(dfree),
            num(chi2_sig(g2, dfree)),
        ]);
        test.rows.push(vec![
            text("N of Valid Cases"),
            int(total as i64),
            JsonValue::Null,
            JsonValue::Null,
        ]);

        let mut measures = OutTable::new("Symmetric Measures", vec!["", "Value"]);
        measures.rows.push(vec![text("Cramér's V"), num(cramers_v)]);

        Ok(Analysis {
            title: format!("Crosstabs — {} × {}", self.display(row), self.display(col)),
            tables: vec![xt, test, measures],
        })
    }

    // ── Factor analysis (principal components) ────────────────────────────────

    fn factor(
        &self,
        df: &DataFrame,
        vars: &[String],
        rotation: &str,
        fixed_factors: Option<usize>,
    ) -> SResult<Analysis> {
        let p = vars.len();
        if p < 2 {
            return Err("Factor analysis needs at least 2 variables".into());
        }
        // Listwise-deleted data matrix.
        let mut data_flat = Vec::new();
        let mut n = 0;
        let cols: Vec<Vec<Option<f64>>> =
            vars.iter().map(|v| col_opt(df, v)).collect::<SResult<_>>()?;
        let nrows = df.height();
        for i in 0..nrows {
            if cols.iter().all(|c| c[i].is_some()) {
                for c in &cols {
                    data_flat.push(c[i].unwrap());
                }
                n += 1;
            }
        }
        if n <= p {
            return Err("Not enough complete cases for the number of variables".into());
        }
        let data = Array2::from_shape_vec((n, p), data_flat).map_err(map_err)?;

        // Correlation matrix R (p × p).
        let mut corr = Array2::zeros((p, p));
        let means = data.mean_axis(ndarray::Axis(0)).ok_or_else(|| "Empty data".to_string())?;
        let mut sds = Array1::zeros(p);
        for j in 0..p {
            let col = data.column(j);
            sds[j] = describe(&col.to_vec()).sd;
        }

        for a in 0..p {
            for b in 0..p {
                let mut cov = 0.0;
                for i in 0..n {
                    cov += (data[[i, a]] - means[a]) * (data[[i, b]] - means[b]);
                }
                cov /= n as f64 - 1.0;
                corr[[a, b]] = if sds[a] > 0.0 && sds[b] > 0.0 {
                    cov / (sds[a] * sds[b])
                } else if a == b {
                    1.0
                } else {
                    0.0
                };
            }
        }

        // Eigen-decomposition (sorted descending).
        let (eigvals, eigvecs) = jacobi_eigen(&corr);
        let mut order: Vec<usize> = (0..p).collect();
        order.sort_by(|&a, &b| eigvals[b].partial_cmp(&eigvals[a]).unwrap_or(std::cmp::Ordering::Equal));

        // Number of components: fixed count, else Kaiser (eigenvalue > 1).
        let m = match fixed_factors {
            Some(m) => m.clamp(1, p),
            None => order.iter().filter(|&&i| eigvals[i] > 1.0).count().max(1),
        };

        // Unrotated loadings: a_ij = v_ij * sqrt(λ_j).
        let mut loadings = Array2::zeros((p, m)); // p vars × m components
        for (cidx, &ei) in order.iter().take(m).enumerate() {
            let scale = eigvals[ei].max(0.0).sqrt();
            for var in 0..p {
                loadings[[var, cidx]] = eigvecs[[var, ei]] * scale;
            }
        }

        // Total variance explained.
        let total_var = p as f64;
        let mut variance = OutTable::new(
            "Total Variance Explained",
            vec!["Component", "Eigenvalue", "% of Variance", "Cumulative %"],
        );
        let mut cum = 0.0;
        for (rank, &ei) in order.iter().enumerate() {
            let pct = eigvals[ei] / total_var * 100.0;
            cum += pct;
            variance.rows.push(vec![
                int(rank as i64 + 1),
                num(eigvals[ei]),
                num(pct),
                num(cum),
            ]);
        }

        // Communalities (PCA: initial = 1, extraction = Σ loadings²).
        let mut comm = OutTable::new("Communalities", vec!["", "Initial", "Extraction"]);
        for var in 0..p {
            let mut extraction = 0.0;
            for c in 0..m {
                extraction += loadings[[var, c]].powi(2);
            }
            comm.rows.push(vec![text(self.display(&vars[var])), num(1.0), num(extraction)]);
        }

        let mut tables = vec![comm, variance];
        tables.push(self.loading_table("Component Matrix", vars, &loadings));

        // Optional varimax rotation.
        if rotation.eq_ignore_ascii_case("varimax") && m > 1 {
            let rotated = varimax(&loadings);
            tables.push(self.loading_table("Rotated Component Matrix (Varimax)", vars, &rotated));
        }

        Ok(Analysis {
            title: "Factor Analysis (Principal Components)".into(),
            tables,
        })
    }

    fn loading_table(
        &self,
        title: &str,
        vars: &[String],
        loadings: &Array2<f64>,
    ) -> OutTable {
        let m = loadings.ncols();
        let mut headers = vec!["".to_string()];
        for c in 0..m {
            headers.push(format!("Component {}", c + 1));
        }
        let refs: Vec<&str> = headers.iter().map(|s| s.as_str()).collect();
        let mut t = OutTable::new(title, refs);
        for var in 0..loadings.nrows() {
            let mut cells = vec![text(self.display(&vars[var]))];
            for c in 0..m {
                cells.push(num(loadings[[var, c]]));
            }
            t.rows.push(cells);
        }
        t
    }

    // ── Reliability (Cronbach's alpha) ────────────────────────────────────────

    fn reliability(&self, df: &DataFrame, vars: &[String]) -> SResult<Analysis> {
        let k = vars.len();
        if k < 2 {
            return Err("Reliability analysis needs at least 2 items".into());
        }
        // Listwise-deleted item matrix.
        let cols: Vec<Vec<Option<f64>>> =
            vars.iter().map(|v| col_opt(df, v)).collect::<SResult<_>>()?;
        let nrows = df.height();
        let mut rows: Vec<Vec<f64>> = Vec::new();
        for i in 0..nrows {
            if cols.iter().all(|c| c[i].is_some()) {
                rows.push(cols.iter().map(|c| c[i].unwrap()).collect());
            }
        }
        let n = rows.len();
        if n < 2 {
            return Err("Not enough complete cases for reliability".into());
        }

        let item = |j: usize| -> Vec<f64> { rows.iter().map(|r| r[j]).collect() };
        let total: Vec<f64> = rows.iter().map(|r| r.iter().sum()).collect();
        let var_total = describe(&total).variance;
        let sum_item_var: f64 = (0..k).map(|j| describe(&item(j)).variance).sum();
        let kf = k as f64;
        let alpha = (kf / (kf - 1.0)) * (1.0 - sum_item_var / var_total);

        // Standardized alpha from the mean inter-item correlation.
        let mut r_sum = 0.0;
        let mut r_cnt = 0.0;
        for a in 0..k {
            for b in (a + 1)..k {
                r_sum += pearson(&item(a), &item(b));
                r_cnt += 1.0;
            }
        }
        let mean_r = r_sum / r_cnt;
        let std_alpha = (kf * mean_r) / (1.0 + (kf - 1.0) * mean_r);

        let mut rel = OutTable::new(
            "Reliability Statistics",
            vec!["Cronbach's Alpha", "Cronbach's Alpha (Standardized)", "N of Items"],
        );
        rel.rows.push(vec![num(alpha), num(std_alpha), int(k as i64)]);

        // Item-Total Statistics.
        let mut it = OutTable::new(
            "Item-Total Statistics",
            vec![
                "",
                "Scale Mean if Item Deleted",
                "Scale Variance if Item Deleted",
                "Corrected Item-Total Correlation",
                "Cronbach's Alpha if Item Deleted",
            ],
        );
        let grand_mean = describe(&total).mean;
        for j in 0..k {
            let item_j = item(j);
            let item_mean = describe(&item_j).mean;
            // Scale score with item j removed (sum of the other items per case).
            let rest_total: Vec<f64> = (0..n).map(|i| total[i] - rows[i][j]).collect();
            let scale_mean = grand_mean - item_mean;
            let scale_var = describe(&rest_total).variance;
            let corrected = pearson(&item_j, &rest_total);

            // Alpha with item j removed.
            let k2 = (k - 1) as f64;
            let sum_var2 = sum_item_var - describe(&item_j).variance;
            let alpha2 = (k2 / (k2 - 1.0)) * (1.0 - sum_var2 / scale_var);

            it.rows.push(vec![
                text(self.display(&vars[j])),
                num(scale_mean),
                num(scale_var),
                num(corrected),
                num(alpha2),
            ]);
        }

        Ok(Analysis {
            title: "Reliability Analysis".into(),
            tables: vec![rel, it],
        })
    }

    // ── GLM Univariate (factorial ANOVA / ANCOVA, Type III SS) ────────────────

    fn glm_univariate(
        &self,
        df: &DataFrame,
        dependent: &str,
        factors: &[String],
        covariates: &[String],
    ) -> SResult<Analysis> {
        if factors.is_empty() && covariates.is_empty() {
            return Err("Specify at least one factor or covariate".into());
        }
        
        let mut all_cols = vec![dependent.to_string()];
        all_cols.extend(factors.iter().cloned());
        all_cols.extend(covariates.iter().cloned());
        let clean_df = prep_data(df, &all_cols)?;
        let n = clean_df.height();
        if n < 3 {
            return Err("Not enough complete cases".into());
        }

        let y: Vec<f64> = df_f64(&clean_df, dependent)?;

        // Per-factor row labels (extracted once; the columns exist post-prep).
        let factor_labels: Vec<Vec<Option<String>>> =
            factors.iter().map(|f| col_labels(&clean_df, f)).collect::<SResult<Vec<_>>>()?;

        // Factor levels (sorted, distinct).
        let levels: Vec<Vec<String>> = factor_labels.iter().map(|labs| {
            let mut lv = labs.iter().flatten().cloned().collect::<Vec<_>>();
            lv.sort();
            lv.dedup();
            lv
        }).collect();

        let mut effects: Vec<Effect> = Vec::new();
        for cov in covariates.iter() {
            let col: Vec<f64> = df_f64(&clean_df, cov)?;
            let n_c = col.len();
            effects.push(Effect { name: self.display(cov), cols: Array2::from_shape_vec((n_c, 1), col).map_err(map_err)? });
        }

        let dummies: Vec<Vec<Vec<f64>>> = factor_labels.iter().enumerate().map(|(fi, row_labels)| {
            levels[fi].iter().skip(1).map(|lev| {
                row_labels.iter().map(|o| if o.as_deref() == Some(lev) { 1.0 } else { 0.0 }).collect()
            }).collect()
        }).collect();

        let nf = factors.len();
        for mask in 1u32..(1u32 << nf) {
            let involved: Vec<usize> = (0..nf).filter(|&b| mask & (1 << b) != 0).collect();
            let name = involved.iter().map(|&b| self.display(&factors[b])).collect::<Vec<_>>().join(" * ");
            let mut cols: Vec<Vec<f64>> = vec![vec![1.0; n]];
            for &b in &involved {
                let mut next = Vec::new();
                for existing in &cols {
                    for dcol in &dummies[b] {
                        next.push((0..n).map(|i| existing[i] * dcol[i]).collect());
                    }
                }
                cols = next;
            }
            let n_r = cols[0].len();
            let n_c = cols.len();
            let mut flat = Vec::with_capacity(n_r * n_c);
            for i in 0..n_r {
                for j in 0..n_c {
                    flat.push(cols[j][i]);
                }
            }
            effects.push(Effect { name, cols: Array2::from_shape_vec((n_r, n_c), flat).map_err(map_err)? });
        }

        let total_cols: usize = effects.iter().map(|e| e.cols.ncols()).sum();
        let mut x_flat = Vec::with_capacity(n * (1 + total_cols));
        x_flat.extend(std::iter::repeat(1.0).take(n));
        for e in &effects {
            for col_idx in 0..e.cols.ncols() {
                for i in 0..n {
                    x_flat.push(e.cols[[i, col_idx]]);
                }
            }
        }
        let x_full = Array2::from_shape_vec((1 + total_cols, n), x_flat).map_err(map_err)?.reversed_axes(); // n x k

        let solve_sse = |x: &Array2<f64>, y: &[f64]| -> Option<f64> {
            let y_arr = Array1::from_vec(y.to_vec());
            let xtx = x.t().dot(x);
            let xtx_inv = invert(&xtx)?;
            let beta = xtx_inv.dot(&x.t().dot(&y_arr));
            let resid = &y_arr - &x.dot(&beta);
            Some(resid.dot(&resid))
        };

        let sse_full = solve_sse(&x_full, &y).ok_or("Design matrix is singular (collinear effects)")?;
        let df_error = (n - 1 - total_cols) as f64;
        if df_error <= 0.0 {
            return Err("Too many parameters for the number of cases".into());
        }
        let mse = sse_full / df_error;

        let ybar = y.iter().sum::<f64>() / n as f64;
        let ss_total_corr: f64 = y.iter().map(|v| (v - ybar).powi(2)).sum();
        let ss_total_unc: f64 = y.iter().map(|v| v * v).sum();
        let ss_model = ss_total_corr - sse_full;
        let df_model = total_cols as f64;

        let mut t = OutTable::new(
            format!("Tests of Between-Subjects Effects — {}", self.display(dependent)),
            vec![
                "Source",
                "Type III Sum of Squares",
                "df",
                "Mean Square",
                "F",
                "Sig.",
                "Partial Eta Squared",
            ],
        );
        let eff_row = |name: &str, ss: f64, dfx: f64| -> Vec<JsonValue> {
            let ms = ss / dfx;
            let f = ms / mse;
            let partial = ss / (ss + sse_full);
            vec![
                text(name),
                num(ss),
                num(dfx),
                num(ms),
                num(f),
                num(f_sig(f, dfx, df_error)),
                num(partial),
            ]
        };

        t.rows.push(eff_row("Corrected Model", ss_model, df_model));

        // Intercept.
        if total_cols > 0 {
            let x_no_int = x_full.slice(ndarray::s![.., 1..]).to_owned();
            if let Some(sse_ni) = solve_sse(&x_no_int, &y) {
                t.rows.push(eff_row("Intercept", sse_ni - sse_full, 1.0));
            }
        }

        // Effects.
        let mut cur_col = 1;
        for e in &effects {
            let k_e = e.cols.ncols();
            let mut indices = (0..x_full.ncols()).collect::<Vec<_>>();
            indices.drain(cur_col..(cur_col + k_e));
            let x_reduced = x_full.select(ndarray::Axis(1), &indices);
            let ss = match solve_sse(&x_reduced, &y) {
                Some(sse_r) => sse_r - sse_full,
                None => f64::NAN,
            };
            t.rows.push(eff_row(&e.name, ss, k_e as f64));
            cur_col += k_e;
        }

        t.rows.push(vec![
            text("Error"),
            num(sse_full),
            num(df_error),
            num(mse),
            JsonValue::Null,
            JsonValue::Null,
            JsonValue::Null,
        ]);
        t.rows.push(vec![
            text("Total"),
            num(ss_total_unc),
            num(n as f64),
            JsonValue::Null,
            JsonValue::Null,
            JsonValue::Null,
            JsonValue::Null,
        ]);
        t.rows.push(vec![
            text("Corrected Total"),
            num(ss_total_corr),
            num(n as f64 - 1.0),
            JsonValue::Null,
            JsonValue::Null,
            JsonValue::Null,
            JsonValue::Null,
        ]);

        let r2 = if ss_total_corr > 0.0 { ss_model / ss_total_corr } else { f64::NAN };
        let t = t.footnote(format!("R Squared = {:.3} (Type III sums of squares).", r2));

        Ok(Analysis {
            title: "GLM Univariate".into(),
            tables: vec![t],
        })
    }

    // ── GLM Multivariate (one-way MANOVA) ─────────────────────────────────────

    fn glm_multivariate(
        &self,
        df: &DataFrame,
        dependents: &[String],
        factors: &[String],
        covariates: &[String],
    ) -> SResult<Analysis> {
        let p = dependents.len();
        if p < 2 {
            return Err("MANOVA needs at least 2 dependent variables".into());
        }
        if factors.is_empty() && covariates.is_empty() {
            return Err("Specify at least one factor or covariate".into());
        }

        let mut all_cols = dependents.to_vec();
        all_cols.extend(factors.iter().cloned());
        all_cols.extend(covariates.iter().cloned());
        let clean_df = prep_data(df, &all_cols)?;
        let n = clean_df.height();
        if n < p + 5 {
            return Err("Not enough complete cases".into());
        }

        let mut y_flat = Vec::with_capacity(n * p);
        for dep in dependents {
            y_flat.extend(df_f64(&clean_df, dep)?);
        }
        let y_data = Array2::from_shape_vec((p, n), y_flat).map_err(map_err)?; // p x n

        // Design matrix.
        let mut effects: Vec<Effect> = Vec::new();
        for cov in covariates.iter() {
            let col: Vec<f64> = df_f64(&clean_df, cov)?;
            let n_c = col.len();
            effects.push(Effect { name: self.display(cov), cols: Array2::from_shape_vec((n_c, 1), col).map_err(map_err)? });
        }

        // Per-factor row labels (extracted once; the columns exist post-prep).
        let factor_labels: Vec<Vec<Option<String>>> =
            factors.iter().map(|f| col_labels(&clean_df, f)).collect::<SResult<Vec<_>>>()?;

        let levels: Vec<Vec<String>> = factor_labels.iter().map(|labs| {
            let mut lv = labs.iter().flatten().cloned().collect::<Vec<_>>();
            lv.sort();
            lv.dedup();
            lv
        }).collect();

        let dummies: Vec<Vec<Vec<f64>>> = factor_labels.iter().enumerate().map(|(fi, row_labels)| {
            levels[fi].iter().skip(1).map(|lev| {
                row_labels.iter().map(|o| if o.as_deref() == Some(lev) { 1.0 } else { 0.0 }).collect()
            }).collect()
        }).collect();

        let nf = factors.len();
        for mask in 1u32..(1u32 << nf) {
            let involved: Vec<usize> = (0..nf).filter(|&b| mask & (1 << b) != 0).collect();
            let name = involved.iter().map(|&b| self.display(&factors[b])).collect::<Vec<_>>().join(" * ");
            let mut cols: Vec<Vec<f64>> = vec![vec![1.0; n]];
            for &b in &involved {
                let mut next = Vec::new();
                for existing in &cols {
                    for dcol in &dummies[b] {
                        next.push((0..n).map(|i| existing[i] * dcol[i]).collect());
                    }
                }
                cols = next;
            }
            let n_r = cols[0].len();
            let n_c = cols.len();
            let mut flat = Vec::with_capacity(n_r * n_c);
            for i in 0..n_r {
                for j in 0..n_c {
                    flat.push(cols[j][i]);
                }
            }
            effects.push(Effect { name, cols: Array2::from_shape_vec((n_r, n_c), flat).map_err(map_err)? });
        }

        let total_k = 1 + effects.iter().map(|e| e.cols.ncols()).sum::<usize>();
        let mut x_flat = Vec::with_capacity(n * total_k);
        x_flat.extend(std::iter::repeat(1.0).take(n));
        for e in &effects {
            for col_idx in 0..e.cols.ncols() {
                for i in 0..n {
                    x_flat.push(e.cols[[i, col_idx]]);
                }
            }
        }
        let x = Array2::from_shape_vec((total_k, n), x_flat).map_err(map_err)?.reversed_axes(); // n x total_k
        let df_e = (n - total_k) as f64;
        if df_e <= 0.0 {
            return Err("Too many parameters for the number of cases".into());
        }

        let xtx = x.t().dot(&x);
        let xtx_inv = invert(&xtx).ok_or("Design matrix is singular")?;

        let mut est_table = OutTable::new(
            "Parameter Estimates",
            vec!["Dependent Variable", "Parameter", "B", "Std. Error", "t", "Sig."],
        );

        let mut e_mat = Array2::zeros((p, p));
        let mut h_mat = Array2::zeros((p, p));

        for ji in 0..p {
            let y_ji = y_data.row(ji);
            let xty = x.t().dot(&y_ji);
            let beta = xtx_inv.dot(&xty);
            let yhat = x.dot(&beta);
            let resid = &y_ji - &yhat;
            let sse = resid.dot(&resid);
            let mse = sse / df_e;

            for j2 in 0..p {
                let y_j2 = y_data.row(j2);
                let xty2 = x.t().dot(&y_j2);
                let beta2 = xtx_inv.dot(&xty2);
                let yhat2 = x.dot(&beta2);
                
                let grand2 = y_j2.mean().unwrap();
                let diff1 = &yhat - y_ji.mean().unwrap();
                let diff2 = &yhat2 - grand2;
                h_mat[[ji, j2]] = diff1.dot(&diff2);
                e_mat[[ji, j2]] = (&y_ji - &yhat).dot(&(&y_j2 - &yhat2));
            }

            let dep_name = &dependents[ji];
            let se_int = (mse * xtx_inv[[0, 0]]).sqrt();
            let t_int = beta[0] / se_int;
            est_table.rows.push(vec![text(self.display(dep_name)), text("(Intercept)"), num(beta[0]), num(se_int), num(t_int), num(t_sig_2tailed(t_int, df_e))]);

            let mut cur_col = 1;
            for e in &effects {
                let n_ec = e.cols.ncols();
                for cci in 0..n_ec {
                    let b_val = beta[cur_col];
                    let se = (mse * xtx_inv[[cur_col, cur_col]]).sqrt();
                    let t_val = b_val / se;
                    est_table.rows.push(vec![
                        text(self.display(dep_name)),
                        text(format!("{} [{}]", e.name, cci + 1)),
                        num(b_val),
                        num(se),
                        num(t_val),
                        num(t_sig_2tailed(t_val, df_e)),
                    ]);
                    cur_col += 1;
                }
            }
        }

        let inv_sqrt = sym_inv_sqrt(&e_mat).ok_or("Error matrix is singular")?;
        let s_mat = inv_sqrt.dot(&h_mat).dot(&inv_sqrt);
        let (mut eig, _) = jacobi_eigen(&s_mat);
        eig.mapv_inplace(|l| if l < 0.0 { 0.0 } else { l });
        let mut eig_vec = eig.to_vec();
        eig_vec.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

        let pillai: f64 = eig_vec.iter().map(|&l| l / (1.0 + l)).sum();
        let wilks: f64 = eig_vec.iter().map(|&l| 1.0 / (1.0 + l)).product();
        let hotelling: f64 = eig_vec.iter().sum();
        let roy = eig_vec[0];

        let mut mv = OutTable::new(
            "Multivariate Tests",
            vec!["Effect", "Value", "F", "Hypothesis df", "Error df", "Sig."],
        );
        let pf = p as f64;
        let df_h = (total_k - 1) as f64;
        let m = (pf - df_h).abs() / 2.0 - 0.5;
        let nn = (df_e - pf - 1.0) / 2.0;
        let s = pf.min(df_h);

        if s > 0.0 {
            let df1 = s * (2.0 * m + s + 1.0);
            let df2 = s * (2.0 * nn + s + 1.0);
            let f = ((2.0 * nn + s + 1.0) / (2.0 * m + s + 1.0)) * (pillai / (s - pillai));
            mv.rows.push(row_mv("Pillai's Trace", pillai, f, df1, df2));
        }
        {
            let r = df_e - (pf - df_h + 1.0) / 2.0;
            let denom = pf * pf + df_h * df_h - 5.0;
            let t = if denom > 0.0 { ((pf * pf * df_h * df_h - 4.0) / denom).sqrt() } else { 1.0 };
            let u = (pf * df_h - 2.0) / 4.0;
            let df1 = pf * df_h;
            let df2 = r * t - 2.0 * u;
            let lam_t = wilks.powf(1.0 / t);
            let f = ((1.0 - lam_t) / lam_t) * (df2 / df1);
            mv.rows.push(row_mv("Wilks' Lambda", wilks, f, df1, df2));
        }
        if s > 0.0 {
            let df1 = s * (2.0 * m + s + 1.0);
            let df2 = 2.0 * (s * nn + 1.0);
            let f = (df2 * hotelling) / (s * s * (2.0 * m + s + 1.0));
            mv.rows.push(row_mv("Hotelling's Trace", hotelling, f, df1, df2));
        }
        {
            let d = pf.max(df_h);
            let df1 = d;
            let df2 = df_e - d + df_h;
            let f = roy * df2 / df1;
            mv.rows.push(row_mv("Roy's Largest Root", roy, f, df1, df2));
        }

        Ok(Analysis {
            title: "GLM Multivariate (MANCOVA)".into(),
            tables: vec![mv, est_table],
        })
    }

    // ── Linear Mixed Model (random intercept) ─────────────────────────────────

    fn mixed_model_single(
        &self,
        df: &DataFrame,
        dependent: &str,
        subject: &str,
        covariates: &[String],
    ) -> SResult<Analysis> {
        let mut all_cols = vec![dependent.to_string(), subject.to_string()];
        all_cols.extend(covariates.iter().cloned());
        let clean_df = prep_data(df, &all_cols)?;

        let y: Vec<f64> = df_f64(&clean_df, dependent)?;
        let subj = col_labels(&clean_df, subject)?;
        
        let n = y.len();
        let p = 1 + covariates.len();
        
        let mut x_flat = Vec::with_capacity(n * p);
        x_flat.extend(std::iter::repeat(1.0).take(n));
        for c in covariates {
            x_flat.extend(df_f64(&clean_df, c)?);
        }
        let x = Array2::from_shape_vec((p, n), x_flat).map_err(map_err)?.reversed_axes(); // n x p

        if n <= p + 1 {
            return Err("Not enough complete cases".into());
        }

        // Group by subject.
        let mut order: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for i in 0..n {
            order.entry(subj[i].clone().unwrap()).or_default().push(i);
        }
        if order.len() < 2 {
            return Err(format!("'{subject}' must have at least 2 levels"));
        }

        struct GStat {
            ni: f64,
            xtx: Array2<f64>,
            xty: Array1<f64>,
            s: Array1<f64>,
            sy: f64,
            yty: f64,
        }
        let mut gstats: Vec<GStat> = Vec::new();
        for idxs in order.values() {
            let mut xtx = Array2::zeros((p, p));
            let mut xty = Array1::zeros(p);
            let mut s = Array1::zeros(p);
            let mut sy = 0.0;
            let mut yty = 0.0;
            for &i in idxs {
                sy += y[i];
                yty += y[i] * y[i];
                let xi = x.row(i);
                for a in 0..p {
                    s[a] += xi[a];
                    xty[a] += xi[a] * y[i];
                    for b in 0..p {
                        xtx[[a, b]] += xi[a] * xi[b];
                    }
                }
            }
            gstats.push(GStat { ni: idxs.len() as f64, xtx, xty, s, sy, yty });
        }

        let np = (n - p) as f64;
        let solve = |gamma: f64| -> Option<(Array1<f64>, Array2<f64>, f64, f64, f64)> {
            let mut a = Array2::zeros((p, p));
            let mut bvec = Array1::zeros(p);
            let mut ytvy = 0.0;
            let mut logdet_v = 0.0;
            for g in &gstats {
                let c = gamma / (1.0 + g.ni * gamma);
                logdet_v += (1.0 + g.ni * gamma).ln();
                ytvy += g.yty - c * g.sy * g.sy;
                for a_i in 0..p {
                    bvec[a_i] += g.xty[a_i] - c * g.s[a_i] * g.sy;
                    for b_i in 0..p {
                        a[[a_i, b_i]] += g.xtx[[a_i, b_i]] - c * g.s[a_i] * g.s[b_i];
                    }
                }
            }
            let ainv = invert(&a)?;
            let ld_a = log_det(&a)?;
            let beta = ainv.dot(&bvec);
            let rss = ytvy - beta.dot(&bvec);
            Some((beta, ainv, rss, ld_a, logdet_v))
        };

        let deviance = |gamma: f64| -> f64 {
            match solve(gamma) {
                Some((_, _, rss, ld_a, ld_v)) if rss > 0.0 => np * (rss / np).ln() + ld_v + ld_a,
                _ => f64::INFINITY,
            }
        };

        let gr = (5.0_f64.sqrt() - 1.0) / 2.0;
        let (mut lo, mut hi) = (-15.0_f64, 8.0_f64);
        let mut c1 = hi - gr * (hi - lo);
        let mut c2 = lo + gr * (hi - lo);
        let mut f1 = deviance(c1.exp());
        let mut f2 = deviance(c2.exp());
        for _ in 0..100 {
            if f1 < f2 {
                hi = c2;
                c2 = c1;
                f2 = f1;
                c1 = hi - gr * (hi - lo);
                f1 = deviance(c1.exp());
            } else {
                lo = c1;
                c1 = c2;
                f1 = f2;
                c2 = lo + gr * (hi - lo);
                f2 = deviance(c2.exp());
            }
        }
        let gamma_opt = (0.5 * (lo + hi)).exp();
        let gamma = if deviance(0.0) <= deviance(gamma_opt) { 0.0 } else { gamma_opt };

        let (beta, ainv, rss, ld_a, ld_v) =
            solve(gamma).ok_or("REML solve failed (collinear design)")?;
        let var_resid = rss / np;
        let var_subject = gamma * var_resid;
        let neg2ll = np * (rss / np).ln() + ld_v + ld_a + np * (1.0 + (2.0 * std::f64::consts::PI).ln());

        let mut fixed = OutTable::new(
            "Fixed Effects Estimates",
            vec!["", "Estimate", "Std. Error", "z", "Sig.", "95% CI Lower", "95% CI Upper"],
        )
        .footnote("REML estimation; significance from Wald z tests.");
        for a in 0..p {
            let se = (var_resid * ainv[[a, a]]).sqrt();
            let z = beta[a] / se;
            let label = if a == 0 { "(Intercept)".to_string() } else { self.display(&covariates[a - 1]) };
            fixed.rows.push(vec![
                text(label),
                num(beta[a]),
                num(se),
                num(z),
                num(z_sig_2tailed(z)),
                num(beta[a] - 1.959964 * se),
                num(beta[a] + 1.959964 * se),
            ]);
        }

        let mut vc = OutTable::new(
            "Covariance Parameters (REML)",
            vec!["Component", "Variance", "Std. Deviation"],
        );
        vc.rows.push(vec![
            text(format!("{} (intercept)", self.display(subject))),
            num(var_subject),
            num(var_subject.sqrt()),
        ]);
        vc.rows.push(vec![text("Residual"), num(var_resid), num(var_resid.sqrt())]);

        let mut info = OutTable::new("Model Information", vec!["Metric", "Value"]);
        info.rows.push(vec![text("ICC"), num(var_subject / (var_subject + var_resid))]);
        info.rows.push(vec![text("−2 Restricted Log Likelihood"), num(neg2ll)]);

        Ok(Analysis {
            title: "Linear Mixed Model (REML)".into(),
            tables: vec![fixed, vc, info],
        })
    }

    fn mixed_model(
        &self,
        df: &DataFrame,
        dependent: &str,
        random_factors: &[String],
        covariates: &[String],
    ) -> SResult<Analysis> {
        let mut all_cols = vec![dependent.to_string()];
        all_cols.extend(random_factors.iter().cloned());
        all_cols.extend(covariates.iter().cloned());
        let clean_df = prep_data(df, &all_cols)?;

        let y_vec: Vec<f64> = df_f64(&clean_df, dependent)?;
        let y = Array1::from_vec(y_vec);
        
        let n = y.len();
        let p = 1 + covariates.len();
        let q = random_factors.len();

        if q == 1 {
            return self.mixed_model_single(df, dependent, &random_factors[0], covariates);
        }

        let mut x_flat = Vec::with_capacity(n * p);
        x_flat.extend(std::iter::repeat(1.0).take(n));
        for c in covariates {
            x_flat.extend(df_f64(&clean_df, c)?);
        }
        let x = Array2::from_shape_vec((p, n), x_flat).map_err(map_err)?.reversed_axes();

        if n <= p + q {
            return Err("Not enough complete cases".into());
        }

        // Build Z matrices.
        let mut z: Vec<Array2<f64>> = Vec::new();
        for rf_name in random_factors {
            let labels = col_labels(&clean_df, rf_name)?;
            let unique: BTreeSet<String> = labels.iter().cloned().flatten().collect();
            let levels: Vec<String> = unique.into_iter().collect();
            let n_l = levels.len();
            let mut zj = Array2::zeros((n, n_l));
            for i in 0..n {
                let l_idx = levels.iter().position(|l| Some(l) == labels[i].as_ref()).unwrap();
                zj[[i, l_idx]] = 1.0;
            }
            z.push(zj);
        }

        let solve = |gammas: &[f64]| -> Option<(Array1<f64>, Array2<f64>, f64, f64, f64)> {
            let mut v = Array2::eye(n);
            v.mapv_inplace(|v| v + 1e-9); // Ridge
            for j in 0..q {
                let zj = &z[j];
                let g_zj = zj.clone() * gammas[j];
                v += &g_zj.dot(&zj.t());
            }
            let vinv = invert(&v)?;
            let logdet_v = log_det(&v)?;

            let xtvx = x.t().dot(&vinv).dot(&x);
            let xtvy = x.t().dot(&vinv).dot(&y);

            let ainv = invert(&xtvx)?;
            let ld_a = log_det(&xtvx)?;
            let beta = ainv.dot(&xtvy);

            let resid = &y - &x.dot(&beta);
            let rss = resid.dot(&vinv).dot(&resid);

            Some((beta, ainv, rss, ld_a, logdet_v))
        };

        let np = (n - p) as f64;
        let deviance = |gs: &[f64]| -> f64 {
            match solve(gs) {
                Some((_, _, rss, ld_a, ld_v)) if rss > 0.0 => np * (rss / np).ln() + ld_v + ld_a,
                _ => f64::INFINITY,
            }
        };

        let mut gammas = vec![0.0; q];
        for _ in 0..5 {
            for j in 0..q {
                let mut best_g = gammas[j];
                let mut best_d = deviance(&gammas);
                for &test_g in &[0.0, 0.01, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0] {
                    let old_g = gammas[j];
                    gammas[j] = test_g;
                    let d = deviance(&gammas);
                    if d < best_d {
                        best_d = d;
                        best_g = test_g;
                    } else {
                        gammas[j] = old_g;
                    }
                }
                gammas[j] = best_g;
            }
        }

        let (beta, ainv, rss, ld_a, ld_v) =
            solve(&gammas).ok_or("REML solve failed (collinear design)")?;
        let var_resid = rss / np;
        let neg2ll = np * (rss / np).ln() + ld_v + ld_a + np * (1.0 + (2.0 * std::f64::consts::PI).ln());

        let mut fixed = OutTable::new(
            "Fixed Effects Estimates",
            vec!["", "Estimate", "Std. Error", "z", "Sig.", "95% CI Lower", "95% CI Upper"],
        )
        .footnote("REML estimation; significance from Wald z tests.");
        for a in 0..p {
            let se = (var_resid * ainv[[a, a]]).sqrt();
            let z = beta[a] / se;
            let label = if a == 0 { "(Intercept)".to_string() } else { self.display(&covariates[a - 1]) };
            fixed.rows.push(vec![
                text(label),
                num(beta[a]),
                num(se),
                num(z),
                num(z_sig_2tailed(z)),
                num(beta[a] - 1.959964 * se),
                num(beta[a] + 1.959964 * se),
            ]);
        }

        let mut vc = OutTable::new(
            "Covariance Parameters (REML)",
            vec!["Component", "Variance", "Std. Deviation"],
        );
        for j in 0..q {
            let v_j = gammas[j] * var_resid;
            vc.rows.push(vec![
                text(format!("{} (intercept)", self.display(&random_factors[j]))),
                num(v_j),
                num(v_j.sqrt()),
            ]);
        }
        vc.rows.push(vec![text("Residual"), num(var_resid), num(var_resid.sqrt())]);

        let mut info = OutTable::new("Model Information", vec!["Metric", "Value"]);
        info.rows.push(vec![text("−2 Restricted Log Likelihood"), num(neg2ll)]);

        Ok(Analysis {
            title: "Linear Mixed Model (REML)".into(),
            tables: vec![fixed, vc, info],
        })
    }

    // ── Survival analysis (Kaplan-Meier + log-rank) ───────────────────────────

    fn survival_km(
        &self,
        df: &DataFrame,
        time: &str,
        status: &str,
        event_value: &str,
        factor: Option<&str>,
    ) -> SResult<Analysis> {
        let times = col_opt(df, time)?;
        let stat_labels = col_labels(df, status)?;
        let group_labels = match factor {
            Some(f) => Some(col_labels(df, f)?),
            None => None,
        };
        let nrows = df.height();

        // (time, event?, group) tuples after listwise deletion.
        let mut obs: Vec<(f64, bool, String)> = Vec::new();
        for i in 0..nrows {
            let (Some(t), Some(s)) = (times[i], stat_labels[i].clone()) else {
                continue;
            };
            let grp = match &group_labels {
                Some(g) => match &g[i] {
                    Some(l) => l.clone(),
                    None => continue,
                },
                None => "All".to_string(),
            };
            obs.push((t, s == event_value, grp));
        }
        if obs.len() < 2 {
            return Err("Not enough valid cases".into());
        }

        let mut tables = Vec::new();
        let mut groups: BTreeMap<String, Vec<(f64, bool)>> = BTreeMap::new();
        for (t, ev, g) in &obs {
            groups.entry(g.clone()).or_default().push((*t, *ev));
        }

        // Kaplan-Meier table + median per group.
        let mut summary = OutTable::new(
            "Survival Summary",
            vec!["Group", "N", "Events", "Censored", "Median Survival"],
        );
        for (g, data) in &groups {
            let km = self.km_table(g, data);
            tables.push(km.0);
            let events = data.iter().filter(|(_, e)| *e).count();
            summary.rows.push(vec![
                text(g),
                int(data.len() as i64),
                int(events as i64),
                int((data.len() - events) as i64),
                km.1.map(num).unwrap_or(JsonValue::Null),
            ]);
        }
        tables.insert(0, summary);

        // Log-rank test across groups.
        if groups.len() >= 2 {
            tables.push(log_rank(&groups)?);
        }

        Ok(Analysis {
            title: format!("Kaplan-Meier — {}", self.display(time)),
            tables,
        })
    }

    /// Build a Kaplan-Meier survival table for one group; returns (table, median).
    fn km_table(&self, group: &str, data: &[(f64, bool)]) -> (OutTable, Option<f64>) {
        let mut d = data.to_vec();
        d.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut t = OutTable::new(
            format!("Survival Table — {group}"),
            vec!["Time", "N at Risk", "Events", "Survival", "Std. Error"],
        );
        let mut surv = 1.0;
        let mut var_sum = 0.0; // Greenwood accumulation
        let mut median = None;
        let total = d.len();
        let mut idx = 0;
        while idx < total {
            let time = d[idx].0;
            let n_risk = (total - idx) as f64;
            // Count events and total records at this exact time.
            let mut events = 0.0;
            let mut tied = 0;
            while idx + tied < total && d[idx + tied].0 == time {
                if d[idx + tied].1 {
                    events += 1.0;
                }
                tied += 1;
            }
            if events > 0.0 {
                surv *= 1.0 - events / n_risk;
                var_sum += events / (n_risk * (n_risk - events));
                let se = surv * var_sum.sqrt();
                t.rows.push(vec![
                    num(time),
                    num(n_risk),
                    num(events),
                    num(surv),
                    num(se),
                ]);
                if median.is_none() && surv <= 0.5 {
                    median = Some(time);
                }
            }
            idx += tied;
        }
        (t, median)
    }

    // ── Cox proportional-hazards regression ───────────────────────────────────

    fn cox_regression(
        &self,
        df: &DataFrame,
        time: &str,
        start_time: Option<&str>,
        status: &str,
        event_value: &str,
        covariates: &[String],
    ) -> SResult<Analysis> {
        let p = covariates.len();
        if p == 0 {
            return Err("Cox regression needs at least one covariate".into());
        }

        // Optimized data preparation.
        let mut all_cols = vec![time.to_string(), status.to_string()];
        if let Some(s) = start_time {
            all_cols.push(s.to_string());
        }
        all_cols.extend(covariates.iter().cloned());
        let clean_df = prep_data(df, &all_cols)?;

        let t_stop: Vec<f64> = df_f64(&clean_df, time)?;
        let t_start: Vec<f64> = match start_time {
            Some(s) => df_f64(&clean_df, s)?,
            None => vec![0.0; t_stop.len()],
        };
        let ev: Vec<bool> = col_labels(&clean_df, status)?.into_iter().map(|o| o == Some(event_value.into())).collect();
        
        let mut x_flat = Vec::with_capacity(t_stop.len() * p);
        for c in covariates {
            x_flat.extend(df_f64(&clean_df, c)?);
        }
        let x = Array2::from_shape_vec((p, t_stop.len()), x_flat).map_err(map_err)?.reversed_axes(); // n x p

        let n = t_stop.len();
        let n_events = ev.iter().filter(|&&e| e).count();
        if n_events == 0 {
            return Err("No events (uncensored cases) found".into());
        }
        if n <= p + 1 {
            return Err("Not enough complete cases".into());
        }

        // Centre covariates.
        let means = x.mean_axis(ndarray::Axis(0)).ok_or_else(|| "Empty data".to_string())?;
        let x_centered = &x - &means;

        let mut event_times: Vec<f64> = Vec::new();
        for i in 0..n {
            if ev[i] {
                event_times.push(t_stop[i]);
            }
        }
        event_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        event_times.dedup_by(|a, b| (*a - *b).abs() < 1e-10);

        let eval = |beta_vec: &[f64]| -> (f64, Vec<f64>, Array2<f64>) {
            let beta = Array1::from_vec(beta_vec.to_vec());
            let etas = x_centered.dot(&beta);
            let exp_etas = etas.mapv(|e| e.exp());

            let results: Vec<(f64, Vec<f64>, Array2<f64>)> = event_times.par_iter().map(|&et| {
                let mut s0 = 0.0;
                let mut s1: Array1<f64> = Array1::zeros(p);
                let mut s2: Array2<f64> = Array2::zeros((p, p));
                let mut d_events = 0;
                let mut d_score: Array1<f64> = Array1::zeros(p);
                let mut d_eta = 0.0;

                for i in 0..n {
                    if t_start[i] < et && t_stop[i] >= et {
                        let w = exp_etas[i];
                        s0 += w;
                        for a in 0..p {
                            let w_xa = w * x_centered[[i, a]];
                            s1[a] += w_xa;
                            for b in 0..p {
                                s2[[a, b]] += w_xa * x_centered[[i, b]];
                            }
                        }
                        if ev[i] && (t_stop[i] - et).abs() < 1e-10 {
                            d_events += 1;
                            d_eta += etas[i];
                            for a in 0..p {
                                d_score[a] += x_centered[[i, a]];
                            }
                        }
                    }
                }

                if d_events > 0 {
                    let ll = d_eta - (d_events as f64) * s0.ln();
                    let mut score: Array1<f64> = Array1::zeros(p);
                    let mut info: Array2<f64> = Array2::zeros((p, p));
                    let ea = &s1 / s0;
                    for a in 0..p {
                        score[a] = d_score[a] - (d_events as f64) * ea[a];
                        for b in 0..p {
                            info[[a, b]] = (d_events as f64) * (s2[[a, b]] / s0 - ea[a] * ea[b]);
                        }
                    }
                    (ll, score.to_vec(), info)
                } else {
                    (0.0, vec![0.0; p], Array2::zeros((p, p)))
                }
            }).collect();

            let mut total_ll = 0.0;
            let mut total_score = vec![0.0; p];
            let mut total_info = Array2::zeros((p, p));
            for (ll, score, info) in results {
                total_ll += ll;
                for a in 0..p {
                    total_score[a] += score[a];
                }
                total_info += &info;
            }
            (total_ll, total_score, total_info)
        };

        // Newton-Raphson.
        let mut beta = vec![0.0; p];
        let ll0 = eval(&vec![0.0; p]).0;
        let mut ll = ll0;
        let mut last_info = Array2::zeros((p, p));
        for _ in 0..50 {
            let (cur_ll, score, info) = eval(&beta);
            ll = cur_ll;
            last_info = info.clone();
            let inv = invert(&info).ok_or("Information matrix is singular (collinear covariates)")?;
            let mut max_step = 0.0_f64;
            for a in 0..p {
                let mut step_a = 0.0;
                for b in 0..p {
                    step_a += inv[[a, b]] * score[b];
                }
                beta[a] += step_a;
                max_step = max_step.max(step_a.abs());
            }
            if max_step < 1e-8 {
                break;
            }
        }
        let inv = invert(&last_info).ok_or("Information matrix is singular")?;

        // Omnibus likelihood-ratio test.
        let lr = 2.0 * (ll - ll0);
        let mut omni = OutTable::new(
            "Omnibus Tests of Model Coefficients",
            vec!["-2 Log Likelihood", "Chi-Square (LR)", "df", "Sig."],
        );
        omni.rows.push(vec![
            num(-2.0 * ll),
            num(lr),
            num(p as f64),
            num(chi2_sig(lr, p as f64)),
        ]);

        // Variables in the Equation.
        let mut vars = OutTable::new(
            "Variables in the Equation",
            vec![
                "",
                "B",
                "SE",
                "Wald",
                "df",
                "Sig.",
                "Exp(B)",
                "95% CI Lower",
                "95% CI Upper",
            ],
        );
        for j in 0..p {
            let se = inv[[j, j]].sqrt();
            let wald = (beta[j] / se).powi(2);
            vars.rows.push(vec![
                text(self.display(&covariates[j])),
                num(beta[j]),
                num(se),
                num(wald),
                int(1),
                num(chi2_sig(wald, 1.0)),
                num(beta[j].exp()),
                num((beta[j] - 1.959964 * se).exp()),
                num((beta[j] + 1.959964 * se).exp()),
            ]);
        }

        Ok(Analysis {
            title: format!("Cox Regression — {}", self.display(time)),
            tables: vec![omni, vars],
        })
    }

    // ── Repeated-measures GLM (within-subjects ANOVA) ─────────────────────────

    fn glm_repeated(&self, df: &DataFrame, vars: &[String]) -> SResult<Analysis> {
        let k = vars.len();
        if k < 2 {
            return Err("Select at least 2 within-subjects levels".into());
        }
        // Listwise-deleted subject × condition matrix.
        let cols: Vec<Vec<Option<f64>>> =
            vars.iter().map(|v| col_opt(df, v)).collect::<SResult<_>>()?;
        let nrows = df.height();
        let mut data: Vec<Vec<f64>> = Vec::new();
        for i in 0..nrows {
            if cols.iter().all(|c| c[i].is_some()) {
                data.push(cols.iter().map(|c| c[i].unwrap()).collect());
            }
        }
        let n = data.len();
        if n < 2 {
            return Err("Not enough complete cases".into());
        }
        let nf = n as f64;
        let kf = k as f64;

        let grand = data.iter().flat_map(|r| r.iter()).sum::<f64>() / (nf * kf);
        let cond_means: Vec<f64> = (0..k).map(|j| data.iter().map(|r| r[j]).sum::<f64>() / nf).collect();
        let subj_means: Vec<f64> = data.iter().map(|r| r.iter().sum::<f64>() / kf).collect();

        let ss_cond = nf * cond_means.iter().map(|m| (m - grand).powi(2)).sum::<f64>();
        let ss_subj = kf * subj_means.iter().map(|m| (m - grand).powi(2)).sum::<f64>();
        let mut ss_total = 0.0;
        for row in &data {
            for j in 0..k {
                ss_total += (row[j] - grand).powi(2);
            }
        }
        let ss_error = ss_total - ss_cond - ss_subj;
        let df_cond = kf - 1.0;
        let df_error = (kf - 1.0) * (nf - 1.0);
        let ms_cond = ss_cond / df_cond;
        let ms_error = ss_error / df_error;
        let f = ms_cond / ms_error;

        // Greenhouse-Geisser epsilon from the covariance matrix of the levels.
        let eps = greenhouse_geisser(&data, &cond_means);
        let gg_df1 = df_cond * eps;
        let gg_df2 = df_error * eps;

        let mut t = OutTable::new(
            "Tests of Within-Subjects Effects",
            vec!["Source", "Sum of Squares", "df", "Mean Square", "F", "Sig."],
        );
        t.rows.push(vec![
            text("Factor (Sphericity Assumed)"),
            num(ss_cond),
            num(df_cond),
            num(ms_cond),
            num(f),
            num(f_sig(f, df_cond, df_error)),
        ]);
        t.rows.push(vec![
            text("Factor (Greenhouse-Geisser)"),
            num(ss_cond),
            num(gg_df1),
            num(ss_cond / gg_df1),
            num(f),
            num(f_sig(f, gg_df1, gg_df2)),
        ]);
        t.rows.push(vec![
            text("Error (Sphericity Assumed)"),
            num(ss_error),
            num(df_error),
            num(ms_error),
            JsonValue::Null,
            JsonValue::Null,
        ]);
        let t = t.footnote(format!("Greenhouse-Geisser ε = {eps:.3}. n = {n} subjects, {k} levels."));

        // Within-subjects descriptives.
        let mut desc = OutTable::new("Descriptive Statistics", vec!["", "Mean", "N"]);
        for (j, v) in vars.iter().enumerate() {
            desc.rows.push(vec![text(self.display(v)), num(cond_means[j]), int(n as i64)]);
        }

        Ok(Analysis {
            title: "Repeated Measures ANOVA".into(),
            tables: vec![desc, t],
        })
    }

    // ── Charts ────────────────────────────────────────────────────────────────

    /// Compute chart data for the Graphs menu. Aggregation happens here so the
    /// renderer only draws (and large datasets don't cross the boundary raw).
    pub fn run_chart(&self, kind: &str, params: &JsonValue) -> SResult<ChartData> {
        let df = self.df.as_ref().ok_or("No dataset loaded")?;
        match kind {
            "histogram" => self.chart_histogram(df, &p_str(params, "var")?),
            "bar" => self.chart_bar(df, &p_str(params, "var")?),
            "scatter" => {
                self.chart_scatter(df, &p_str(params, "x")?, &p_str(params, "y")?)
            }
            "box" => self.chart_box(
                df,
                &p_strs(params, "vars")?,
                p_str_opt(params, "group").as_deref(),
            ),
            "line" => self.chart_line(df, &p_strs(params, "vars")?),
            "clustered_bar" => {
                self.chart_clustered_bar(df, &p_str(params, "var")?, &p_str(params, "cluster")?)
            }
            other => Err(format!("Unknown chart: {other}")),
        }
    }

    fn chart_histogram(&self, df: &DataFrame, var: &str) -> SResult<ChartData> {
        let x = col_valid(df, var)?;
        if x.len() < 2 {
            return Err(format!("'{var}' has too few valid values"));
        }
        let d = describe(&x);
        let value_labels = self.var_meta.get(var).and_then(|m| m.value_labels.clone());
        let (min, max) = (d.min, d.max);
        // Sturges' rule, clamped to a sensible range.
        let bin_count = ((x.len() as f64).log2().ceil() as usize + 1).clamp(5, 40);
        let width = ((max - min) / bin_count as f64).max(f64::MIN_POSITIVE);
        let mut counts = vec![0usize; bin_count];
        for &v in &x {
            let mut b = ((v - min) / width).floor() as usize;
            if b >= bin_count {
                b = bin_count - 1;
            }
            counts[b] += 1;
        }
        let bins: Vec<JsonValue> = counts
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                serde_json::json!({
                    "x0": min + i as f64 * width,
                    "x1": min + (i as f64 + 1.0) * width,
                    "count": c,
                })
            })
            .collect();
        Ok(ChartData {
            title: format!("Histogram of {}", self.display(var)),
            kind: "histogram".into(),
            x_label: self.display(var),
            y_label: "Frequency".into(),
            payload: serde_json::json!({
                "bins": bins, 
                "mean": num(d.mean), 
                "sd": num(d.sd), 
                "n": d.n,
                "valueLabels": value_labels,
            }),
        })
    }

    fn chart_bar(&self, df: &DataFrame, var: &str) -> SResult<ChartData> {
        let labels = col_labels(df, var)?;
        let value_labels = self.var_meta.get(var).and_then(|m| m.value_labels.clone());
        let mut counts: BTreeMap<OrderKey, usize> = BTreeMap::new();
        for l in labels.iter().flatten() {
            *counts.entry(OrderKey::from(l.as_str())).or_insert(0) += 1;
        }
        let categories: Vec<JsonValue> = counts
            .iter()
            .map(|(k, &c)| {
                let raw = k.label();
                let shown = value_labels
                    .as_ref()
                    .and_then(|m| m.get(&raw).cloned())
                    .unwrap_or(raw);
                serde_json::json!({ "label": shown, "count": c })
            })
            .collect();
        Ok(ChartData {
            title: format!("Bar Chart of {}", self.display(var)),
            kind: "bar".into(),
            x_label: self.display(var),
            y_label: "Count".into(),
            payload: serde_json::json!({ "categories": categories }),
        })
    }

    fn chart_scatter(&self, df: &DataFrame, x: &str, y: &str) -> SResult<ChartData> {
        let xs = col_opt(df, x)?;
        let ys = col_opt(df, y)?;
        let mut xv = Vec::new();
        let mut yv = Vec::new();
        for i in 0..xs.len() {
            if let (Some(a), Some(b)) = (xs[i], ys[i]) {
                xv.push(a);
                yv.push(b);
            }
        }
        if xv.len() < 2 {
            return Err("Need at least 2 complete (x, y) pairs".into());
        }
        // Cap points crossing the boundary; even-stride sample if very large.
        const CAP: usize = 5000;
        let stride = (xv.len() / CAP).max(1);
        let points: Vec<JsonValue> = (0..xv.len())
            .step_by(stride)
            .map(|i| serde_json::json!([xv[i], yv[i]]))
            .collect();
        // Least-squares fit line for an overlay.
        let r = pearson(&xv, &yv);
        let dx = describe(&xv);
        let dy = describe(&yv);
        let slope = r * dy.sd / dx.sd;
        let intercept = dy.mean - slope * dx.mean;
        Ok(ChartData {
            title: format!("{} vs {}", self.display(y), self.display(x)),
            kind: "scatter".into(),
            x_label: self.display(x),
            y_label: self.display(y),
            payload: serde_json::json!({
                "points": points,
                "fit": { "slope": num(slope), "intercept": num(intercept) },
                "r": num(r),
                "xMin": dx.min, "xMax": dx.max,
            }),
        })
    }

    fn chart_box(
        &self,
        df: &DataFrame,
        vars: &[String],
        group: Option<&str>,
    ) -> SResult<ChartData> {
        // Each "box" is either one variable, or one (variable × group level).
        let mut boxes: Vec<JsonValue> = Vec::new();
        let group_labels = match group {
            Some(g) => Some(col_labels(df, g)?),
            None => None,
        };
        for v in vars {
            let xs = col_opt(df, v)?;
            match &group_labels {
                None => {
                    let data: Vec<f64> = xs.iter().flatten().copied().collect();
                    if let Some(b) = box_summary(&self.display(v), &data) {
                        boxes.push(b);
                    }
                }
                Some(glab) => {
                    let mut grouped: BTreeMap<String, Vec<f64>> = BTreeMap::new();
                    for (i, l) in glab.iter().enumerate() {
                        if let (Some(l), Some(val)) = (l, xs[i]) {
                            grouped.entry(l.clone()).or_default().push(val);
                        }
                    }
                    for (lev, data) in grouped {
                        let label = if vars.len() > 1 {
                            format!("{} ({})", self.display(v), lev)
                        } else {
                            lev
                        };
                        if let Some(b) = box_summary(&label, &data) {
                            boxes.push(b);
                        }
                    }
                }
            }
        }
        if boxes.is_empty() {
            return Err("No valid data to plot".into());
        }
        let title = match group {
            Some(g) => format!("Boxplot by {}", self.display(g)),
            None => "Boxplot".into(),
        };
        Ok(ChartData {
            title,
            kind: "box".into(),
            x_label: String::new(),
            y_label: "Value".into(),
            payload: serde_json::json!({ "boxes": boxes }),
        })
    }

    fn chart_line(&self, df: &DataFrame, vars: &[String]) -> SResult<ChartData> {
        // One series per variable, plotted against case order (sampled if large).
        const CAP: usize = 3000;
        let mut series = Vec::new();
        for v in vars {
            let xs = col_opt(df, v)?;
            let stride = (xs.len() / CAP).max(1);
            let points: Vec<JsonValue> = (0..xs.len())
                .step_by(stride)
                .filter_map(|i| xs[i].map(|y| serde_json::json!([i as f64 + 1.0, y])))
                .collect();
            series.push(serde_json::json!({ "label": self.display(v), "points": points }));
        }
        if series.is_empty() {
            return Err("Select at least one variable".into());
        }
        Ok(ChartData {
            title: if vars.len() == 1 {
                format!("Line Chart of {}", self.display(&vars[0]))
            } else {
                "Line Chart".into()
            },
            kind: "line".into(),
            x_label: "Case".into(),
            y_label: "Value".into(),
            payload: serde_json::json!({ "series": series }),
        })
    }

    fn chart_clustered_bar(&self, df: &DataFrame, var: &str, cluster: &str) -> SResult<ChartData> {
        // Counts for each (category × cluster) cell; categories on the X axis,
        // clusters as colour-coded series.
        let cat_labels = col_labels(df, var)?;
        let clus_labels = col_labels(df, cluster)?;
        let cat_vl = self.var_meta.get(var).and_then(|m| m.value_labels.clone());
        let clus_vl = self.var_meta.get(cluster).and_then(|m| m.value_labels.clone());
        let cats = ordered_levels(&cat_labels);
        let clusters = ordered_levels(&clus_labels);
        if cats.is_empty() || clusters.is_empty() {
            return Err("Both variables need at least one category".into());
        }
        let mut counts = vec![vec![0i64; clusters.len()]; cats.len()];
        for i in 0..cat_labels.len() {
            if let (Some(c), Some(g)) = (&cat_labels[i], &clus_labels[i]) {
                let ci = cats.iter().position(|x| x == c).unwrap();
                let gi = clusters.iter().position(|x| x == g).unwrap();
                counts[ci][gi] += 1;
            }
        }
        let label_for = |raw: &str, vl: &Option<std::collections::HashMap<String, String>>| {
            vl.as_ref().and_then(|m| m.get(raw).cloned()).unwrap_or_else(|| raw.to_string())
        };
        let categories: Vec<JsonValue> = cats.iter().map(|c| text(label_for(c, &cat_vl))).collect();
        let series: Vec<JsonValue> = clusters
            .iter()
            .enumerate()
            .map(|(gi, g)| {
                let vals: Vec<i64> = (0..cats.len()).map(|ci| counts[ci][gi]).collect();
                serde_json::json!({ "label": label_for(g, &clus_vl), "counts": vals })
            })
            .collect();
        Ok(ChartData {
            title: format!("{} by {}", self.display(var), self.display(cluster)),
            kind: "clustered_bar".into(),
            x_label: self.display(var),
            y_label: "Count".into(),
            payload: serde_json::json!({ "categories": categories, "series": series }),
        })
    }
}

// ── Shared numeric helpers ───────────────────────────────────────────────────

/// Levene's test for two groups: one-way ANOVA on absolute deviations from each
/// group's mean. Returns (F, Sig) with df1 = 1, df2 = n1 + n2 − 2.
fn levene_two(a: &[f64], b: &[f64], mean_a: f64, mean_b: f64) -> (f64, f64) {
    let za: Vec<f64> = a.iter().map(|x| (x - mean_a).abs()).collect();
    let zb: Vec<f64> = b.iter().map(|x| (x - mean_b).abs()).collect();
    let (n1, n2) = (za.len() as f64, zb.len() as f64);
    let mza = za.iter().sum::<f64>() / n1;
    let mzb = zb.iter().sum::<f64>() / n2;
    let grand = (za.iter().sum::<f64>() + zb.iter().sum::<f64>()) / (n1 + n2);
    let ss_between = n1 * (mza - grand).powi(2) + n2 * (mzb - grand).powi(2);
    let ss_within: f64 = za.iter().map(|z| (z - mza).powi(2)).sum::<f64>()
        + zb.iter().map(|z| (z - mzb).powi(2)).sum::<f64>();
    let df1 = 1.0;
    let df2 = n1 + n2 - 2.0;
    if ss_within == 0.0 {
        return (f64::NAN, f64::NAN);
    }
    let f = (ss_between / df1) / (ss_within / df2);
    (f, f_sig(f, df1, df2))
}

/// Linear-interpolation quantile (type 7) of an already-sorted slice.
fn quantile_sorted(sorted: &[f64], q: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n == 1 {
        return sorted[0];
    }
    let pos = q * (n as f64 - 1.0);
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    sorted[lo] + frac * (sorted[hi] - sorted[lo])
}

/// Five-number summary + 1.5·IQR outliers for one box, as JSON. None if empty.
fn box_summary(label: &str, data: &[f64]) -> Option<JsonValue> {
    if data.is_empty() {
        return None;
    }
    let mut s = data.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let q1 = quantile_sorted(&s, 0.25);
    let median = quantile_sorted(&s, 0.5);
    let q3 = quantile_sorted(&s, 0.75);
    let iqr = q3 - q1;
    let lo_fence = q1 - 1.5 * iqr;
    let hi_fence = q3 + 1.5 * iqr;
    // Whiskers reach the most extreme non-outlier values.
    let whisker_lo = s.iter().copied().find(|&v| v >= lo_fence).unwrap_or(s[0]);
    let whisker_hi = s
        .iter()
        .rev()
        .copied()
        .find(|&v| v <= hi_fence)
        .unwrap_or(s[s.len() - 1]);
    let outliers: Vec<f64> = s
        .iter()
        .copied()
        .filter(|&v| v < lo_fence || v > hi_fence)
        .collect();
    Some(serde_json::json!({
        "label": label,
        "min": whisker_lo,
        "q1": q1,
        "median": median,
        "q3": q3,
        "max": whisker_hi,
        "outliers": outliers,
        "n": data.len(),
    }))
}

/// Distinct category labels in canonical order (numeric values numerically,
/// strings lexically, numbers before strings) — shared by crosstabs.
fn ordered_levels(labels: &[Option<String>]) -> Vec<String> {
    let mut set: std::collections::BTreeSet<OrderKey> = std::collections::BTreeSet::new();
    for l in labels.iter().flatten() {
        set.insert(OrderKey::from(l.as_str()));
    }
    set.into_iter().map(|k| k.label()).collect()
}

/// Jacobi eigenvalue algorithm for a real symmetric matrix. Returns
/// (eigenvalues, eigenvectors) where eigenvectors[i][j] is the i-th component of
/// the j-th eigenvector (columns are eigenvectors).
fn jacobi_eigen(input: &Array2<f64>) -> (Array1<f64>, Array2<f64>) {
    let n = input.nrows();
    let mut a = input.clone();
    let mut v = Array2::eye(n);

    for _sweep in 0..100 {
        let mut off = 0.0;
        for i in 0..n {
            for j in (i + 1)..n {
                off += a[[i, j]].abs();
            }
        }
        if off < 1e-12 {
            break;
        }
        for p in 0..n {
            for q in (p + 1)..n {
                if a[[p, q]].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[[q, q]] - a[[p, p]]) / (2.0 * a[[p, q]]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;

                for i in 0..n {
                    let aip = a[[i, p]];
                    let aiq = a[[i, q]];
                    a[[i, p]] = c * aip - s * aiq;
                    a[[i, q]] = s * aip + c * aiq;
                }
                for i in 0..n {
                    let api = a[[p, i]];
                    let aqi = a[[q, i]];
                    a[[p, i]] = c * api - s * aqi;
                    a[[q, i]] = s * api + c * aqi;
                }
                for i in 0..n {
                    let vip = v[[i, p]];
                    let viq = v[[i, q]];
                    v[[i, p]] = c * vip - s * viq;
                    v[[i, q]] = s * vip + c * viq;
                }
            }
        }
    }
    let eigvals = Array1::from_iter((0..n).map(|i| a[[i, i]]));
    (eigvals, v)
}

/// Kaiser varimax rotation of a loadings matrix (p vars × m components).
fn varimax(loadings_in: &Array2<f64>) -> Array2<f64> {
    let p = loadings_in.nrows();
    let m = loadings_in.ncols();
    let mut l = loadings_in.clone();
    for _ in 0..50 {
        let mut converged = true;
        for c1 in 0..m {
            for c2 in (c1 + 1)..m {
                let (mut u_sum, mut v_sum, mut u2v2, mut uv2) = (0.0, 0.0, 0.0, 0.0);
                for i in 0..p {
                    let x = l[[i, c1]];
                    let y = l[[i, c2]];
                    let u = x * x - y * y;
                    let v = 2.0 * x * y;
                    u_sum += u;
                    v_sum += v;
                    u2v2 += u * u - v * v;
                    uv2 += 2.0 * u * v;
                }
                let num = uv2 - 2.0 * u_sum * v_sum / p as f64;
                let den = u2v2 - (u_sum * u_sum - v_sum * v_sum) / p as f64;
                if den.abs() < 1e-12 && num.abs() < 1e-12 {
                    continue;
                }
                let angle = 0.25 * num.atan2(den);
                if angle.abs() > 1e-6 {
                    converged = false;
                }
                let (cos, sin) = (angle.cos(), angle.sin());
                for i in 0..p {
                    let x = l[[i, c1]];
                    let y = l[[i, c2]];
                    l[[i, c1]] = cos * x + sin * y;
                    l[[i, c2]] = -sin * x + cos * y;
                }
            }
        }
        if converged {
            break;
        }
    }
    l
}

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

/// A named GLM effect carrying its own design columns (n rows × k columns).
struct Effect {
    name: String,
    cols: Array2<f64>,
}

/// Greenhouse-Geisser sphericity correction ε (Box's formula) from repeated-
/// measures data (`data[subject][level]`). Clamped to [1/(k−1), 1].
fn greenhouse_geisser(data: &[Vec<f64>], means: &[f64]) -> f64 {
    let n = data.len() as f64;
    let k = means.len();
    if k < 2 || n < 2.0 {
        return 1.0;
    }
    // Sample covariance matrix S (k × k).
    let mut s = Array2::zeros((k, k));
    for row in data {
        for a in 0..k {
            for b in 0..k {
                s[[a, b]] += (row[a] - means[a]) * (row[b] - means[b]);
            }
        }
    }
    s.mapv_inplace(|v| v / (n - 1.0));

    let kf = k as f64;
    let diag_mean: f64 = s.diag().mean().unwrap();
    let grand: f64 = s.mean().unwrap();
    // k ≥ 2 is guaranteed above, so the covariance matrix is non-empty.
    let row_means = s.mean_axis(ndarray::Axis(1)).unwrap();
    let sum_sq = s.mapv(|v| v * v).sum();
    let sum_row_sq = row_means.mapv(|v| v * v).sum();

    let numer: f64 = kf * kf * (diag_mean - grand).powi(2);
    let denom: f64 = (kf - 1.0) * (sum_sq - 2.0 * kf * sum_row_sq + kf * kf * grand * grand);
    if denom <= 0.0 {
        return 1.0;
    }
    (numer / denom).clamp(1.0 / (kf - 1.0), 1.0)
}

/// One row of a MANOVA "Multivariate Tests" table.
fn row_mv(name: &str, value: f64, f: f64, df1: f64, df2: f64) -> Vec<JsonValue> {
    vec![
        text(name),
        num(value),
        num(f),
        num(df1),
        num(df2),
        num(f_sig(f, df1, df2)),
    ]
}

/// Log-rank (Mantel-Cox) test across `groups` of (time, event) data. Builds the
/// observed/expected table and the overall chi-square via the (k−1) covariance.
fn log_rank(groups: &BTreeMap<String, Vec<(f64, bool)>>) -> SResult<OutTable> {
    let names: Vec<&String> = groups.keys().collect();
    let g = names.len();
    let data: Vec<&Vec<(f64, bool)>> = names.iter().map(|n| &groups[*n]).collect();

    // All distinct event times across groups.
    let mut event_times: Vec<f64> = groups
        .values()
        .flat_map(|d| d.iter().filter(|(_, e)| *e).map(|(t, _)| *t))
        .collect();
    event_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    event_times.dedup();

    let mut observed = vec![0.0; g];
    let mut expected = vec![0.0; g];
    // (k−1) × (k−1) covariance of (O − E).
    let mut cov = Array2::zeros((g - 1, g - 1));

    for &et in &event_times {
        let at_risk: Vec<f64> = data
            .iter()
            .map(|d| d.iter().filter(|(t, _)| *t >= et).count() as f64)
            .collect();
        let d_grp: Vec<f64> = data
            .iter()
            .map(|d| d.iter().filter(|(t, e)| *e && *t == et).count() as f64)
            .collect();
        let n: f64 = at_risk.iter().sum();
        let dt: f64 = d_grp.iter().sum();
        if n <= 1.0 {
            continue;
        }
        for i in 0..g {
            observed[i] += d_grp[i];
            expected[i] += dt * at_risk[i] / n;
        }
        let factor = dt * (n - dt) / (n - 1.0);
        for i in 0..(g - 1) {
            for j in 0..(g - 1) {
                let kron = if i == j { 1.0 } else { 0.0 };
                cov[[i, j]] += factor * (kron * at_risk[i] / n - at_risk[i] * at_risk[j] / (n * n));
            }
        }
    }

    let diff: Vec<f64> = (0..(g - 1)).map(|i| observed[i] - expected[i]).collect();
    let chi = match invert(&cov) {
        Some(ci) => {
            let mut q = 0.0;
            for i in 0..(g - 1) {
                for j in 0..(g - 1) {
                    q += diff[i] * ci[[i, j]] * diff[j];
                }
            }
            q
        }
        None => f64::NAN,
    };
    let dfree = (g - 1) as f64;

    let mut t = OutTable::new(
        "Overall Comparisons (Log Rank)",
        vec!["", "Observed", "Expected", "Chi-Square", "df", "Sig."],
    );
    for i in 0..g {
        t.rows.push(vec![
            text(names[i]),
            num(observed[i]),
            num(expected[i]),
            if i == 0 { num(chi) } else { JsonValue::Null },
            if i == 0 { num(dfree) } else { JsonValue::Null },
            if i == 0 { num(chi2_sig(chi, dfree)) } else { JsonValue::Null },
        ]);
    }
    Ok(t)
}

/// Matrix product A·B.
fn mat_mul(a: &Array2<f64>, b: &Array2<f64>) -> Array2<f64> {
    a.dot(b)
}

/// Symmetric inverse square root E^(−1/2) via eigen-decomposition.
fn sym_inv_sqrt(e: &Array2<f64>) -> Option<Array2<f64>> {
    let (vals, vecs) = jacobi_eigen(e);
    let n = e.nrows();
    if vals.iter().any(|&l| l <= 1e-12) {
        return None;
    }
    let mut out = Array2::zeros((n, n));
    for a in 0..n {
        for b in 0..n {
            let mut s = 0.0;
            for j in 0..n {
                s += vecs[[a, j]] * vecs[[b, j]] / vals[j].sqrt();
            }
            out[[a, b]] = s;
        }
    }
    Some(out)
}

/// OLS residual sum of squares.
fn ols_sse(x: &Array2<f64>, y: &Array1<f64>) -> Option<f64> {
    let xtx = x.t().dot(x);
    let xtx_inv = invert(&xtx)?;
    let beta = xtx_inv.dot(&x.t().dot(y));
    let resid = y - &x.dot(&beta);
    Some(resid.dot(&resid))
}

/// Natural log of |det(A)| via Gaussian elimination with partial pivoting.
fn log_det(a_in: &Array2<f64>) -> Option<f64> {
    let n = a_in.nrows();
    let mut a = a_in.clone();
    let mut ld = 0.0;
    for col in 0..n {
        let mut pivot = col;
        for r in (col + 1)..n {
            if a[[r, col]].abs() > a[[pivot, col]].abs() {
                pivot = r;
            }
        }
        if a[[pivot, col]].abs() < 1e-12 {
            return None;
        }
        // Swap rows.
        for j in col..n {
            let tmp = a[[col, j]];
            a[[col, j]] = a[[pivot, j]];
            a[[pivot, j]] = tmp;
        }
        ld += a[[col, col]].abs().ln();
        for r in (col + 1)..n {
            let factor = a[[r, col]] / a[[col, col]];
            for j in (col + 1)..n {
                let val = a[[col, j]];
                a[[r, j]] -= factor * val;
            }
        }
    }
    Some(ld)
}

/// Matrix inversion via Gauss-Jordan elimination.
fn invert(a_in: &Array2<f64>) -> Option<Array2<f64>> {
    let n = a_in.nrows();
    let mut a = a_in.clone();
    let mut inv = Array2::eye(n);
    for col in 0..n {
        let mut pivot = col;
        for r in (col + 1)..n {
            if a[[r, col]].abs() > a[[pivot, col]].abs() {
                pivot = r;
            }
        }
        if a[[pivot, col]].abs() < 1e-12 {
            return None;
        }
        // Swap rows.
        for j in 0..n {
            let tmp_a = a[[col, j]];
            a[[col, j]] = a[[pivot, j]];
            a[[pivot, j]] = tmp_a;
            let tmp_i = inv[[col, j]];
            inv[[col, j]] = inv[[pivot, j]];
            inv[[pivot, j]] = tmp_i;
        }
        let d = a[[col, col]];
        for j in 0..n {
            a[[col, j]] /= d;
            inv[[col, j]] /= d;
        }
        for r in 0..n {
            if r == col {
                continue;
            }
            let factor = a[[r, col]];
            for j in 0..n {
                let val_a = a[[col, j]];
                a[[r, j]] -= factor * val_a;
                let val_i = inv[[col, j]];
                inv[[r, j]] -= factor * val_i;
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
        run("crosstabs", serde_json::json!({ "row": "grp", "col": "x" }));
        run(
            "anova_oneway",
            serde_json::json!({ "vars": ["y"], "factor": "grp", "posthoc": "bonferroni" }),
        );
        run("factor", serde_json::json!({ "vars": ["y", "x"], "rotation": "varimax" }));
    }

    #[test]
    fn charts_run_without_panic() {
        let eng = sample_engine();
        eng.run_chart("histogram", &serde_json::json!({ "var": "y" })).unwrap();
        eng.run_chart("bar", &serde_json::json!({ "var": "grp" })).unwrap();
        eng.run_chart("scatter", &serde_json::json!({ "x": "x", "y": "y" }))
            .unwrap();
        eng.run_chart("box", &serde_json::json!({ "vars": ["y"], "group": "grp" }))
            .unwrap();
        eng.run_chart("box", &serde_json::json!({ "vars": ["y", "x"] })).unwrap();
    }

    #[test]
    fn jacobi_recovers_eigenvalues() {
        let (vals, _) = jacobi_eigen(&ndarray::array![[2.0, 0.0], [0.0, 3.0]]);
        let mut v = vals.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        close(v[0], 2.0, 1e-9);
        close(v[1], 3.0, 1e-9);
        let (vals2, _) = jacobi_eigen(&ndarray::array![[1.0, 0.6], [0.6, 1.0]]);
        let mut v2 = vals2.to_vec();
        v2.sort_by(|a, b| a.partial_cmp(b).unwrap());
        close(v2[0], 0.4, 1e-9);
        close(v2[1], 1.6, 1e-9);
    }

    #[test]
    fn box_summary_quartiles() {
        // 1..=9 → Q1=3, median=5, Q3=7 (type-7 interpolation).
        let data: Vec<f64> = (1..=9).map(|i| i as f64).collect();
        let b = box_summary("v", &data).unwrap();
        close(b["q1"].as_f64().unwrap(), 3.0, 1e-9);
        close(b["median"].as_f64().unwrap(), 5.0, 1e-9);
        close(b["q3"].as_f64().unwrap(), 7.0, 1e-9);
    }

    #[test]
    fn t_critical_values() {
        // t_0.05,2-tailed for df=10 is 2.2281; df=∞-ish (1000) ≈ 1.962.
        close(t_crit(0.05, 10.0), 2.22814, 1e-3);
        close(t_crit(0.05, 1000.0), 1.9623, 2e-3);
        // Round-trip: p of the critical value equals alpha.
        close(t_sig_2tailed(t_crit(0.01, 25.0), 25.0), 0.01, 1e-4);
    }

    #[test]
    fn ptukey_known() {
        // q_0.05(3, 20) = 3.578 → ptukey ≈ 0.95.
        close(ptukey(3.578, 3.0, 20.0), 0.95, 5e-3);
        // q_0.05(4, 12) = 4.199 → ptukey ≈ 0.95.
        close(ptukey(4.199, 4.0, 12.0), 0.95, 5e-3);
    }

    #[test]
    fn reliability_and_glm_run() {
        let df = df![
            "i1" => [1.0, 2.0, 3.0, 4.0, 5.0, 2.0, 3.0, 4.0],
            "i2" => [2.0, 2.0, 3.0, 5.0, 4.0, 1.0, 3.0, 5.0],
            "i3" => [1.0, 3.0, 4.0, 4.0, 5.0, 2.0, 2.0, 4.0],
            "g"  => [1i64, 1, 1, 1, 2, 2, 2, 2],
            "y"  => [5.0, 6.0, 7.0, 8.0, 3.0, 4.0, 5.0, 6.0],
        ]
        .unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        eng.run_analysis("reliability", &serde_json::json!({ "vars": ["i1", "i2", "i3"] }))
            .unwrap();
        let glm = eng
            .run_analysis(
                "glm_univariate",
                &serde_json::json!({ "dependent": "y", "factors": ["g"], "covariates": ["i1"] }),
            )
            .unwrap();
        assert!(!glm.tables[0].rows.is_empty());
        eng.run_chart("line", &serde_json::json!({ "vars": ["y", "i1"] })).unwrap();
        eng.run_chart("clustered_bar", &serde_json::json!({ "var": "g", "cluster": "i1" }))
            .unwrap();
        eng.run_analysis(
            "anova_oneway",
            &serde_json::json!({ "vars": ["y"], "factor": "g", "posthoc": "tukey" }),
        )
        .unwrap();
    }

    #[test]
    fn manova_two_groups_consistent() {
        // With 2 groups (df_h = 1) all four multivariate tests give the same F.
        let df = df![
            "y1" => [1.0, 2.0, 3.0, 2.0, 8.0, 9.0, 7.0, 8.0],
            "y2" => [2.0, 1.0, 2.0, 3.0, 7.0, 8.0, 9.0, 7.0],
            "g"  => [1i64, 1, 1, 1, 2, 2, 2, 2],
        ]
        .unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        let a = eng
            .run_analysis(
                "glm_multivariate",
                &serde_json::json!({ "dependents": ["y1", "y2"], "factors": ["g"] }),
            )
            .unwrap();
        let fs: Vec<f64> = a.tables[0].rows.iter().map(|r| r[2].as_f64().unwrap()).collect();
        for f in &fs {
            close(*f, fs[0], 1e-6);
        }
        assert!(fs[0] > 0.0);
    }

    #[test]
    fn mixed_and_survival_run() {
        let df = df![
            "y"      => [5.0, 6.0, 7.0, 8.0, 3.0, 4.0, 5.0, 6.0],
            "subj"   => [1i64, 1, 2, 2, 3, 3, 4, 4],
            "x"      => [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
            "time"   => [5.0, 8.0, 12.0, 3.0, 9.0, 15.0, 7.0, 20.0],
            "status" => [1i64, 1, 0, 1, 1, 0, 1, 0],
            "grp"    => [1i64, 1, 1, 1, 2, 2, 2, 2],
        ]
        .unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        let mm = eng
            .run_analysis(
                "mixed_model",
                &serde_json::json!({ "dependent": "y", "randomFactors": ["subj"], "covariates": ["x"] }),
            )
            .unwrap();
        // ICC value is the last table's single row, column 1.
        let icc = mm.tables.last().unwrap().rows[0][1].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&icc), "ICC out of range: {icc}");

        eng.run_analysis(
            "survival_km",
            &serde_json::json!({ "time": "time", "status": "status", "eventValue": "1", "factor": "grp" }),
        )
        .unwrap();
    }

    #[test]
    fn cox_recovers_positive_effect() {
        // Higher x → shorter survival → positive coefficient / HR > 1.
        // Group x=1 tends to fail earlier but event times overlap, so the MLE
        // is finite (not separated).
        let df = df![
            "time"   => [2.0, 4.0, 6.0, 9.0, 3.0, 5.0, 7.0, 8.0, 10.0],
            "status" => [1i64, 1, 1, 1, 1, 1, 1, 1, 1],
            "x"      => [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ]
        .unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        let a = eng
            .run_analysis(
                "cox_regression",
                &serde_json::json!({ "time": "time", "status": "status", "eventValue": "1", "covariates": ["x"] }),
            )
            .unwrap();
        // Variables in the Equation: B in column 1, Exp(B) in column 6.
        let b = a.tables[1].rows[0][1].as_f64().unwrap();
        let hr = a.tables[1].rows[0][6].as_f64().unwrap();
        assert!(b > 0.0, "expected positive B, got {b}");
        assert!(hr > 1.0, "expected HR > 1, got {hr}");
    }

    #[test]
    fn reml_balanced_matches_anova() {
        // Balanced one-way design: REML variance components equal the ANOVA
        // estimator. Groups with a clear between-subject shift.
        let df = df![
            "y"    => [10.0, 11.0, 12.0, 20.0, 21.0, 22.0, 30.0, 31.0, 32.0],
            "subj" => [1i64, 1, 1, 2, 2, 2, 3, 3, 3],
        ]
        .unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        let a = eng
            .run_analysis("mixed_model", &serde_json::json!({ "dependent": "y", "randomFactors": ["subj"], "covariates": [] }))
            .unwrap();
        // Covariance parameters: residual variance ≈ 1.0 (each group is m-1,m,m+1).
        let resid = a.tables[1].rows[1][1].as_f64().unwrap();
        close(resid, 1.0, 1e-3);
        // Subject variance is large and positive; ICC near 1.
        let var_subj = a.tables[1].rows[0][1].as_f64().unwrap();
        assert!(var_subj > 50.0, "expected large subject variance, got {var_subj}");
        let icc = a.tables[2].rows[0][1].as_f64().unwrap();
        assert!(icc > 0.95, "expected high ICC, got {icc}");
    }

    #[test]
    fn repeated_measures_runs() {
        let df = df![
            "t1" => [8.0, 10.0, 9.0, 11.0, 7.0],
            "t2" => [12.0, 13.0, 11.0, 15.0, 10.0],
            "t3" => [15.0, 18.0, 16.0, 19.0, 14.0],
        ]
        .unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        let a = eng
            .run_analysis("glm_repeated", &serde_json::json!({ "vars": ["t1", "t2", "t3"] }))
            .unwrap();
        // Strong increasing trend with non-zero error → significant within effect.
        let sig = a.tables[1].rows[0][5].as_f64().unwrap();
        assert!(sig < 0.01, "expected significant within effect, got {sig}");
    }

    #[test]
    fn levene_equal_variances() {
        // Two groups with identical spread → Levene F ≈ 0, large p.
        let a = [1.0, 2.0, 3.0, 4.0, 5.0];
        let b = [11.0, 12.0, 13.0, 14.0, 15.0];
        let (f, sig) = levene_two(&a, &b, 3.0, 13.0);
        close(f, 0.0, 1e-9);
        assert!(sig > 0.99, "expected high p, got {sig}");
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
        let m = ndarray::array![[4.0, 7.0], [2.0, 6.0]];
        let inv = invert(&m).unwrap();
        // inverse of [[4,7],[2,6]] = [[0.6,-0.7],[-0.2,0.4]]
        close(inv[[0, 0]], 0.6, 1e-9);
        close(inv[[0, 1]], -0.7, 1e-9);
        close(inv[[1, 0]], -0.2, 1e-9);
        close(inv[[1, 1]], 0.4, 1e-9);
    }

    #[test]
    fn cox_time_varying() {
        let df = df![
            "start"  => [0.0, 5.0, 0.0],
            "stop"   => [5.0, 10.0, 10.0],
            "status" => [0i64, 1, 1],
            "x"      => [0.0, 1.0, 0.0],
        ].unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        let a = eng.run_analysis("cox_regression", &serde_json::json!({
            "time": "stop",
            "startTime": "start",
            "status": "status",
            "eventValue": "1",
            "covariates": ["x"]
        })).unwrap();
        let b = a.tables[1].rows[0][1].as_f64().unwrap();
        assert!(b.is_finite());
    }

    #[test]
    fn mixed_model_crossed() {
        let df = df![
            "y"   => [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
            "r1"  => ["a", "a", "b", "b", "a", "a", "b", "b"],
            "r2"  => ["x", "y", "x", "y", "x", "y", "x", "y"],
        ].unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        let a = eng.run_analysis("mixed_model", &serde_json::json!({
            "dependent": "y",
            "randomFactors": ["r1", "r2"]
        })).unwrap();
        assert_eq!(a.tables[1].rows.len(), 3);
    }

    #[test]
    fn mancova_full() {
        // Use random-ish data to avoid singularity.
        let df = df![
            "y1" => [1.0, 2.2, 3.1, 4.5, 5.2, 6.7, 7.3, 8.9, 1.4, 2.5, 3.6, 4.1, 5.8, 6.2, 7.7, 8.3],
            "y2" => [2.1, 3.4, 4.2, 5.6, 6.1, 7.8, 8.2, 9.9, 2.3, 3.1, 4.5, 5.2, 6.7, 7.3, 8.9, 1.4],
            "f1" => ["a", "a", "a", "a", "b", "b", "b", "b", "a", "a", "a", "a", "b", "b", "b", "b"],
            "f2" => ["x", "x", "y", "y", "x", "x", "y", "y", "x", "x", "y", "y", "x", "x", "y", "y"],
            "c1" => [1.1, 2.3, 1.4, 2.5, 1.6, 2.7, 1.8, 2.9, 1.2, 2.4, 1.5, 2.6, 1.7, 2.8, 1.9, 2.1],
        ].unwrap();
        let mut eng = Engine::default();
        eng.df = Some(df);
        let a = eng.run_analysis("glm_multivariate", &serde_json::json!({
            "dependents": ["y1", "y2"],
            "factors": ["f1", "f2"],
            "covariates": ["c1"]
        })).unwrap();
        assert_eq!(a.tables.len(), 2);
    }
}
