import {
  ColDef,
  GridApi,
  createGrid,
  themeBalham,
} from 'ag-grid-community';
import { dataStore } from '../stores/dataStore';
import type { Variable, Alignment } from '../types/dataset';

const MEASURE_OPTIONS = ['scale', 'ordinal', 'nominal'];
const ROLE_OPTIONS = ['input', 'target', 'both', 'none', 'partition', 'split'];
const TYPE_OPTIONS = ['numeric', 'string', 'date'];
const ALIGN_OPTIONS: Alignment[] = ['left', 'center', 'right'];

const COL_DEFS: ColDef<Variable>[] = [
  { field: 'name', headerName: 'Name', width: 130, editable: () => dataStore.get().editMode },
  {
    field: 'type',
    headerName: 'Type',
    width: 90,
    editable: () => dataStore.get().editMode,
    cellEditor: 'agSelectCellEditor',
    cellEditorParams: { values: TYPE_OPTIONS },
  },
  { field: 'width', headerName: 'Width', width: 70, editable: () => dataStore.get().editMode, type: 'numericColumn' },
  { field: 'decimals', headerName: 'Decimals', width: 80, editable: () => dataStore.get().editMode, type: 'numericColumn' },
  { field: 'label', headerName: 'Label', width: 250, editable: () => dataStore.get().editMode },
  {
    colId: 'values',
    headerName: 'Values',
    width: 120,
    valueGetter: (p) => {
      const labels = p.data?.valueLabels;
      if (!labels) return 'None';
      const keys = Object.keys(labels);
      if (keys.length === 0) return 'None';
      return `{${keys[0]}=${labels[keys[0]]}, ...}`;
    },
    tooltipValueGetter: (p) => {
      const labels = p.data?.valueLabels;
      if (!labels) return '';
      return Object.entries(labels).map(([k, v]) => `${k}: ${v}`).join('\n');
    },
  },
  {
    colId: 'missing',
    headerName: 'Missing',
    width: 100,
    valueGetter: (p) => {
      const missing = p.data?.missingValues;
      if (!missing || missing.length === 0) return 'None';
      return missing.join(', ');
    },
  },
  { field: 'columns', headerName: 'Columns', width: 80, editable: () => dataStore.get().editMode, type: 'numericColumn' },
  {
    field: 'align',
    headerName: 'Align',
    width: 80,
    editable: () => dataStore.get().editMode,
    cellEditor: 'agSelectCellEditor',
    cellEditorParams: { values: ALIGN_OPTIONS },
  },
  {
    field: 'measureLevel',
    headerName: 'Measure',
    width: 100,
    editable: () => dataStore.get().editMode,
    cellEditor: 'agSelectCellEditor',
    cellEditorParams: { values: MEASURE_OPTIONS },
  },
  {
    field: 'role',
    headerName: 'Role',
    width: 90,
    editable: () => dataStore.get().editMode,
    cellEditor: 'agSelectCellEditor',
    cellEditorParams: { values: ROLE_OPTIONS },
  },
];

export class VariableView {
  private api: GridApi<Variable> | null = null;
  private unsub: (() => void) | null = null;
  private onChangeCb?: () => void;
  private detailPanel: HTMLElement | null = null;

  /** @param onVariableChange - called after any edit so DataView can refresh. */
  mount(container: HTMLElement, onVariableChange?: () => void): void {
    this.onChangeCb = onVariableChange;
    container.classList.add('variable-view-container');

    // Search bar + grid stacked in a left column; detail panel sits to the right.
    const gridCol = document.createElement('div');
    gridCol.className = 'variable-grid-col';

    const toolbar = document.createElement('div');
    toolbar.className = 'view-toolbar';
    toolbar.innerHTML = `
      <div class="search-input-wrapper">
        <span class="search-icon">🔍</span>
        <input type="text" class="search-input" placeholder="Search variables…" />
      </div>
    `;
    const searchInput = toolbar.querySelector('input')!;
    searchInput.addEventListener('input', () => {
      this.api?.setGridOption('quickFilterText', searchInput.value);
    });
    gridCol.appendChild(toolbar);

    const gridDiv = document.createElement('div');
    gridDiv.id = 'variable-grid';
    gridCol.appendChild(gridDiv);
    container.appendChild(gridCol);

    this.detailPanel = document.createElement('div');
    this.detailPanel.id = 'variable-detail';
    this.detailPanel.innerHTML = `
      <div class="variable-detail-header">
        <span class="variable-detail-title">Selection Details</span>
        <button class="detail-collapse-btn" title="Collapse panel" aria-label="Collapse panel">⟩</button>
      </div>
      <div class="variable-detail-content">Select a cell to see full value.</div>`;
    const collapseBtn = this.detailPanel.querySelector('.detail-collapse-btn') as HTMLButtonElement;
    collapseBtn.addEventListener('click', () => this.toggleDetail());
    container.appendChild(this.detailPanel);

    this.api = createGrid<Variable>(gridDiv, {
      theme: themeBalham.withParams({ accentColor: '#6366f1' }),
      rowData: [],
      columnDefs: COL_DEFS,
      defaultColDef: { resizable: true, sortable: false },
      singleClickEdit: true,
      stopEditingWhenCellsLoseFocus: true,
      onCellClicked: (event) => this.onCellFocused(event),
      onCellValueChanged: (event) => {
        const variable = event.data as Variable;
        window.electron.data
          .setVariableMeta(variable.name, { [event.colDef.field!]: event.newValue })
          .then(() => {
            dataStore.setModified(true);
            // Reflect rename
            if (event.colDef.field === 'name') {
              const vars = dataStore.get().variables.map(v =>
                v.name === event.oldValue ? { ...v, name: event.newValue as string } : v,
              );
              dataStore.setVariables(vars);
            }
            this.onChangeCb?.();
          });
      },
      overlayNoRowsTemplate:
        '<span class="empty-state">No variables — open a dataset to see variable metadata</span>',
    });

    this.unsub = dataStore.subscribe(() => this.onStoreChange());
  }

  private toggleDetail(): void {
    if (!this.detailPanel) return;
    const collapsed = this.detailPanel.classList.toggle('collapsed');
    const btn = this.detailPanel.querySelector('.detail-collapse-btn') as HTMLButtonElement | null;
    if (btn) {
      btn.textContent = collapsed ? '⟨' : '⟩';
      const action = collapsed ? 'Expand' : 'Collapse';
      btn.title = `${action} panel`;
      btn.setAttribute('aria-label', `${action} panel`);
    }
  }

  private onCellFocused(event: any): void {
    console.log('Cell focused/clicked event:', event);
    if (!this.detailPanel || !event.column) return;
    
    const rowNode = event.rowNode || event.node;
    if (!rowNode) return;

    const col = event.column.getColDef();
    const data = rowNode.data;
    const field = col.field;
    
    let value = '';
    if (field === 'values') {
        const labels = data.valueLabels;
        value = labels ? Object.entries(labels).map(([k, v]) => `${k}: ${v}`).join('\n') : 'None';
    } else if (field === 'missing') {
        value = data.missingValues ? data.missingValues.join(', ') : 'None';
    } else {
        value = data[field as keyof Variable]?.toString() || '';
    }

    const content = this.detailPanel.querySelector('.variable-detail-content');
    if (content) content.textContent = value;
  }

  private onStoreChange(): void {
    const { variables } = dataStore.get();
    this.api?.setGridOption('rowData', [...variables]);
    this.api?.refreshCells({ force: true });
  }

  destroy(): void {
    this.unsub?.();
    this.api?.destroy();
    this.api = null;
  }
}
