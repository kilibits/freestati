import type { DatasetState, LoadResult, Variable } from '../types/dataset';

type Listener = () => void;

const DEFAULT: DatasetState = {
  loaded: false,
  filename: '',
  path: null,
  rowCount: 0,
  colCount: 0,
  variables: [],
  modified: false,
  editMode: false,
};

class DataStore {
  private state: DatasetState = { ...DEFAULT };
  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private notify(): void {
    this.listeners.forEach(fn => fn());
  }

  get(): DatasetState {
    return this.state;
  }

  applyLoadResult(result: LoadResult): void {
    this.state = {
      loaded: true,
      filename: result.filename,
      path: result.path,
      rowCount: result.rowCount,
      colCount: result.colCount,
      variables: result.variables,
      modified: false,
      editMode: false, // opened files start read-only
    };
    this.notify();
  }

  setEditMode(editMode: boolean): void {
    if (this.state.editMode === editMode) return;
    this.state = { ...this.state, editMode };
    this.notify();
  }

  reset(): void {
    this.state = { ...DEFAULT };
    this.notify();
  }

  setVariables(variables: Variable[]): void {
    this.state = { ...this.state, variables };
    this.notify();
  }

  setModified(modified: boolean): void {
    if (this.state.modified === modified) return;
    this.state = { ...this.state, modified };
    this.notify();
  }
}

export const dataStore = new DataStore();
