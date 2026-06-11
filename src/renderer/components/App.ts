import { dataStore } from '../stores/dataStore';
import { DataView } from './DataView';
import { FileExplorer } from './FileExplorer';
import { StatusBar } from './StatusBar';
import { VariableView } from './VariableView';

type ActiveView = 'data' | 'variable';

export class App {
  private dataView = new DataView();
  private variableView = new VariableView();
  private statusBar = new StatusBar();
  private fileExplorer = new FileExplorer();
  private activeView: ActiveView = 'data';
  private sidebarOpen = true;

  mount(): void {
    this.dataView.mount(document.getElementById('pane-data')!);
    this.variableView.mount(document.getElementById('pane-variable')!, () => this.dataView.refresh());
    this.statusBar.mount(document.getElementById('status-bar-host')!);
    this.fileExplorer.mount(document.getElementById('sidebar')!, (path) => this.openFilePath(path));

    this.bindTabs();
    this.bindMenuEvents();
    this.bindToolbar();
    this.restoreSidebarState();
  }

  // ── Sidebar ───────────────────────────────────────────────────────────────

  private restoreSidebarState(): void {
    const saved = localStorage.getItem('freestats:sidebarOpen');
    this.sidebarOpen = saved !== 'false';
    this.applySidebarState();
  }

  private toggleSidebar(): void {
    this.sidebarOpen = !this.sidebarOpen;
    localStorage.setItem('freestats:sidebarOpen', String(this.sidebarOpen));
    this.applySidebarState();
  }

  private applySidebarState(): void {
    const layout = document.getElementById('layout')!;
    const btn = document.getElementById('btn-explorer') as HTMLButtonElement;
    layout.classList.toggle('sidebar-hidden', !this.sidebarOpen);
    if (btn) btn.classList.toggle('active', this.sidebarOpen);
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
    document.getElementById('pane-data')!.style.display = view === 'data' ? 'flex' : 'none';
    document.getElementById('pane-variable')!.style.display = view === 'variable' ? 'flex' : 'none';
  }

  // ── Toolbar ───────────────────────────────────────────────────────────────

  private bindToolbar(): void {
    document.getElementById('btn-explorer')?.addEventListener('click', () => this.toggleSidebar());
    document.getElementById('btn-open-folder')?.addEventListener('click', () => {
      if (!this.sidebarOpen) this.toggleSidebar();
      this.fileExplorer.openFolderDialog();
    });
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
    window.electron.menu.on('menu:view:explorer', () => this.toggleSidebar());
  }

  // ── Actions ───────────────────────────────────────────────────────────────

  private async newDataset(): Promise<void> {
    if (dataStore.get().modified) {
      if (!confirm('Unsaved changes will be lost. Continue?')) return;
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
    this.switchView('data');
  }

  private async openFilePath(filePath: string): Promise<void> {
    try {
      const result = await window.electron.python.execute<
        import('../types/dataset').LoadResult & { error?: string }
      >('load_file', { path: filePath });
      if (result.error) { alert(`Error: ${result.error}`); return; }
      dataStore.applyLoadResult(result);
      this.switchView('data');
    } catch (err) {
      alert(`Failed to open file:\n${err}`);
    }
  }

  private async save(): Promise<void> {
    const { path } = dataStore.get();
    if (!path) { await this.saveAs(); return; }
    await window.electron.file.save(path);
    dataStore.setModified(false);
  }

  private async saveAs(): Promise<void> {
    const result = await window.electron.file.saveAs();
    if (result) dataStore.setModified(false);
  }
}
