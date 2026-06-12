import {
  AllCommunityModule,
  ColDef,
  GridApi,
  GridOptions,
  IDatasource,
  IGetRowsParams,
  InfiniteRowModelModule,
  ModuleRegistry,
  createGrid,
  themeBalham,
} from 'ag-grid-community';
import { dataStore } from '../stores/dataStore';
import type { Variable } from '../types/dataset';

// AllCommunityModule does NOT include InfiniteRowModelModule in v33 — register both
ModuleRegistry.registerModules([AllCommunityModule, InfiniteRowModelModule]);

export class DataView {
  private container!: HTMLElement;
  private gridContainer!: HTMLElement;
  private api: GridApi | null = null;
  private unsub: (() => void) | null = null;
  // Identity of the dataset currently shown; avoids rebuilding the grid on
  // unrelated store updates (e.g. the "modified" flag toggling after an edit).
  private loadedSignature: string | null = null;

  private searchQuery = '';
  private searchTimer: number | null = null;

  mount(container: HTMLElement): void {
    this.container = container;
    this.container.classList.add('data-view-container');
    this.container.style.display = 'flex';
    this.container.style.flexDirection = 'column';

    // ── Toolbar ──────────────────────────────────────────────────────────────
    const toolbar = document.createElement('div');
    toolbar.className = 'view-toolbar';
    toolbar.innerHTML = `
      <div class="search-input-wrapper">
        <span class="search-icon">🔍</span>
        <input type="text" class="search-input" placeholder="Search in all columns..." />
      </div>
    `;
    const input = toolbar.querySelector('input')!;
    input.addEventListener('input', (e) => this.onSearchInput(e));
    this.container.appendChild(toolbar);

    // ── Grid Container ───────────────────────────────────────────────────────
    this.gridContainer = document.createElement('div');
    this.gridContainer.style.flex = '1';
    this.gridContainer.style.width = '100%';
    this.container.appendChild(this.gridContainer);

    // Start with an empty grid (no dataset loaded yet).
    this.api = createGrid(this.gridContainer, this.gridOptions(100, [], undefined));
    this.unsub = dataStore.subscribe(() => this.onStoreChange());
  }

  private onSearchInput(e: Event): void {
    const val = (e.target as HTMLInputElement).value;
    this.searchQuery = val;

    if (this.searchTimer) window.clearTimeout(this.searchTimer);
    this.searchTimer = window.setTimeout(() => {
      if (!this.api) return;
      const state = dataStore.get();
      if (!state.loaded) return;
      // Re-bind the datasource to trigger a fresh fetch from row 0 with the query.
      this.api.setGridOption('datasource', this.buildDatasource(state.rowCount));
    }, 250);
  }

  // ── Static grid options shared by every (re)build ──────────────────────────

  private gridOptions(
    cacheBlockSize: number,
    columnDefs: ColDef[],
    datasource: IDatasource | undefined,
  ): GridOptions {
    return {
      theme: themeBalham.withParams({ accentColor: '#6366f1' }),
      rowModelType: 'infinite',
      // cacheBlockSize is an init-only property in AG Grid, so it can only be
      // set when the grid is created — that's why a new dataset rebuilds the grid.
      cacheBlockSize,
      maxBlocksInCache: 10,
      infiniteInitialRowCount: 100,
      // Treat `field` as a literal key. Without this, column names containing a
      // dot (e.g. "Q1.A") are read as nested paths row["Q1"]["A"] → blank cells.
      suppressFieldDotNotation: true,
      columnDefs,
      datasource,
      defaultColDef: {
        resizable: true,
        sortable: false,
        editable: true,
        minWidth: 80,
      },
      onCellValueChanged: (event) => {
        const col = event.colDef.field ?? '';
        const data = event.data as Record<string, unknown>;
        if (!data || !data['__row__']) return;
        const caseNum = data['__row__'] as number;
        window.electron.data.updateCell(caseNum, col, event.newValue).then(() => {
          dataStore.setModified(true);
        });
      },
      overlayNoRowsTemplate:
        '<span class="empty-state">No data loaded — use File › Open to load a dataset</span>',
    };
  }

  // ── Store reactions ────────────────────────────────────────────────────────

  private onStoreChange(): void {
    const state = dataStore.get();

    if (!state.loaded) {
      if (this.loadedSignature !== null) {
        this.loadedSignature = null;
        this.rebuild(100, [], undefined);
      }
      return;
    }

    const signature = `${state.path ?? ''}|${state.rowCount}|${state.colCount}`;
    if (signature === this.loadedSignature) {
      return; // same dataset — a non-structural change (e.g. modified flag)
    }
    this.loadedSignature = signature;

    const blockSize = this.blockSizeFor(state.variables.length);
    this.rebuild(blockSize, this.buildColumnDefs(state.variables), this.buildDatasource(state.rowCount));
  }

  /**
   * Re-apply column metadata (labels, decimals, value labels) after a Variable
   * View edit. Column defs can be swapped at runtime, so no grid rebuild needed.
   */
  refresh(): void {
    if (!this.api) return;
    const { variables } = dataStore.get();
    this.api.updateGridOptions({ columnDefs: this.buildColumnDefs(variables) });
    this.api.refreshCells({ force: true });
  }

  // ── Grid lifecycle ─────────────────────────────────────────────────────────

  /** Destroy and recreate the grid — the only way to change cacheBlockSize. */
  private rebuild(blockSize: number, columnDefs: ColDef[], datasource: IDatasource | undefined): void {
    this.api?.destroy();
    this.api = createGrid(this.gridContainer, this.gridOptions(blockSize, columnDefs, datasource));
  }

  // ── Builders ───────────────────────────────────────────────────────────────

  /** Cap each fetched page near ~50k cells so wide frames don't pull huge blocks. */
  private blockSizeFor(colCount: number): number {
    const cols = Math.max(1, colCount);
    return Math.min(200, Math.max(10, Math.floor(50_000 / cols)));
  }

  private buildColumnDefs(variables: Variable[]): ColDef[] {
    return [
      {
        headerName: '#',
        valueGetter: (p) => (p.node?.rowIndex != null ? p.node.rowIndex + 1 : ''),
        width: 56,
        minWidth: 40,
        pinned: 'left',
        sortable: false,
        editable: false,
        suppressMovable: true,
        cellStyle: { color: '#8892a4', fontStyle: 'italic', textAlign: 'right' },
      },
      ...variables.map((v): ColDef => ({
        field: v.name,
        headerName: v.name,
        headerTooltip: v.label || v.name,
        editable: true,
        width: Math.max(100, v.width * 10),
        valueFormatter: (params) => {
          if (params.value == null) return '';
          const label = v.valueLabels[String(params.value)];
          if (label) return label;
          if (v.type === 'numeric' && v.decimals >= 0) {
            const num = Number(params.value);
            if (isNaN(num)) return String(params.value);
            if (num % 1 === 0) return num.toFixed(0);
            return num.toFixed(v.decimals);
          }
          return String(params.value);
        },
      })),
    ];
  }

  private buildDatasource(rowCount: number): IDatasource {
    return {
      rowCount,
      getRows: (params: IGetRowsParams) => {
        const offset = params.startRow;
        const limit = params.endRow - params.startRow;
        window.electron.data
          .getPage(offset, limit, this.searchQuery)
          .then(({ rows, total }: { rows: Record<string, unknown>[]; total: number }) => {
            params.successCallback(rows, total);
          })
          .catch((err: unknown) => {
            console.error('[DataView] getPage failed:', err);
            params.failCallback();
          });
      },
    };
  }

  destroy(): void {
    this.unsub?.();
    this.api?.destroy();
    this.api = null;
  }
}
