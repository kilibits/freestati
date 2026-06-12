import { syntaxStore } from '../stores/syntaxStore';
import { runSyntax } from './syntax';

/**
 * Syntax editor — an editable log of the commands you've run, for reproducible
 * analyses. New runs append a line; "Run All" replays the (possibly edited)
 * script. Scripts can be saved to / opened from `.fst` text files.
 */
export class SyntaxView {
  private container!: HTMLElement;
  private textarea!: HTMLTextAreaElement;
  private unsub: (() => void) | null = null;
  private onRan: (() => void) | null = null;

  mount(container: HTMLElement, onRan: () => void): void {
    this.container = container;
    this.onRan = onRan;
    this.container.classList.add('syntax-view');

    const toolbar = document.createElement('div');
    toolbar.className = 'view-toolbar output-toolbar';
    toolbar.innerHTML = `
      <button class="output-btn" data-act="run">▶ Run All</button>
      <button class="output-btn" data-act="open">Open…</button>
      <button class="output-btn" data-act="save">Save…</button>
      <button class="output-btn" data-act="clear">Clear</button>
      <span class="syntax-hint">RUN &lt;procedure&gt; { … }  ·  CHART &lt;kind&gt; { … }</span>`;
    toolbar.querySelector('[data-act="run"]')!.addEventListener('click', () => this.runAll());
    toolbar.querySelector('[data-act="open"]')!.addEventListener('click', () => this.open());
    toolbar.querySelector('[data-act="save"]')!.addEventListener('click', () => this.save());
    toolbar.querySelector('[data-act="clear"]')!.addEventListener('click', () => {
      if (confirm('Clear the syntax?')) syntaxStore.clear();
    });
    this.container.appendChild(toolbar);

    this.textarea = document.createElement('textarea');
    this.textarea.className = 'syntax-editor';
    this.textarea.spellcheck = false;
    this.textarea.placeholder = 'Run a procedure from the Analyze menu and it appears here as a replayable command…';
    // Persist manual edits back to the store when focus leaves.
    this.textarea.addEventListener('blur', () => syntaxStore.setText(this.textarea.value));
    this.container.appendChild(this.textarea);

    this.unsub = syntaxStore.subscribe(() => this.refresh());
    this.refresh();
  }

  /** Sync the textarea from the store unless the user is actively editing it. */
  private refresh(): void {
    if (document.activeElement !== this.textarea) {
      this.textarea.value = syntaxStore.getText();
    }
  }

  private async runAll(): Promise<void> {
    const text = this.textarea.value;
    syntaxStore.setText(text); // persist before replaying
    const { ran, errors } = await runSyntax(text);
    this.onRan?.();
    if (errors.length > 0) {
      alert(`Ran ${ran} command(s).\n\nErrors:\n${errors.join('\n')}`);
    }
  }

  private async save(): Promise<void> {
    try {
      await window.electron.analysis.exportSyntax(this.textarea.value);
    } catch (err) {
      alert(`Save failed:\n${err}`);
    }
  }

  private async open(): Promise<void> {
    try {
      const text = await window.electron.analysis.openSyntax();
      if (text != null) {
        syntaxStore.setText(text);
        this.textarea.value = text;
      }
    } catch (err) {
      alert(`Open failed:\n${err}`);
    }
  }

  destroy(): void {
    this.unsub?.();
  }
}
