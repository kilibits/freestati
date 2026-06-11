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
      cacheBlockSize: 100,
      maxBlocksInCache: 20,
      infiniteInitialRowCount: 0,
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
        const caseNum = (event.data as Record<string, unknown>)['__row__'] as number;
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
      this.api?.setGridOption('columnDefs', []);
      this.api?.setGridOption('datasource', undefined);
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
            return isNaN(num) ? params.value : num.toFixed(v.decimals);
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
          .then(({ rows, total }) => {
            params.successCallback(rows, total);
          })
          .catch((err) => {
            console.error('[DataView] getPage failed:', err);
            params.failCallback();
          });
      },
    };

    this.api.setGridOption('columnDefs', colDefs);
    // Setting a new datasource resets the infinite cache and triggers the first load
    this.api.setGridOption('datasource', datasource);
    this.api.purgeInfiniteCache();
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
