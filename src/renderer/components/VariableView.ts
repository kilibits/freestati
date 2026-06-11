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
  { field: 'name', headerName: 'Name', width: 130, editable: true },
  {
    field: 'type',
    headerName: 'Type',
    width: 90,
    editable: true,
    cellEditor: 'agSelectCellEditor',
    cellEditorParams: { values: TYPE_OPTIONS },
  },
  { field: 'width', headerName: 'Width', width: 70, editable: true, type: 'numericColumn' },
  { field: 'decimals', headerName: 'Decimals', width: 80, editable: true, type: 'numericColumn' },
  { field: 'label', headerName: 'Label', width: 250, editable: true },
  {
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
    headerName: 'Missing',
    width: 100,
    valueGetter: (p) => {
      const missing = p.data?.missingValues;
      if (!missing || missing.length === 0) return 'None';
      return missing.join(', ');
    },
  },
  { field: 'columns', headerName: 'Columns', width: 80, editable: true, type: 'numericColumn' },
  {
    field: 'align',
    headerName: 'Align',
    width: 80,
    editable: true,
    cellEditor: 'agSelectCellEditor',
    cellEditorParams: { values: ALIGN_OPTIONS },
  },
  {
    field: 'measureLevel',
    headerName: 'Measure',
    width: 100,
    editable: true,
    cellEditor: 'agSelectCellEditor',
    cellEditorParams: { values: MEASURE_OPTIONS },
  },
  {
    field: 'role',
    headerName: 'Role',
    width: 90,
    editable: true,
    cellEditor: 'agSelectCellEditor',
    cellEditorParams: { values: ROLE_OPTIONS },
  },
];

export class VariableView {
  private api: GridApi<Variable> | null = null;
  private unsub: (() => void) | null = null;
  private onChangeCb?: () => void;

  /** @param onVariableChange - called after any edit so DataView can refresh. */
  mount(container: HTMLElement, onVariableChange?: () => void): void {
    this.onChangeCb = onVariableChange;
    container.classList.add('variable-view-container');

    this.api = createGrid<Variable>(container, {
      theme: themeBalham.withParams({ accentColor: '#6366f1' }),
      rowData: [],
      columnDefs: COL_DEFS,
      defaultColDef: { resizable: true, sortable: false },
      singleClickEdit: true,
      stopEditingWhenCellsLoseFocus: true,
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

  private onStoreChange(): void {
    const { variables } = dataStore.get();
    console.log('[VariableView] onStoreChange, variables:', variables.length);
    this.api?.setGridOption('rowData', [...variables]);
  }

  destroy(): void {
    this.unsub?.();
    this.api?.destroy();
    this.api = null;
  }
}
