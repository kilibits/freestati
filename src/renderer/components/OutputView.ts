import { outputStore } from '../stores/outputStore';
import type { Analysis, ChartData, OutTable, OutputItem } from '../types/analysis';
import { renderChart } from './charts';

/**
 * Output viewer — an SPSS-style results pane. Each procedure run appends an
 * `OutputItem` (a statistical `Analysis` or a `ChartData`) to the `outputStore`;
 * this component re-renders the accumulated list. Tables support copy-to-TSV and
 * a transpose (pivot) toggle; the whole output can be exported to a standalone
 * HTML file. Numbers are formatted for display here so the engine returns raw f64s.
 */
export class OutputView {
  private container!: HTMLElement;
  private body!: HTMLElement;
  private unsub: (() => void) | null = null;

  mount(container: HTMLElement): void {
    this.container = container;
    this.container.classList.add('output-view');

    const toolbar = document.createElement('div');
    toolbar.className = 'view-toolbar output-toolbar';
    toolbar.innerHTML = `
      <button class="output-btn" data-act="export">Export…</button>
      <button class="output-btn" data-act="copyAll">Copy All</button>
      <button class="output-btn" data-act="clear">Clear Output</button>`;
    toolbar.querySelector('[data-act="export"]')!.addEventListener('click', () => this.exportHtml());
    toolbar.querySelector('[data-act="copyAll"]')!.addEventListener('click', () => this.copyAll());
    toolbar.querySelector('[data-act="clear"]')!.addEventListener('click', () => outputStore.clear());
    this.container.appendChild(toolbar);

    this.body = document.createElement('div');
    this.body.className = 'output-body';
    this.container.appendChild(this.body);

    this.unsub = outputStore.subscribe(() => this.render());
    this.render();
  }

  /** Scroll the latest result into view (called when output is appended). */
  scrollToBottom(): void {
    this.body.scrollTop = this.body.scrollHeight;
  }

  private render(): void {
    const items = outputStore.get();
    this.body.innerHTML = '';

    if (items.length === 0) {
      const empty = document.createElement('div');
      empty.className = 'output-empty';
      empty.textContent = 'No output yet — run a procedure from the Analyze or Graphs menu.';
      this.body.appendChild(empty);
      return;
    }
    items.forEach((item) => this.body.appendChild(this.renderItem(item)));
  }

  private renderItem(item: OutputItem): HTMLElement {
    return item.kind === 'analysis'
      ? this.renderAnalysis(item.analysis)
      : this.renderChartBlock(item.chart);
  }

  private renderAnalysis(analysis: Analysis): HTMLElement {
    const block = document.createElement('div');
    block.className = 'output-block';
    block.appendChild(heading(analysis.title));
    analysis.tables.forEach((table) => block.appendChild(this.renderTableWrap(table)));
    return block;
  }

  private renderChartBlock(chart: ChartData): HTMLElement {
    const block = document.createElement('div');
    block.className = 'output-block';

    const head = document.createElement('div');
    head.className = 'output-table-head';
    const title = heading(chart.title);
    title.classList.add('output-chart-title');
    head.appendChild(title);

    const svg = renderChart(chart);
    const actions = document.createElement('span');
    actions.className = 'output-table-actions';
    actions.append(
      miniButton('Save SVG', () => void saveSvg(svg)),
      miniButton('Save PNG', () => void savePng(svg)),
    );
    head.appendChild(actions);
    block.appendChild(head);

    const host = document.createElement('div');
    host.className = 'output-chart-host';
    host.appendChild(svg);
    block.appendChild(host);
    return block;
  }

  private renderTableWrap(table: OutTable): HTMLElement {
    const wrap = document.createElement('div');
    wrap.className = 'output-table-wrap';

    const head = document.createElement('div');
    head.className = 'output-table-head';
    const caption = document.createElement('span');
    caption.className = 'output-table-title';
    caption.textContent = table.title;
    head.appendChild(caption);

    const actions = document.createElement('span');
    actions.className = 'output-table-actions';
    let transposed = false;
    const tableHost = document.createElement('div');

    const copyBtn = miniButton('Copy', () => copyText(tableToTsv(table, transposed)));
    const pivotBtn = miniButton('Transpose', () => {
      transposed = !transposed;
      tableHost.replaceChildren(buildTable(table, transposed));
      pivotBtn.classList.toggle('active', transposed);
    });
    actions.append(copyBtn, pivotBtn);
    head.appendChild(actions);
    wrap.appendChild(head);

    tableHost.appendChild(buildTable(table, transposed));
    wrap.appendChild(tableHost);

    if (table.footnote) {
      const note = document.createElement('div');
      note.className = 'output-footnote';
      note.textContent = table.footnote;
      wrap.appendChild(note);
    }
    return wrap;
  }

  // ── Toolbar actions ──────────────────────────────────────────────────────────

  private async exportHtml(): Promise<void> {
    if (outputStore.get().length === 0) {
      alert('No output to export.');
      return;
    }
    try {
      const path = await window.electron.analysis.exportText(buildExportHtml(this.body));
      if (path) alert(`Output exported to:\n${path}`);
    } catch (err) {
      alert(`Export failed:\n${err}`);
    }
  }

  private copyAll(): void {
    const parts: string[] = [];
    outputStore.get().forEach((item) => {
      if (item.kind === 'analysis') {
        parts.push(item.analysis.title);
        item.analysis.tables.forEach((t) => parts.push(`${t.title}\n${tableToTsv(t, false)}`));
      } else {
        parts.push(item.chart.title);
      }
    });
    copyText(parts.join('\n\n'));
  }

  destroy(): void {
    this.unsub?.();
  }
}

// ── Rendering helpers ────────────────────────────────────────────────────────

function heading(title: string): HTMLElement {
  const h = document.createElement('h2');
  h.className = 'output-heading';
  h.textContent = title;
  return h;
}

function miniButton(label: string, onClick: () => void): HTMLButtonElement {
  const b = document.createElement('button');
  b.className = 'output-mini-btn';
  b.textContent = label;
  b.addEventListener('click', onClick);
  return b;
}

/** Build the <table>, optionally transposed (rows↔columns) as a simple pivot. */
function buildTable(table: OutTable, transposed: boolean): HTMLTableElement {
  const grid = toGrid(table, transposed);
  const tbl = document.createElement('table');
  tbl.className = 'output-table';

  const thead = document.createElement('thead');
  const htr = document.createElement('tr');
  grid.header.forEach((col, i) => {
    const th = document.createElement('th');
    th.textContent = col;
    if (i > 0) th.classList.add('num');
    htr.appendChild(th);
  });
  thead.appendChild(htr);
  tbl.appendChild(thead);

  const tbody = document.createElement('tbody');
  grid.rows.forEach((row) => {
    const tr = document.createElement('tr');
    row.forEach((cell, i) => {
      const td = document.createElement('td');
      if (i === 0) {
        td.textContent = cell == null ? '' : String(cell);
        td.classList.add('label');
      } else {
        td.textContent = formatCell(cell);
        td.classList.add('num');
      }
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  });
  tbl.appendChild(tbody);
  return tbl;
}

type Grid = { header: string[]; rows: (string | number | null)[][] };

/** Normalise an OutTable into a header + body grid, transposing if requested. */
function toGrid(table: OutTable, transposed: boolean): Grid {
  if (!transposed) return { header: table.columns, rows: table.rows };
  // Transpose: original column headers become the first body column; original
  // first-column labels become the new header.
  const header = [table.columns[0] ?? '', ...table.rows.map((r) => String(r[0] ?? ''))];
  const rows: (string | number | null)[][] = [];
  for (let c = 1; c < table.columns.length; c++) {
    const row: (string | number | null)[] = [table.columns[c] ?? ''];
    for (const r of table.rows) row.push(r[c] ?? null);
    rows.push(row);
  }
  return { header, rows };
}

function tableToTsv(table: OutTable, transposed: boolean): string {
  const grid = toGrid(table, transposed);
  const lines = [grid.header.join('\t')];
  grid.rows.forEach((row) => {
    lines.push(row.map((c, i) => (i === 0 ? String(c ?? '') : formatCell(c))).join('\t'));
  });
  return lines.join('\n');
}

/** Build a standalone HTML document of the current output for export. */
function buildExportHtml(body: HTMLElement): string {
  const style = `
    body{font-family:-apple-system,Segoe UI,Roboto,sans-serif;margin:24px;color:#1a1a1a}
    h2{border-bottom:2px solid #6366f1;padding-bottom:4px;font-size:18px}
    table{border-collapse:collapse;margin:8px 0 18px;font-size:13px}
    th,td{border:1px solid #ccc;padding:4px 10px;text-align:right}
    th:first-child,td:first-child{text-align:left}
    .output-footnote{font-size:11px;color:#666;font-style:italic;margin-bottom:16px}
    .output-table-actions{display:none}
    svg{background:#fff}`;
  return `<!DOCTYPE html><html><head><meta charset="utf-8"><title>FreeStati Output</title><style>${style}</style></head><body>${body.innerHTML}</body></html>`;
}

/** Format a numeric cell SPSS-style: integers plain, others to 3 decimals. */
function formatCell(cell: string | number | null): string {
  if (cell == null) return '';
  if (typeof cell === 'string') return cell;
  if (!Number.isFinite(cell)) return '';
  if (Number.isInteger(cell)) return String(cell);
  const rounded = Number(cell.toFixed(3));
  return (Object.is(rounded, -0) ? 0 : rounded).toFixed(3);
}

/** Serialize an SVG element to a standalone document string. */
function svgString(svg: SVGSVGElement): string {
  const clone = svg.cloneNode(true) as SVGSVGElement;
  clone.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
  // Paint a solid background so exported images aren't transparent.
  const bg = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
  bg.setAttribute('width', '100%');
  bg.setAttribute('height', '100%');
  bg.setAttribute('fill', '#1a1d27');
  clone.insertBefore(bg, clone.firstChild);
  return new XMLSerializer().serializeToString(clone);
}

async function saveSvg(svg: SVGSVGElement): Promise<void> {
  try {
    await window.electron.analysis.exportSvg(svgString(svg));
  } catch (err) {
    alert(`Save SVG failed:\n${err}`);
  }
}

/** Rasterize the SVG to a PNG via an offscreen canvas, then write the bytes. */
async function savePng(svg: SVGSVGElement): Promise<void> {
  try {
    const scale = 2; // export at 2× for crispness
    const w = svg.viewBox.baseVal.width || svg.clientWidth || 560;
    const h = svg.viewBox.baseVal.height || svg.clientHeight || 360;
    const url = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svgString(svg))}`;
    const img = new Image();
    await new Promise<void>((resolve, reject) => {
      img.onload = () => resolve();
      img.onerror = () => reject(new Error('Could not rasterize chart'));
      img.src = url;
    });
    const canvas = document.createElement('canvas');
    canvas.width = w * scale;
    canvas.height = h * scale;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('Canvas unavailable');
    ctx.scale(scale, scale);
    ctx.drawImage(img, 0, 0);
    const blob = await new Promise<Blob | null>((res) => canvas.toBlob(res, 'image/png'));
    if (!blob) throw new Error('PNG encoding failed');
    const bytes = Array.from(new Uint8Array(await blob.arrayBuffer()));
    await window.electron.analysis.exportPng(bytes);
  } catch (err) {
    alert(`Save PNG failed:\n${err}`);
  }
}

/** Copy text to the clipboard, falling back to execCommand in older WebViews. */
function copyText(str: string): void {
  if (navigator.clipboard?.writeText) {
    navigator.clipboard.writeText(str).catch(() => fallbackCopy(str));
  } else {
    fallbackCopy(str);
  }
}

function fallbackCopy(str: string): void {
  const ta = document.createElement('textarea');
  ta.value = str;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  try {
    document.execCommand('copy');
  } catch {
    /* ignore */
  }
  ta.remove();
}
