mod engine;

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

#[tauri::command]
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

#[tauri::command]
fn load_file(state: State<EngineState>, path: String) -> Result<LoadResult, String> {
    state.lock().unwrap().load_file(&path)
}

#[tauri::command]
fn new_dataset(state: State<EngineState>) -> LoadResult {
    state.lock().unwrap().new_dataset()
}

#[tauri::command]
fn get_page(state: State<EngineState>, offset: usize, limit: usize) -> Result<PageResult, String> {
    state.lock().unwrap().get_page(offset, limit)
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

#[tauri::command]
fn save_file(state: State<EngineState>, path: String) -> Result<JsonValue, String> {
    state.lock().unwrap().save_file(&path)?;
    Ok(json!({ "ok": true, "path": path }))
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
    let disabled = |id: &str, label: &str| MenuItemBuilder::with_id(id, label).enabled(false).build(app);

    // File
    let file = SubmenuBuilder::new(app, "File")
        .item(&mi("menu:file:new", "New Dataset", Some("CmdOrCtrl+N"))?)
        .separator()
        .item(&mi("menu:file:open", "Open…", Some("CmdOrCtrl+O"))?)
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

    // Analyze (placeholders until procedures land)
    let analyze = SubmenuBuilder::new(app, "Analyze")
        .item(&disabled("an:freq", "Frequencies…")?)
        .item(&disabled("an:desc", "Descriptives…")?)
        .item(&disabled("an:cross", "Crosstabs…")?)
        .separator()
        .item(&disabled("an:ttest", "T Tests…")?)
        .item(&disabled("an:anova", "One-Way ANOVA…")?)
        .item(&disabled("an:corr", "Correlate…")?)
        .item(&disabled("an:reg", "Linear Regression…")?)
        .build()?;

    // Graphs (placeholders)
    let graphs = SubmenuBuilder::new(app, "Graphs")
        .item(&disabled("gr:hist", "Histogram…")?)
        .item(&disabled("gr:bar", "Bar Chart…")?)
        .item(&disabled("gr:scatter", "Scatter Plot…")?)
        .item(&disabled("gr:box", "Box Plot…")?)
        .build()?;

    // View
    let view = SubmenuBuilder::new(app, "View")
        .item(&mi("menu:view:explorer", "File Explorer", Some("CmdOrCtrl+Shift+E"))?)
        .separator()
        .item(&mi("menu:view:dataView", "Data View", Some("CmdOrCtrl+D"))?)
        .item(&mi("menu:view:variableView", "Variable View", Some("CmdOrCtrl+Shift+D"))?)
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running FreeStati");
}
