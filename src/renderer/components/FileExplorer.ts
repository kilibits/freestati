import { dataStore } from '../stores/dataStore';

interface FsEntry {
  name: string;
  path: string;
  isDirectory: boolean;
  ext: string;
}

interface TreeNode extends FsEntry {
  depth: number;
  expanded: boolean;
  loading: boolean;
  children: TreeNode[] | null; // null = not yet loaded
}

const DATA_EXTS = new Set(['.tab', '.tsv', '.csv', '.xlsx', '.xls', '.sav', '.dta', '.sas7bdat']);

const EXT_ICON: Record<string, string> = {
  '.tab': '⬛', '.tsv': '⬛', '.csv': '📋', '.xlsx': '📗', '.xls': '📗',
  '.sav': '📊', '.dta': '📊', '.sas7bdat': '📊',
};

export class FileExplorer {
  private container!: HTMLElement;
  private rootPath: string | null = null;
  private nodes: TreeNode[] = [];
  private listEl!: HTMLElement;
  private headerPathEl!: HTMLElement;
  private onOpenFileCb?: (path: string) => void;
  private loadingRoot = false;

  mount(container: HTMLElement, onOpenFile: (path: string) => void): void {
    this.container = container;
    this.onOpenFileCb = onOpenFile;
    this.container.className = 'file-explorer';
    this.render();
    this.tryRestoreFolder();
  }

  // ── Persistence ────────────────────────────────────────────────────────────

  private tryRestoreFolder(): void {
    const saved = localStorage.getItem('freestati:explorerPath');
    if (saved) this.loadRoot(saved);
  }

  // ── Public API ─────────────────────────────────────────────────────────────

  async openFolderDialog(): Promise<void> {
    const chosen = await window.electron.fs.openFolder();
    if (chosen) this.loadRoot(chosen);
  }

  // ── Data loading ───────────────────────────────────────────────────────────

  private async loadRoot(dirPath: string): Promise<void> {
    this.rootPath = dirPath;
    localStorage.setItem('freestati:explorerPath', dirPath);
    this.loadingRoot = true;
    this.headerPathEl.textContent = dirPath.split('/').pop() ?? dirPath;
    this.headerPathEl.title = dirPath;
    this.renderList();

    try {
      const entries = await window.electron.fs.readDir(dirPath);
      this.nodes = entries.map(e => this.makeNode(e, 0));
    } catch {
      this.nodes = [];
    } finally {
      this.loadingRoot = false;
    }
    this.renderList();
  }

  private async expandNode(node: TreeNode): Promise<void> {
    node.expanded = !node.expanded;
    if (node.expanded && node.children === null) {
      node.loading = true;
      this.renderList();
      try {
        const entries = await window.electron.fs.readDir(node.path);
        node.children = entries.map(e => this.makeNode(e, node.depth + 1));
      } catch {
        node.children = [];
      } finally {
        node.loading = false;
      }
    }
    this.renderList();
  }

  private makeNode(entry: FsEntry, depth: number): TreeNode {
    return { ...entry, depth, expanded: false, loading: false, children: null };
  }

  // ── Rendering ──────────────────────────────────────────────────────────────

  private render(): void {
    this.container.innerHTML = '';

    // Header
    const header = document.createElement('div');
    header.className = 'fe-header';

    this.headerPathEl = document.createElement('span');
    this.headerPathEl.className = 'fe-folder-name';
    this.headerPathEl.textContent = 'No folder open';

    const openBtn = document.createElement('button');
    openBtn.className = 'fe-open-btn';
    openBtn.title = 'Open Folder';
    openBtn.textContent = '⊕';
    openBtn.addEventListener('click', () => this.openFolderDialog());

    header.append(this.headerPathEl, openBtn);
    this.container.appendChild(header);

    // File tree
    this.listEl = document.createElement('div');
    this.listEl.className = 'fe-list';
    this.container.appendChild(this.listEl);
  }

  private renderList(): void {
    this.listEl.innerHTML = '';

    if (this.loadingRoot) {
      const msg = document.createElement('div');
      msg.className = 'fe-message';
      msg.textContent = 'Loading…';
      this.listEl.appendChild(msg);
      return;
    }

    if (!this.rootPath) {
      const msg = document.createElement('div');
      msg.className = 'fe-message';
      msg.textContent = 'Click ⊕ to open a folder';
      this.listEl.appendChild(msg);
      return;
    }

    if (this.nodes.length === 0) {
      const msg = document.createElement('div');
      msg.className = 'fe-message';
      msg.textContent = 'No supported files found';
      this.listEl.appendChild(msg);
      return;
    }

    this.renderNodes(this.nodes);
  }

  private renderNodes(nodes: TreeNode[]): void {
    for (const node of nodes) {
      const row = this.makeRow(node);
      this.listEl.appendChild(row);
      if (node.expanded && node.children) {
        this.renderNodes(node.children);
      }
    }
  }

  private makeRow(node: TreeNode): HTMLElement {
    const row = document.createElement('div');
    row.className = `fe-row${node.isDirectory ? ' fe-dir' : ' fe-file'}`;
    row.style.paddingLeft = `${8 + node.depth * 16}px`;

    // Check if this file is the active dataset
    const active = !node.isDirectory && dataStore.get().path === node.path;
    if (active) row.classList.add('fe-active');

    const icon = document.createElement('span');
    icon.className = 'fe-icon';
    if (node.isDirectory) {
      icon.textContent = node.loading ? '⋯' : node.expanded ? '▾' : '▸';
    } else {
      icon.textContent = EXT_ICON[node.ext] ?? '📄';
    }

    const label = document.createElement('span');
    label.className = 'fe-label';
    label.textContent = node.name;
    label.title = node.path;

    row.append(icon, label);

    if (node.isDirectory) {
      row.addEventListener('click', () => this.expandNode(node));
    } else if (DATA_EXTS.has(node.ext)) {
      row.addEventListener('click', () => this.onOpenFileCb?.(node.path));
      row.addEventListener('dblclick', () => this.onOpenFileCb?.(node.path));
    }

    return row;
  }
}
