# FreeStati

A free, open, cross-platform statistical analysis application — an SPSS-style data
editor and analysis environment built on **Tauri + TypeScript**, powered by a
native **Rust + Polars** data engine. First-class support for `.tab` files
alongside `.tsv` and `.csv`.

## Features

- **SPSS-style data editor** — Data View and Variable View; datasets open
  **read-only** by default, with a toolbar toggle to enable inline editing
- **Statistical procedures** — Descriptives, Frequencies, Crosstabs (chi-square,
  Cramér's V), t-tests (one-sample, independent with Levene's test, paired) with
  Cohen's d and 95% CIs, one-way ANOVA with post-hoc (LSD/Bonferroni/Tukey HSD)
  and η², GLM Univariate (factorial ANOVA/ANCOVA, Type III SS), correlation
  GLM Multivariate (one-way MANOVA: Pillai/Wilks/Hotelling/Roy) and Repeated
  Measures (within-subjects ANOVA + Greenhouse-Geisser), correlation
  (Pearson/Spearman), linear regression with coefficient CIs, factor analysis
  (PCA with varimax), reliability (Cronbach's α), linear mixed models (random
  intercept, **REML**), survival analysis (Kaplan-Meier + log-rank and **Cox
  proportional-hazards** regression), and nonparametric tests (Mann-Whitney U,
  Wilcoxon, Kruskal-Wallis, chi-square) — all computed natively in Rust with no
  external stats dependency
- **Syntax & scripting** — every analysis and chart is recorded as a replayable
  command in a Syntax editor; edit, save/open `.fst` scripts, and re-run an
  entire session for reproducibility
- **Charts** — histogram, bar, clustered bar, line, scatter (with fit line), and
  box plots, drawn as dependency-free SVG from engine-side aggregates; export any
  chart to SVG or PNG
- **Output viewer** — an SPSS-style results pane that accumulates tables and
  charts, with per-table copy / transpose / inline pivot editing, and export of
  the whole session to HTML, Word, or PDF (print)
- **File Explorer sidebar** — open a whole folder and load datasets on demand
- **Virtualized grid** — AG Grid infinite row model scrolls millions of rows without
  loading them all into the WebView
- **Native data engine** — datasets held as a Polars `DataFrame` in Rust; no Python
  runtime to install or bundle
- **Native menus & dialogs** — real OS menu bar and file dialogs via Tauri
- **Cross-platform** — small, fast installers for Windows, macOS (Intel + Apple
  Silicon), and Linux

### Supported formats

| Format | Read | Write |
|---|---|---|
| `.tab` / `.tsv` | ✅ | ✅ |
| `.csv` | ✅ | ✅ |
| `.sav` / `.zsav` (SPSS) | ✅ (incl. variable & value labels, measure) | ⏳ roadmap |
| `.xlsx` / `.dta` / `.sas7bdat` | ⏳ roadmap | ⏳ roadmap |

> `.sav` is read with the pure-Rust [`ambers`](https://crates.io/crates/ambers)
> crate. Excel/Stata/SAS reading and SPSS writing are on the roadmap; the loader
> returns a clear message for unsupported formats.

## Architecture

```
┌───────────────────────────── Tauri ──────────────────────────────┐
│                                                                   │
│  WebView (system: WKWebView / WebView2 / WebKitGTK)               │
│  ┌────────────────────────────┐                                  │
│  │ App / DataView             │     invoke() / events            │
│  │ VariableView / FileExplorer│◄───────────────┐                 │
│  │ AG Grid · bridge.ts        │                │                 │
│  └────────────────────────────┘                │                 │
│                                                 ▼                 │
│                                   ┌──────────────────────────┐    │
│                                   │ Rust core (lib.rs)       │    │
│                                   │  commands · native menu  │    │
│                                   │  engine.rs (Polars)      │    │
│                                   └──────────────────────────┘    │
└───────────────────────────────────────────────────────────────────┘
```

- **WebView**: the same TypeScript renderer (AG Grid, components) that the Electron
  version used. A thin shim, [bridge.ts](src/renderer/bridge.ts), recreates the old
  `window.electron` API on top of Tauri's `invoke`, event, and dialog APIs, so the
  components are unchanged.
- **Rust core**: Tauri commands in [lib.rs](src-tauri/src/lib.rs) plus the Polars
  data engine in [engine.rs](src-tauri/src/engine.rs). The dataset lives in a
  `Mutex<Engine>` managed by Tauri.

### Why this replaced Electron + Python

The previous stack was Electron (bundled Chromium) talking to a Python/Polars
subprocess over MessagePack. Tauri uses the **OS WebView** instead of bundling
Chromium, and Polars has first-class **Rust** bindings — so the engine moved
in-process and the Python subprocess + IPC layer were removed entirely.

| | Electron + Python | Tauri + Rust |
|---|---|---|
| Runtime UI | bundled Chromium (~150 MB) | system WebView (~few MB) |
| Data engine | Python subprocess (Polars) | in-process Rust (Polars) |
| Engine transport | MessagePack over stdio | direct function calls |
| External runtime | Python + pip deps required | none |
| Idle memory | ~150–250 MB | ~40–80 MB |

## Performance design

| Concern | Approach |
| --- | --- |
| Large datasets in the grid | AG Grid **infinite row model**, adaptive block size (~50k cells/page so wide frames don't pull huge blocks) |
| Data paging | Polars `slice` → `JsonWriter` (Rust) → renderer `JSON.parse` — no per-row allocation crossing the boundary |
| Engine calls | Direct in-process Rust; no subprocess or serialization round-trip |
| Load-time inference | Type/measure/decimal inference **samples** the first 10k rows — O(cols) |
| Renderer payload | esbuild **minifies** the production bundle |

## Prerequisites

- **Node.js** ≥ 18
- **Rust** ≥ 1.77 (via [rustup](https://rustup.rs))
- Platform WebView libraries (preinstalled on macOS/Windows 10+; on Linux install
  `webkit2gtk` and `libsoup` per the
  [Tauri prerequisites](https://tauri.app/start/prerequisites/))

## Getting started

```bash
npm install          # JS deps (@tauri-apps/api, AG Grid, esbuild, …)
npm run dev          # build renderer + launch the app (Tauri dev)
```

The first `dev`/`build` compiles the Rust core (downloads crates once; subsequent
builds are incremental).

## Development

```bash
npm run dev              # Tauri dev: renderer + Rust core, live
npm run build:renderer   # bundle the renderer only → dist/renderer
npm run lint             # type-check the renderer
(cd src-tauri && cargo check)   # type-check the Rust core
```

The renderer is bundled by [scripts/build-renderer.mjs](scripts/build-renderer.mjs)
(esbuild) into `dist/renderer`, which Tauri serves to the WebView.

## Packaging

```bash
npm run package      # tauri build → native installer for the current OS
```

`tauri build` runs `build:renderer` first, then compiles a release binary and
bundles it (`.dmg`/`.app`, `.msi`/`.exe`, `.AppImage`/`.deb`). Per-platform icons
live in [src-tauri/icons](src-tauri/icons) (replace the placeholder `icon.png`
with real assets before shipping).

## Project layout

```
src/renderer/              WebView UI
├── components/            App, DataView, VariableView, OutputView, SyntaxView,
│                          FileExplorer, StatusBar, dialogs (variable pickers),
│                          charts (SVG), syntax (parse/replay)
├── stores/                dataStore · outputStore (results) · syntaxStore (script)
├── types/                 dataset.ts · analysis.ts (result tables)
├── bridge.ts              window.electron shim over Tauri invoke/events/dialog
├── global.d.ts            window.electron typings
└── index.html · styles.css · renderer.ts
src-tauri/                 Rust core
├── src/lib.rs             Tauri commands, native menu, app setup
├── src/engine.rs          Polars data engine + metadata/inference
├── src/stats.rs           statistical procedures + p-value special functions
├── tauri.conf.json        window, bundle, build config
├── capabilities/          permission grants for the WebView
└── Cargo.toml
scripts/build-renderer.mjs esbuild bundler
```

## Roadmap

- Restore `.xlsx` / `.dta` / `.sas7bdat` reading and `.sav` writing in the Rust engine
- Time-varying Cox covariates, crossed/nested random effects, multivariate GLM contrasts
- Tauri auto-updater

## License

TBD.
