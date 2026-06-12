import { outputStore } from '../stores/outputStore';
import type { Analysis, OutTable } from '../types/analysis';

/**
 * Output viewer — an SPSS-style results pane. Each procedure run appends an
 * `Analysis` (heading + tables) to the `outputStore`; this component re-renders
 * the accumulated list. Numbers are formatted for display here so the engine can
 * return raw f64s.
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
    toolbar.innerHTML = `<button class="output-clear-btn" title="Clear all output">Clear Output</button>`;
    toolbar.querySelector('button')!.addEventListener('click', () => outputStore.clear());
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
      empty.textContent = 'No output yet — run a procedure from the Analyze menu.';
      this.body.appendChild(empty);
      return;
    }

    items.forEach((analysis) => this.body.appendChild(this.renderAnalysis(analysis)));
  }

  private renderAnalysis(analysis: Analysis): HTMLElement {
    const block = document.createElement('div');
    block.className = 'output-block';

    const heading = document.createElement('h2');
    heading.className = 'output-heading';
    heading.textContent = analysis.title;
    block.appendChild(heading);

    analysis.tables.forEach((table) => block.appendChild(this.renderTable(table)));
    return block;
  }

  private renderTable(table: OutTable): HTMLElement {
    const wrap = document.createElement('div');
    wrap.className = 'output-table-wrap';

    const caption = document.createElement('div');
    caption.className = 'output-table-title';
    caption.textContent = table.title;
    wrap.appendChild(caption);

    const tbl = document.createElement('table');
    tbl.className = 'output-table';

    const thead = document.createElement('thead');
    const htr = document.createElement('tr');
    table.columns.forEach((col, i) => {
      const th = document.createElement('th');
      th.textContent = col;
      if (i > 0) th.classList.add('num');
      htr.appendChild(th);
    });
    thead.appendChild(htr);
    tbl.appendChild(thead);

    const tbody = document.createElement('tbody');
    table.rows.forEach((row) => {
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
    wrap.appendChild(tbl);

    if (table.footnote) {
      const note = document.createElement('div');
      note.className = 'output-footnote';
      note.textContent = table.footnote;
      wrap.appendChild(note);
    }
    return wrap;
  }

  destroy(): void {
    this.unsub?.();
  }
}

/** Format a numeric cell SPSS-style: integers plain, others to 3 decimals. */
function formatCell(cell: string | number | null): string {
  if (cell == null) return '';
  if (typeof cell === 'string') return cell;
  if (!Number.isFinite(cell)) return '';
  if (Number.isInteger(cell)) return String(cell);
  // Very small p-values: SPSS shows ".000"; keep 3 decimals but avoid "-0.000".
  const rounded = Number(cell.toFixed(3));
  return (Object.is(rounded, -0) ? 0 : rounded).toFixed(3);
}
