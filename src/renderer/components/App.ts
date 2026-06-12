import { dataStore } from '../stores/dataStore';
import { DataView } from './DataView';
import { FileExplorer } from './FileExplorer';
import { OutputView } from './OutputView';
import { StatusBar } from './StatusBar';
import { VariableView } from './VariableView';
import { openChartDialog, openProcedureDialog } from './dialogs';

type ActiveView = 'data' | 'variable' | 'output';

/** Procedures wired to "menu:analyze:<id>" events from the native menu. */
const ANALYZE_PROCEDURES = [
  'frequencies',
  'descriptives',
  'crosstabs',
  'ttest_one_sample',
  'ttest_independent',
  'ttest_paired',
  'anova_oneway',
  'correlate',
  'regression_linear',
  'factor',
  'mann_whitney',
  'wilcoxon',
  'kruskal_wallis',
  'chi_square',
];

/** Charts wired to "menu:graph:<kind>" events from the native menu. */
const GRAPH_KINDS = ['histogram', 'bar', 'scatter', 'box'];

export class App {
  private dataView = new DataView();
  private variableView = new VariableView();
  private outputView = new OutputView();
  private statusBar = new StatusBar();
  private fileExplorer = new FileExplorer();
  private activeView: ActiveView = 'data';
  private sidebarOpen = true;
  private loadingTimer: number | null = null;

  mount(): void {
    this.dataView.mount(document.getElementById('pane-data')!);
    this.variableView.mount(document.getElementById('pane-variable')!, () => this.dataView.refresh());
    this.outputView.mount(document.getElementById('pane-output')!);
    this.statusBar.mount(document.getElementById('status-bar-host')!);
    this.fileExplorer.mount(document.getElementById('sidebar')!, (path) => this.openFilePath(path));

    this.bindTabs();
    this.bindMenuEvents();
    this.bindAnalyzeMenu();
    this.bindToolbar();
    this.restoreSidebarState();
  }

  // ── Analyze menu ────────────────────────────────────────────────────────────

  private bindAnalyzeMenu(): void {
    const showOutput = () => {
      this.switchView('output');
      this.outputView.scrollToBottom();
    };
    ANALYZE_PROCEDURES.forEach((proc) => {
      window.electron.menu.on(`menu:analyze:${proc}`, () => openProcedureDialog(proc, showOutput));
    });
    GRAPH_KINDS.forEach((kind) => {
      window.electron.menu.on(`menu:graph:${kind}`, () => openChartDialog(kind, showOutput));
    });
  }

  // ── Sidebar ───────────────────────────────────────────────────────────────

  private restoreSidebarState(): void {
    const saved = localStorage.getItem('freestati:sidebarOpen');
    this.sidebarOpen = saved !== 'false';
    this.applySidebarState();
  }

  private toggleSidebar(): void {
    this.sidebarOpen = !this.sidebarOpen;
    localStorage.setItem('freestati:sidebarOpen', String(this.sidebarOpen));
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
    document.getElementById('pane-output')!.style.display = view === 'output' ? 'flex' : 'none';
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

  // ── Loading overlay ─────────────────────────────────────────────────────────

  /** Show after a short delay so fast operations don't flash the overlay. */
  private showLoading(message: string, delayMs = 150): void {
    const msgEl = document.getElementById('loading-message');
    if (msgEl) msgEl.textContent = message;
    this.clearLoadingTimer();
    this.loadingTimer = window.setTimeout(() => {
      document.getElementById('loading-overlay')?.classList.remove('hidden');
    }, delayMs);
  }

  private hideLoading(): void {
    this.clearLoadingTimer();
    document.getElementById('loading-overlay')?.classList.add('hidden');
  }

  private clearLoadingTimer(): void {
    if (this.loadingTimer !== null) {
      clearTimeout(this.loadingTimer);
      this.loadingTimer = null;
    }
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
    // Pick the path first (no overlay during the native dialog), then load.
    const path = await window.electron.file.pickOpenPath();
    if (path) await this.loadPath(path);
  }

  private openFilePath(filePath: string): Promise<void> {
    return this.loadPath(filePath);
  }

  /** Shared load path with a loading indicator and error handling. */
  private async loadPath(filePath: string): Promise<void> {
    const name = filePath.split(/[\\/]/).pop() ?? filePath;
    this.showLoading(`Opening ${name}…`);
    try {
      const result = await window.electron.python.execute<
        import('../types/dataset').LoadResult & { error?: string }
      >('load_file', { path: filePath });
      if (result.error) { alert(`Failed to open file:\n${result.error}`); return; }
      dataStore.applyLoadResult(result);
      this.switchView('data');
    } catch (err) {
      alert(`Failed to open file:\n${err}`);
    } finally {
      this.hideLoading();
    }
  }

  private async save(): Promise<void> {
    const { path } = dataStore.get();
    if (!path) { await this.saveAs(); return; }
    this.showLoading('Saving…');
    try {
      await window.electron.file.save(path);
      dataStore.setModified(false);
    } catch (err) {
      alert(`Failed to save file:\n${err}`);
    } finally {
      this.hideLoading();
    }
  }

  private async saveAs(): Promise<void> {
    const result = await window.electron.file.saveAs();
    if (result) dataStore.setModified(false);
  }
}
