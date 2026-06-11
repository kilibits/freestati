import {
  AllCommunityModule,
  ColDef,
  GridApi,
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
  private api: GridApi | null = null;
  private unsub: (() => void) | null = null;

  mount(container: HTMLElement): void {
    this.container = container;
    this.container.classList.add('data-view-container');

    this.api = createGrid(this.container, {
      theme: themeBalham.withParams({ accentColor: '#6366f1' }),
      rowModelType: 'infinite',
      // Block size is tuned per-dataset in applyDataset(): wide frames (1000s of
      // columns) must use small blocks or the first page would be millions of
      // cells / tens of MB. 100 is a safe default until a dataset is loaded.
      cacheBlockSize: 100,
      maxBlocksInCache: 10,
      infiniteInitialRowCount: 100,
      // Treat `field` as a literal key. Without this, column names containing a
      // dot (e.g. "Q1.A") are read as nested paths row["Q1"]["A"] → blank cells.
      suppressFieldDotNotation: true,
      columnDefs: [],
      defaultColDef: {
        resizable: true,
        sortable: false,
        editable: true,
        minWidth: 80,
      },
      onCellValueChanged: (event) => {
        const rowIndex = event.node.rowIndex;
        if (rowIndex == null) return;
        const col = event.colDef.field ?? '';
        const value = event.newValue;
        // 1-based row number stored as __row__
        const data = event.data as Record<string, unknown>;
        if (!data || !data['__row__']) return;
        const caseNum = data['__row__'] as number;
        window.electron.data.updateCell(caseNum, col, value).then(() => {
          dataStore.setModified(true);
        });
      },
      overlayNoRowsTemplate:
        '<span class="empty-state">No data loaded — use File › Open to load a dataset</span>',
    });

    this.unsub = dataStore.subscribe(() => this.onStoreChange());
  }

  private onStoreChange(): void {
    const state = dataStore.get();
    if (!state.loaded) {
      this.api?.updateGridOptions({
        columnDefs: [],
        datasource: undefined,
      });
      return;
    }
    this.applyDataset(state.variables, state.rowCount);
  }

  private applyDataset(variables: Variable[], rowCount: number): void {
    if (!this.api) return;

    const colDefs: ColDef[] = [
      {
        headerName: '#',
        valueGetter: (p) => p.node?.rowIndex != null ? p.node.rowIndex + 1 : '',
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
          // Show value labels if configured
          const label = v.valueLabels[String(params.value)];
          if (label) return label;
          if (v.type === 'numeric' && v.decimals >= 0) {
            const num = Number(params.value);
            if (isNaN(num)) return params.value;
            // If it's an integer and not "clearly a float", don't show decimals
            if (num % 1 === 0) return num.toFixed(0);
            return num.toFixed(v.decimals);
          }
          return String(params.value);
        },
      })),
    ];

    const datasource: IDatasource = {
      rowCount,
      getRows: (params: IGetRowsParams) => {
        const offset = params.startRow;
        const limit = params.endRow - params.startRow;
        window.electron.data
          .getPage(offset, limit)
          .then(({ rows, total }: { rows: Record<string, unknown>[]; total: number }) => {
            params.successCallback(rows, total);
          })
          .catch((err: unknown) => {
            console.error('[DataView] getPage failed:', err);
            params.failCallback();
          });
      },
    };

    // Keep each fetched page to a sane number of cells regardless of width.
    // ~50k cells/page: 4484 cols → 11 rows, 50 cols → 1000 (capped at 200).
    const cols = Math.max(1, variables.length);
    const blockSize = Math.min(200, Math.max(10, Math.floor(50_000 / cols)));

    this.api.updateGridOptions({
      cacheBlockSize: blockSize,
      columnDefs: colDefs,
      datasource: datasource,
    });

    // Explicitly refresh the cache to ensure the new datasource is used immediately
    this.api.refreshInfiniteCache();
  }

  /** Called externally after variables are refreshed from VariableView edits. */
  refresh(): void {
    const { variables, rowCount } = dataStore.get();
    this.applyDataset(variables, rowCount);
  }

  destroy(): void {
    this.unsub?.();
    this.api?.destroy();
    this.api = null;
  }
}
