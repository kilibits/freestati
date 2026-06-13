import { dataStore } from '../stores/dataStore';
import { outputStore } from '../stores/outputStore';
import { DataView } from './DataView';
import { FileExplorer } from './FileExplorer';
import { OutputView } from './OutputView';
import { StatusBar } from './StatusBar';
import { SyntaxView } from './SyntaxView';
import { VariableView } from './VariableView';
import { openChartDialog, openProcedureDialog } from './dialogs';

type ActiveView = 'data' | 'variable' | 'output' | 'syntax';

/** Procedures wired to "menu:analyze:<id>" events from the native menu. */
const ANALYZE_PROCEDURES = [
  'frequencies',
  'descriptives',
  'crosstabs',
  'ttest_one_sample',
  'ttest_independent',
  'ttest_paired',
  'anova_oneway',
  'glm_univariate',
  'glm_multivariate',
  'glm_repeated',
  'mixed_model',
  'correlate',
  'regression_linear',
  'factor',
  'reliability',
  'survival_km',
  'cox_regression',
  'mann_whitney',
  'wilcoxon',
  'kruskal_wallis',
  'chi_square',
];

/** Charts wired to "menu:graph:<kind>" events from the native menu. */
const GRAPH_KINDS = ['histogram', 'bar', 'clustered_bar', 'line', 'scatter', 'box'];

/** Human-readable labels for the toolbar dropdowns (match the menu wording). */
const PROC_LABELS: Record<string, string> = {
  frequencies: 'Frequencies',
  descriptives: 'Descriptives',
  crosstabs: 'Crosstabs',
  correlate: 'Correlate',
  regression_linear: 'Linear Regression',
  ttest_one_sample: 'One-Sample T Test',
  ttest_independent: 'Independent-Samples T Test',
  ttest_paired: 'Paired-Samples T Test',
  anova_oneway: 'One-Way ANOVA',
  glm_univariate: 'GLM Univariate',
  glm_multivariate: 'GLM Multivariate (MANOVA)',
  glm_repeated: 'Repeated Measures',
  mixed_model: 'Linear Mixed Model',
  factor: 'Factor Analysis',
  reliability: 'Reliability Analysis',
  survival_km: 'Kaplan-Meier',
  cox_regression: 'Cox Regression',
  mann_whitney: 'Mann-Whitney U',
  wilcoxon: 'Wilcoxon',
  kruskal_wallis: 'Kruskal-Wallis',
  chi_square: 'Chi-Square',
};

const GRAPH_LABELS: Record<string, string> = {
  histogram: 'Histogram',
  bar: 'Bar Chart',
  clustered_bar: 'Clustered Bar Chart',
  line: 'Line Chart',
  scatter: 'Scatter Plot',
  box: 'Box Plot',
};

export class App {
  private dataView = new DataView();
  private variableView = new VariableView();
  private outputView = new OutputView();
  private syntaxView = new SyntaxView();
  private statusBar = new StatusBar();
  private fileExplorer = new FileExplorer();
  private activeView: ActiveView = 'data';
  private sidebarOpen = true;
  private loadingTimer: number | null = null;

  mount(): void {
    this.dataView.mount(document.getElementById('pane-data')!);
    this.variableView.mount(document.getElementById('pane-variable')!, () => this.dataView.refresh());
    this.outputView.mount(document.getElementById('pane-output')!);
    this.syntaxView.mount(document.getElementById('pane-syntax')!, () => {
      this.switchView('output');
      this.outputView.scrollToBottom();
    });
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
    layout.classList.toggle('sidebar-hidden', !this.sidebarOpen);
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
    document.getElementById('pane-syntax')!.style.display = view === 'syntax' ? 'flex' : 'none';
  }

  // ── Toolbar ───────────────────────────────────────────────────────────────

  private bindToolbar(): void {
    const toggle = document.getElementById('btn-edit-toggle') as HTMLButtonElement | null;
    toggle?.addEventListener('click', () => {
      const { loaded, editMode } = dataStore.get();
      if (!loaded) return;
      dataStore.setEditMode(!editMode);
    });
    // Reflect edit-mode state on the toggle button.
    dataStore.subscribe(() => this.refreshEditToggle());
    this.refreshEditToggle();

    this.renderAnalyzeToolbar();
  }

  private renderAnalyzeToolbar(): void {
    const toolbar = document.getElementById('analyze-toolbar');
    if (!toolbar) return;

    const showOutput = () => {
      this.switchView('output');
      this.outputView.scrollToBottom();
    };

    const categories = [
      { name: 'Statistics', procs: ['frequencies', 'descriptives', 'correlate', 'regression_linear', 'crosstabs'] },
      { name: 'Compare Means', procs: ['ttest_one_sample', 'ttest_independent', 'ttest_paired', 'anova_oneway'] },
      { name: 'Advanced Models', procs: ['glm_univariate', 'glm_multivariate', 'glm_repeated', 'mixed_model', 'factor', 'reliability', 'survival_km', 'cox_regression'] },
      { name: 'Nonparametric', procs: ['mann_whitney', 'wilcoxon', 'kruskal_wallis', 'chi_square'] },
    ];

    // Each dropdown lives in a wrapper span. The select is truly `disabled`
    // when no dataset is loaded; the tooltip lives on the wrapper because a
    // disabled control receives no hover events (so its own title never shows).
    const wrappers: HTMLElement[] = [];
    const addDropdown = (label: string, entries: [string, string][], onPick: (v: string) => void) => {
      const wrap = document.createElement('span');
      wrap.className = 'toolbar-item';
      const select = document.createElement('select');
      select.className = 'toolbar-btn';
      select.innerHTML = `<option value="">${label} ▾</option>`;
      for (const [value, text] of entries) {
        const option = document.createElement('option');
        option.value = value;
        option.textContent = text;
        select.appendChild(option);
      }
      select.addEventListener('change', () => {
        if (select.value) onPick(select.value);
        select.value = '';
      });
      wrap.appendChild(select);
      toolbar.appendChild(wrap);
      wrappers.push(wrap);
    };

    categories.forEach((cat) =>
      addDropdown(
        cat.name,
        cat.procs.map((p) => [p, PROC_LABELS[p] ?? p]),
        (v) => openProcedureDialog(v, showOutput),
      ),
    );
    addDropdown(
      'Graphs',
      GRAPH_KINDS.map((k) => [k, GRAPH_LABELS[k] ?? k]),
      (v) => openChartDialog(v, showOutput),
    );

    const updateDisabledState = () => {
      const { loaded } = dataStore.get();
      wrappers.forEach((wrap) => {
        const select = wrap.querySelector('select')!;
        select.disabled = !loaded;
        wrap.classList.toggle('disabled', !loaded);
        wrap.title = loaded ? '' : 'Open a dataset to enable analysis tools';
      });
    };

    dataStore.subscribe(updateDisabledState);
    updateDisabledState();
  }

  private refreshEditToggle(): void {
    const wrap = document.getElementById('edit-toggle-wrap');
    const toggle = document.getElementById('btn-edit-toggle') as HTMLButtonElement | null;
    if (!toggle || !wrap) return;
    const { loaded, editMode } = dataStore.get();
    toggle.disabled = !loaded;
    toggle.textContent = editMode ? '🔓 Editing' : '🔒 Read-only';
    toggle.classList.toggle('active', editMode);
    wrap.classList.toggle('disabled', !loaded);
    // Tooltip on the wrapper so it still shows while the button is disabled.
    const tip = !loaded
      ? 'Open a dataset to enable editing'
      : editMode
        ? 'Editing enabled — click to make the dataset read-only'
        : 'Dataset is read-only — click to allow editing';
    wrap.title = tip;
    toggle.title = loaded ? tip : '';
  }

  // ── Native menu events ────────────────────────────────────────────────────

  private bindMenuEvents(): void {
    window.electron.menu.on('menu:file:new', () => this.newDataset());
    window.electron.menu.on('menu:file:open', () => this.openFile());
    window.electron.menu.on('menu:file:openFolder', () => {
      if (!this.sidebarOpen) this.toggleSidebar();
      this.fileExplorer.openFolderDialog();
    });
    window.electron.menu.on('menu:file:save', () => this.save());
    window.electron.menu.on('menu:file:saveAs', () => this.saveAs());
    window.electron.menu.on('menu:view:dataView', () => this.switchView('data'));
    window.electron.menu.on('menu:view:variableView', () => this.switchView('variable'));
    window.electron.menu.on('menu:view:output', () => this.switchView('output'));
    window.electron.menu.on('menu:view:syntax', () => this.switchView('syntax'));
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
    dataStore.setEditMode(true); // a fresh dataset is editable so you can enter data
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
      outputStore.clear();
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
