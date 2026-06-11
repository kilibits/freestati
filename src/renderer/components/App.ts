import { dataStore } from '../stores/dataStore';
import { DataView } from './DataView';
import { VariableView } from './VariableView';
import { StatusBar } from './StatusBar';

type ActiveView = 'data' | 'variable';

export class App {
  private dataView = new DataView();
  private variableView = new VariableView();
  private statusBar = new StatusBar();
  private activeView: ActiveView = 'data';

  mount(): void {
    const dataContainer = document.getElementById('pane-data')!;
    const varContainer = document.getElementById('pane-variable')!;
    const statusContainer = document.getElementById('status-bar-host')!;

    this.dataView.mount(dataContainer);
    this.variableView.mount(varContainer, () => this.dataView.refresh());
    this.statusBar.mount(statusContainer);

    this.bindTabs();
    this.bindMenuEvents();
    this.bindToolbar();
  }

  // ── Tab switching ─────────────────────────────────────────────────────────

  private bindTabs(): void {
    document.querySelectorAll<HTMLButtonElement>('.tab-btn').forEach(btn => {
      btn.addEventListener('click', () => {
        const view = btn.dataset['view'] as ActiveView;
        this.switchView(view);
      });
    });
  }

  private switchView(view: ActiveView): void {
    this.activeView = view;
    document.querySelectorAll<HTMLButtonElement>('.tab-btn').forEach(btn => {
      btn.classList.toggle('active', btn.dataset['view'] === view);
    });
    const dataPane = document.getElementById('pane-data')!;
    const varPane = document.getElementById('pane-variable')!;
    dataPane.style.display = view === 'data' ? 'flex' : 'none';
    varPane.style.display = view === 'variable' ? 'flex' : 'none';
  }

  // ── Toolbar ───────────────────────────────────────────────────────────────

  private bindToolbar(): void {
    document.getElementById('btn-new')?.addEventListener('click', () => this.newDataset());
    document.getElementById('btn-open')?.addEventListener('click', () => this.openFile());
    document.getElementById('btn-save')?.addEventListener('click', () => this.save());
  }

  // ── Native menu events ────────────────────────────────────────────────────

  private bindMenuEvents(): void {
    window.electron.menu.on('menu:file:new', () => this.newDataset());
    window.electron.menu.on('menu:file:open', () => this.openFile());
    window.electron.menu.on('menu:file:save', () => this.save());
    window.electron.menu.on('menu:file:saveAs', () => this.saveAs());
    window.electron.menu.on('menu:view:dataView', () => this.switchView('data'));
    window.electron.menu.on('menu:view:variableView', () => this.switchView('variable'));
  }

  // ── Actions ───────────────────────────────────────────────────────────────

  private async newDataset(): Promise<void> {
    if (dataStore.get().modified) {
      const ok = confirm('Unsaved changes will be lost. Continue?');
      if (!ok) return;
    }
    await window.electron.python.execute('new_dataset');
    dataStore.reset();
  }

  private async openFile(): Promise<void> {
    const result = await window.electron.file.open();
    if (!result) return;
    if ('error' in result) {
      alert(`Failed to open file:\n${(result as { error: string }).error}`);
      return;
    }
    dataStore.applyLoadResult(result);
    // Switch to Data View to show the loaded data
    this.switchView('data');
  }

  private async save(): Promise<void> {
    const { path } = dataStore.get();
    if (!path) { await this.saveAs(); return; }
    await window.electron.file.save(path);
    dataStore.setModified(false);
  }

  private async saveAs(): Promise<void> {
    const result = await window.electron.file.saveAs();
    if (!result) return;
    dataStore.setModified(false);
  }
}
