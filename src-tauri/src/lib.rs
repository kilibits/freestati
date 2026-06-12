mod engine;
mod sav;
mod stats;

use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use tauri::menu::{Menu, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use engine::{Engine, LoadResult, PageResult, Variable};

type EngineState = Mutex<Engine>;

// ── Data exts recognised by the File Explorer ───────────────────────────────
const DATA_EXTS: &[&str] = &["tab", "tsv", "csv", "xlsx", "xls", "sav", "dta", "sas7bdat"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirEntry {
    name: String,
    path: String,
    is_directory: bool,
    ext: String,
}

#[derive(Serialize)]
struct VariablesResult {
    variables: Vec<Variable>,
}

// ── App / misc commands ─────────────────────────────────────────────────────

#[tauri::command]
fn get_platform() -> String {
    // Mirror Node's process.platform values the renderer already understands.
    match std::env::consts::OS {
        "macos" => "darwin".into(),
        "windows" => "win32".into(),
        other => other.into(),
    }
}

#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(&url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn read_dir(path: String) -> Result<Vec<DirEntry>, String> {
    let mut entries: Vec<DirEntry> = std::fs::read_dir(&path)
        .map_err(|e| e.to_string())?
        .filter_map(|res| res.ok())
        .filter_map(|entry| {
            let p = entry.path();
            let is_dir = p.is_dir();
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !is_dir && !DATA_EXTS.contains(&ext.as_str()) {
                return None;
            }
            Some(DirEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                path: p.to_string_lossy().to_string(),
                is_directory: is_dir,
                ext: if ext.is_empty() { String::new() } else { format!(".{ext}") },
            })
        })
        .collect();

    // Directories first, then alphabetical (case-insensitive).
    entries.sort_by(|a, b| match (a.is_directory, b.is_directory) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(entries)
}

// ── Dataset commands (named to match the renderer's IPC channels) ───────────

#[tauri::command(async)]
fn load_file(state: State<EngineState>, path: String) -> Result<LoadResult, String> {
    state.lock().unwrap().load_file(&path)
}

#[tauri::command]
fn new_dataset(state: State<EngineState>) -> LoadResult {
    state.lock().unwrap().new_dataset()
}

#[tauri::command(async)]
fn get_page(
    state: State<EngineState>,
    offset: usize,
    limit: usize,
    query: Option<String>,
) -> Result<PageResult, String> {
    state.lock().unwrap().get_page(offset, limit, query)
}

#[tauri::command]
fn get_variables(state: State<EngineState>) -> VariablesResult {
    VariablesResult {
        variables: state.lock().unwrap().variables(),
    }
}

#[tauri::command]
fn set_variable_meta(state: State<EngineState>, name: String, meta: JsonValue) -> JsonValue {
    state.lock().unwrap().set_variable_meta(&name, &meta);
    json!({ "ok": true })
}

#[tauri::command]
fn update_cell(
    state: State<EngineState>,
    row: usize,
    col: String,
    value: JsonValue,
) -> Result<JsonValue, String> {
    state.lock().unwrap().update_cell(row, &col, &value)?;
    Ok(json!({ "ok": true }))
}

#[tauri::command(async)]
fn save_file(state: State<EngineState>, path: String) -> Result<JsonValue, String> {
    state.lock().unwrap().save_file(&path)?;
    Ok(json!({ "ok": true, "path": path }))
}

// ── Statistical procedures ──────────────────────────────────────────────────

#[tauri::command(async)]
fn run_analysis(
    state: State<EngineState>,
    procedure: String,
    params: JsonValue,
) -> Result<stats::Analysis, String> {
    state.lock().unwrap().run_analysis(&procedure, &params)
}

#[tauri::command(async)]
fn run_chart(
    state: State<EngineState>,
    kind: String,
    params: JsonValue,
) -> Result<stats::ChartData, String> {
    state.lock().unwrap().run_chart(&kind, &params)
}

/// Write text (e.g. exported output HTML) to a path chosen via the save dialog.
#[tauri::command(async)]
fn save_text_file(path: String, contents: String) -> Result<JsonValue, String> {
    std::fs::write(&path, contents).map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "path": path }))
}

/// Write raw bytes (e.g. a PNG rendered from a chart) to a chosen path.
#[tauri::command(async)]
fn save_binary_file(path: String, bytes: Vec<u8>) -> Result<JsonValue, String> {
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "path": path }))
}

/// Read a UTF-8 text file (e.g. a saved syntax script).
#[tauri::command(async)]
fn read_text_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

// ── Native menu ─────────────────────────────────────────────────────────────

fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let mi = |id: &str, label: &str, accel: Option<&str>| {
        let mut b = MenuItemBuilder::with_id(id, label);
        if let Some(a) = accel {
            b = b.accelerator(a);
        }
        b.build(app)
    };
    // File
    let file = SubmenuBuilder::new(app, "File")
        .item(&mi("menu:file:new", "New Dataset", Some("CmdOrCtrl+N"))?)
        .separator()
        .item(&mi("menu:file:open", "Open…", Some("CmdOrCtrl+O"))?)
        .item(&mi("menu:file:openFolder", "Open Folder…", Some("CmdOrCtrl+Shift+O"))?)
        .separator()
        .item(&mi("menu:file:save", "Save", Some("CmdOrCtrl+S"))?)
        .item(&mi("menu:file:saveAs", "Save As…", Some("CmdOrCtrl+Shift+S"))?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, Some("Quit FreeStati"))?)
        .build()?;

    // Edit (native roles)
    let edit = SubmenuBuilder::new(app, "Edit")
        .item(&PredefinedMenuItem::undo(app, None)?)
        .item(&PredefinedMenuItem::redo(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::cut(app, None)?)
        .item(&PredefinedMenuItem::copy(app, None)?)
        .item(&PredefinedMenuItem::paste(app, None)?)
        .item(&PredefinedMenuItem::select_all(app, None)?)
        .build()?;

    // Analyze — emits "menu:analyze:<id>" to the renderer, which opens a dialog.
    let nonparametric = SubmenuBuilder::new(app, "Nonparametric Tests")
        .item(&mi("menu:analyze:mann_whitney", "2 Independent Samples (Mann-Whitney U)…", None)?)
        .item(&mi("menu:analyze:wilcoxon", "2 Related Samples (Wilcoxon)…", None)?)
        .item(&mi("menu:analyze:kruskal_wallis", "K Independent Samples (Kruskal-Wallis)…", None)?)
        .item(&mi("menu:analyze:chi_square", "Chi-Square…", None)?)
        .build()?;

    let compare_means = SubmenuBuilder::new(app, "Compare Means")
        .item(&mi("menu:analyze:ttest_one_sample", "One-Sample T Test…", None)?)
        .item(&mi("menu:analyze:ttest_independent", "Independent-Samples T Test…", None)?)
        .item(&mi("menu:analyze:ttest_paired", "Paired-Samples T Test…", None)?)
        .item(&mi("menu:analyze:anova_oneway", "One-Way ANOVA…", None)?)
        .build()?;

    let glm = SubmenuBuilder::new(app, "General Linear Model")
        .item(&mi("menu:analyze:glm_univariate", "Univariate…", None)?)
        .item(&mi("menu:analyze:glm_multivariate", "Multivariate (MANOVA)…", None)?)
        .item(&mi("menu:analyze:glm_repeated", "Repeated Measures…", None)?)
        .build()?;

    let mixed = SubmenuBuilder::new(app, "Mixed Models")
        .item(&mi("menu:analyze:mixed_model", "Linear (random intercept)…", None)?)
        .build()?;

    let survival = SubmenuBuilder::new(app, "Survival")
        .item(&mi("menu:analyze:survival_km", "Kaplan-Meier…", None)?)
        .item(&mi("menu:analyze:cox_regression", "Cox Regression…", None)?)
        .build()?;

    let analyze = SubmenuBuilder::new(app, "Analyze")
        .item(&mi("menu:analyze:frequencies", "Frequencies…", None)?)
        .item(&mi("menu:analyze:descriptives", "Descriptives…", None)?)
        .item(&mi("menu:analyze:crosstabs", "Crosstabs…", None)?)
        .separator()
        .item(&compare_means)
        .item(&glm)
        .item(&mixed)
        .item(&mi("menu:analyze:correlate", "Correlate…", None)?)
        .item(&mi("menu:analyze:regression_linear", "Linear Regression…", None)?)
        .item(&mi("menu:analyze:factor", "Factor Analysis…", None)?)
        .item(&mi("menu:analyze:reliability", "Reliability Analysis…", None)?)
        .item(&survival)
        .item(&nonparametric)
        .build()?;

    // Graphs — emit "menu:graph:<kind>" to the renderer, which opens a dialog.
    let graphs = SubmenuBuilder::new(app, "Graphs")
        .item(&mi("menu:graph:histogram", "Histogram…", None)?)
        .item(&mi("menu:graph:bar", "Bar Chart…", None)?)
        .item(&mi("menu:graph:clustered_bar", "Clustered Bar Chart…", None)?)
        .item(&mi("menu:graph:line", "Line Chart…", None)?)
        .item(&mi("menu:graph:scatter", "Scatter Plot…", None)?)
        .item(&mi("menu:graph:box", "Box Plot…", None)?)
        .build()?;

    // View
    let view = SubmenuBuilder::new(app, "View")
        .item(&mi("menu:view:explorer", "File Explorer", Some("CmdOrCtrl+Shift+E"))?)
        .separator()
        .item(&mi("menu:view:dataView", "Data View", Some("CmdOrCtrl+D"))?)
        .item(&mi("menu:view:variableView", "Variable View", Some("CmdOrCtrl+Shift+D"))?)
        .item(&mi("menu:view:output", "Output", Some("CmdOrCtrl+Shift+U"))?)
        .item(&mi("menu:view:syntax", "Syntax", Some("CmdOrCtrl+Shift+Y"))?)
        .separator()
        .item(&mi("menu:view:reload", "Reload", Some("CmdOrCtrl+R"))?)
        .item(&mi("menu:view:devtools", "Toggle Developer Tools", Some("CmdOrCtrl+Alt+I"))?)
        .build()?;

    // Help
    let help = SubmenuBuilder::new(app, "Help")
        .item(&mi("menu:help:about", "About FreeStati", None)?)
        .build()?;

    Menu::with_items(app, &[&file, &edit, &analyze, &graphs, &view, &help])
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        "menu:view:reload" => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.eval("window.location.reload()");
            }
        }
        "menu:view:devtools" => {
            #[cfg(debug_assertions)]
            if let Some(w) = app.get_webview_window("main") {
                if w.is_devtools_open() {
                    w.close_devtools();
                } else {
                    w.open_devtools();
                }
            }
        }
        // Everything else is forwarded to the renderer, which listens via the
        // bridge's menu.on(channel, …) (mirrors the old Electron menu events).
        other => {
            let _ = app.emit(other, ());
        }
    }
}

// ── Entry point ─────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Mutex::new(Engine::default()))
        .menu(|app| build_menu(app))
        .on_menu_event(|app, event| handle_menu_event(app, event.id().0.as_str()))
        .invoke_handler(tauri::generate_handler![
            get_platform,
            open_external,
            read_dir,
            load_file,
            new_dataset,
            get_page,
            get_variables,
            set_variable_meta,
            update_cell,
            save_file,
            run_analysis,
            run_chart,
            save_text_file,
            save_binary_file,
            read_text_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running FreeStati");
}
