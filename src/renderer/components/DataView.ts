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
  private searchColumn = '';
  private searchTimer: number | null = null;
  private loadedEditMode = false;
  private searchInput!: HTMLInputElement;
  private filterSelect!: HTMLSelectElement;
  private clearBtn!: HTMLButtonElement;

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
        <input type="text" class="search-input" placeholder="Search…" />
      </div>
      <select class="column-filter" title="Limit the search to one column">
        <option value="">All columns</option>
      </select>
      <button class="clear-filter-btn" title="Clear search and column filter" hidden>✕ Clear</button>
    `;
    this.searchInput = toolbar.querySelector('input')!;
    this.filterSelect = toolbar.querySelector('select')!;
    this.clearBtn = toolbar.querySelector('button')!;
    this.searchInput.addEventListener('input', () => this.onSearchInput());
    this.filterSelect.addEventListener('change', () => this.onFilterChange());
    this.clearBtn.addEventListener('click', () => this.clearFilters());
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

  private onSearchInput(): void {
    this.searchQuery = this.searchInput.value;
    this.updateClearButton();
    // Debounce: re-fetch from row 0 with the new query after typing settles.
    if (this.searchTimer) window.clearTimeout(this.searchTimer);
    this.searchTimer = window.setTimeout(() => this.applySearch(), 250);
  }

  private onFilterChange(): void {
    this.searchColumn = this.filterSelect.value;
    this.updateClearButton();
    this.applySearch(); // column change takes effect immediately
  }

  private clearFilters(): void {
    this.searchInput.value = '';
    this.filterSelect.value = '';
    this.searchQuery = '';
    this.searchColumn = '';
    this.updateClearButton();
    this.applySearch();
  }

  /** Re-bind the datasource so AG Grid re-fetches page 0 with the current query. */
  private applySearch(): void {
    if (!this.api) return;
    const state = dataStore.get();
    if (!state.loaded) return;
    this.api.setGridOption('datasource', this.buildDatasource(state.rowCount));
  }

  private updateClearButton(): void {
    this.clearBtn.hidden = this.searchQuery === '' && this.searchColumn === '';
  }

  /** Fill the column-filter dropdown with the dataset's variables and reset it. */
  private populateColumnFilter(variables: Variable[]): void {
    const options = ['<option value="">All columns</option>'];
    for (const v of variables) {
      const name = v.name.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
      options.push(`<option value="${name}">${name}</option>`);
    }
    this.filterSelect.innerHTML = options.join('');
    this.filterSelect.value = '';
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
        this.clearFilters();
        this.populateColumnFilter([]);
        this.rebuild(100, [], undefined);
      }
      return;
    }

    const signature = `${state.path ?? ''}|${state.rowCount}|${state.colCount}`;
    if (signature === this.loadedSignature) {
      // Same dataset — but if edit mode toggled, re-apply column editability
      // (no grid rebuild needed; column defs can be swapped at runtime).
      if (state.editMode !== this.loadedEditMode) {
        this.loadedEditMode = state.editMode;
        this.refresh();
      }
      return;
    }
    this.loadedSignature = signature;

    // New dataset: reset any prior search/filter and repopulate the column list.
    this.searchInput.value = '';
    this.searchQuery = '';
    this.searchColumn = '';
    this.updateClearButton();
    this.populateColumnFilter(state.variables);
    this.loadedEditMode = state.editMode;

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
    const editable = dataStore.get().editMode;
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
        editable,
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
    const colFilter = this.searchColumn || undefined;
    return {
      rowCount,
      getRows: (params: IGetRowsParams) => {
        const offset = params.startRow;
        const limit = params.endRow - params.startRow;
        window.electron.data
          .getPage(offset, limit, this.searchQuery, colFilter)
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
