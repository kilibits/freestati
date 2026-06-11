# FreeStats

A free, open, cross-platform statistical analysis application — an SPSS-style data
editor and analysis environment built on Electron + TypeScript, powered by a
Polars/SciPy Python engine. First-class support for `.tab` files alongside CSV,
Excel, SPSS (`.sav`), Stata (`.dta`), and SAS (`.sas7bdat`).

## Features

- **SPSS-style data editor** — Data View and Variable View with inline editing
- **File Explorer sidebar** — open a whole folder and load datasets on demand
- **Virtualized grid** — AG Grid infinite row model scrolls millions of rows without
  loading them all into the renderer
- **Broad format support** — `.tab`, `.tsv`, `.csv`, `.xlsx`/`.xls`, `.sav`, `.dta`,
  `.sas7bdat` (read); `.tab`, `.csv`, `.xlsx`, `.sav` (write)
- **SPSS metadata** — variable labels, value labels, measure level, missing values,
  alignment imported from `.sav` files
- **Native menus** — full Analyze / Graphs / Data menu tree (procedures land here as
  the analysis layer is built out)
- **Cross-platform** — packaged installers for Windows, macOS (Intel + Apple
  Silicon), and Linux

## Architecture

```
┌────────────────────────── Electron ──────────────────────────┐
│                                                               │
│  Renderer (sandboxed)            Main process                 │
│  ┌──────────────────┐            ┌────────────────────────┐   │
│  │ App / DataView   │            │ main.ts  (window/menu) │   │
│  │ VariableView     │  IPC       │ fileHandlers.ts        │   │
│  │ FileExplorer     │◄─preload──►│ pythonBridge.ts        │   │
│  │ AG Grid          │            └───────────┬────────────┘   │
│  └──────────────────┘                        │                │
│                                              │ msgpack over   │
│                                              │ stdin/stdout   │
└──────────────────────────────────────────────┼───────────────┘
                                                ▼
                              ┌──────────────────────────────┐
                              │ engine.py (Python)           │
                              │  Polars  · SciPy             │
                              │  statsmodels · pyreadstat    │
                              └──────────────────────────────┘
```

- **Renderer** is fully sandboxed (`contextIsolation`, no `nodeIntegration`); it
  talks to the main process only through the typed `window.electron` bridge defined
  in [src/main/preload.ts](src/main/preload.ts).
- **Main process** brokers file dialogs, filesystem browsing, and forwards analysis
  requests to the Python engine.
- **Python engine** holds the active dataset as a Polars DataFrame and serves data
  pages, variable metadata, and (in progress) statistical procedures.

### IPC transport

Communication with Python uses **MessagePack** framed with a 4-byte big-endian
length prefix on both directions — faster to parse and ~30% smaller than
newline-delimited JSON. The engine runs an `asyncio` loop with a
`ThreadPoolExecutor`, so concurrent data-page requests (fired by the grid during
fast scrolling) run in parallel instead of queuing.

## Performance design

| Concern | Approach |
| --- | --- |
| Large datasets in the grid | AG Grid **infinite row model**, 500-row cache blocks |
| Data paging | Polars `slice` + `write_json(row_oriented=True)` — rows serialized in Rust, **no per-row Python dicts** |
| IPC overhead | Binary **MessagePack** + length-prefix framing |
| Concurrency | `asyncio` + `ThreadPoolExecutor` engine; rapid scroll requests run in parallel |
| Load-time inference | Type/measure/decimal inference **samples** the first 10k rows — O(cols), not O(rows × cols) |
| Renderer payload | esbuild **minifies** the production bundle |

## Prerequisites

- **Node.js** ≥ 18
- **Python** ≥ 3.10 available on `PATH` as `python3` (macOS/Linux) or `python`
  (Windows)

## Getting started

```bash
# 1. Install Node dependencies
npm install

# 2. Install the Python engine dependencies
pip install -r src/main/python/requirements.txt

# 3. Build and launch
npm start
```

## Development

```bash
npm run dev      # watch main + renderer, launch with DevTools and --inspect
npm run build    # one-off production build (minified renderer) → dist/
npm run lint     # type-check main + renderer without emitting
```

`npm run dev` rebuilds the main process and renderer on change. The renderer is
bundled by [scripts/build-renderer.mjs](scripts/build-renderer.mjs) (esbuild); the
main process is compiled by `tsc` against
[tsconfig.main.json](tsconfig.main.json).

## Packaging

`electron-builder` produces signed-ready installers. The Python engine is bundled
as an extra resource (`src/main/python` → `resources/python`).

```bash
npm run package          # current platform
npm run package:win      # NSIS installer + portable (x64, arm64)
npm run package:mac      # DMG + zip (Intel + Apple Silicon)
npm run package:linux    # AppImage + deb + rpm
```

> Packaged builds expect a compatible Python interpreter with the engine's
> dependencies available at runtime. Bundling a self-contained interpreter is on
> the roadmap.

## Project layout

```
src/
├── main/                  Electron main process
│   ├── main.ts            window lifecycle + native menu
│   ├── preload.ts         typed contextBridge API (window.electron)
│   ├── ipc/
│   │   ├── fileHandlers.ts  file dialogs, fs browsing, data paging
│   │   └── pythonBridge.ts  msgpack length-prefixed transport to Python
│   └── python/
│       ├── engine.py        Polars data engine (asyncio + thread pool)
│       └── requirements.txt
└── renderer/              Sandboxed UI
    ├── components/        App, DataView, VariableView, FileExplorer, StatusBar
    ├── stores/dataStore.ts  reactive dataset state
    ├── types/dataset.ts     shared types
    ├── index.html · styles.css · renderer.ts
    └── global.d.ts          window.electron typings
scripts/build-renderer.mjs   esbuild bundler
```

## Roadmap

- Descriptive statistics (Frequencies, Descriptives, Explore, Crosstabs)
- Compare Means (t-tests, one-way ANOVA)
- Correlation & regression (linear, binary logistic)
- Nonparametric tests (chi-square, Mann-Whitney, Kruskal-Wallis, Wilcoxon)
- Factor / cluster / reliability analysis
- Charts (histogram, bar, scatter, box plot)
- Output viewer for results and pivot tables
- Bundled Python runtime for zero-dependency installers

## License

TBD.
