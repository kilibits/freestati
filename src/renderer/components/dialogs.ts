/**
 * Analysis dialogs — SPSS-style variable pickers for each procedure.
 *
 * A small modal scaffold ([`createModal`]) plus a variable mover ([`VarMover`])
 * back the config-driven dialogs in `SPECS`. Two procedures (paired t-test and
 * Wilcoxon) need a pair builder, handled by [`openPairsDialog`]. On OK each
 * dialog calls the Rust `run_analysis` command and appends the result to the
 * output store.
 */
import { dataStore } from '../stores/dataStore';
import { outputStore } from '../stores/outputStore';
import { syntaxStore } from '../stores/syntaxStore';
import { syntaxLine } from './syntax';
import type { Analysis, ChartData } from '../types/analysis';
import type { Variable } from '../types/dataset';

// ── Modal scaffold ───────────────────────────────────────────────────────────

interface Modal {
  root: HTMLElement;
  body: HTMLElement;
  close: () => void;
  /** Wire the OK button. The callback returns false to keep the modal open. */
  onOk: (run: () => boolean | Promise<boolean>) => void;
}

function createModal(title: string): Modal {
  const overlay = document.createElement('div');
  overlay.className = 'dialog-overlay';

  const dialog = document.createElement('div');
  dialog.className = 'dialog';
  dialog.innerHTML = `
    <div class="dialog-title">${escapeHtml(title)}</div>
    <div class="dialog-body"></div>
    <div class="dialog-footer">
      <button class="dialog-btn dialog-ok">OK</button>
      <button class="dialog-btn dialog-cancel">Cancel</button>
    </div>
  `;
  overlay.appendChild(dialog);
  document.body.appendChild(overlay);

  const close = () => overlay.remove();
  overlay.addEventListener('mousedown', (e) => {
    if (e.target === overlay) close();
  });
  dialog.querySelector('.dialog-cancel')!.addEventListener('click', close);

  return {
    root: dialog,
    body: dialog.querySelector('.dialog-body') as HTMLElement,
    close,
    onOk(run) {
      dialog.querySelector('.dialog-ok')!.addEventListener('click', () => {
        void Promise.resolve(run()).then((ok) => {
          if (ok) close();
        });
      });
    },
  };
}

// ── Variable mover (source list ⇄ target slots) ──────────────────────────────

interface Slot {
  key: string;
  label: string;
  /** false = single-variable slot (e.g. grouping variable, dependent). */
  multiple: boolean;
}

class VarMover {
  private source: HTMLSelectElement;
  private targets = new Map<string, HTMLSelectElement>();

  constructor(container: HTMLElement, variables: Variable[], slots: Slot[]) {
    const grid = document.createElement('div');
    grid.className = 'var-mover';

    // Source list.
    const srcWrap = document.createElement('div');
    srcWrap.className = 'var-col';
    srcWrap.innerHTML = `<div class="var-col-label">Variables</div>`;
    this.source = listBox();
    variables.forEach((v) => this.source.appendChild(option(v)));
    srcWrap.appendChild(this.source);
    grid.appendChild(srcWrap);

    // Target slots stacked on the right, each with move buttons.
    const right = document.createElement('div');
    right.className = 'var-targets';
    slots.forEach((slot) => {
      const row = document.createElement('div');
      row.className = 'var-target-row';

      const buttons = document.createElement('div');
      buttons.className = 'var-move-buttons';
      const add = button('→', () => this.move(slot));
      const remove = button('←', () => this.unmove(slot));
      buttons.append(add, remove);

      const col = document.createElement('div');
      col.className = 'var-col';
      col.innerHTML = `<div class="var-col-label">${escapeHtml(slot.label)}</div>`;
      const box = listBox();
      if (!slot.multiple) box.size = 1;
      this.targets.set(slot.key, box);
      col.appendChild(box);

      row.append(buttons, col);
      right.appendChild(row);
    });
    grid.appendChild(right);
    container.appendChild(grid);
  }

  private move(slot: Slot): void {
    const target = this.targets.get(slot.key)!;
    const selected = Array.from(this.source.selectedOptions);
    if (selected.length === 0) return;
    const toMove = slot.multiple ? selected : selected.slice(0, 1);
    // Single-capacity slot: return any existing occupant to the source first.
    if (!slot.multiple) {
      Array.from(target.options).forEach((o) => this.source.appendChild(o));
    }
    toMove.forEach((o) => target.appendChild(o));
    sortOptions(this.source);
  }

  private unmove(slot: Slot): void {
    const target = this.targets.get(slot.key)!;
    Array.from(target.selectedOptions).forEach((o) => this.source.appendChild(o));
    sortOptions(this.source);
  }

  values(key: string): string[] {
    return Array.from(this.targets.get(key)!.options).map((o) => o.value);
  }
}

// ── Dialog specs (config-driven, common shapes) ──────────────────────────────

interface DialogSpec {
  title: string;
  procedure: string;
  slots: Slot[];
  /** Extra option controls appended below the mover. */
  extras?: (body: HTMLElement) => void;
  /** Build params from the mover + body; return an error string to block OK. */
  collect: (mover: VarMover, body: HTMLElement) => Record<string, unknown> | string;
}

const SPECS: Record<string, DialogSpec> = {
  descriptives: {
    title: 'Descriptives',
    procedure: 'descriptives',
    slots: [{ key: 'vars', label: 'Variable(s)', multiple: true }],
    collect: (m) => requireVars(m, 'vars'),
  },
  frequencies: {
    title: 'Frequencies',
    procedure: 'frequencies',
    slots: [{ key: 'vars', label: 'Variable(s)', multiple: true }],
    collect: (m) => requireVars(m, 'vars'),
  },
  chi_square: {
    title: 'Chi-Square Test',
    procedure: 'chi_square',
    slots: [{ key: 'vars', label: 'Test Variable(s)', multiple: true }],
    collect: (m) => requireVars(m, 'vars'),
  },
  ttest_one_sample: {
    title: 'One-Sample T Test',
    procedure: 'ttest_one_sample',
    slots: [{ key: 'vars', label: 'Test Variable(s)', multiple: true }],
    extras: (b) => b.appendChild(numberField('Test Value', 'testValue', '0')),
    collect: (m, b) => {
      const vars = m.values('vars');
      if (vars.length === 0) return 'Select at least one test variable.';
      const raw = (b.querySelector('[data-key="testValue"]') as HTMLInputElement).value;
      if (raw.trim() === '' || Number.isNaN(Number(raw))) return 'Enter a numeric test value.';
      return { vars, testValue: Number(raw) };
    },
  },
  ttest_independent: {
    title: 'Independent-Samples T Test',
    procedure: 'ttest_independent',
    slots: [
      { key: 'vars', label: 'Test Variable(s)', multiple: true },
      { key: 'group', label: 'Grouping Variable', multiple: false },
    ],
    extras: groupValueFields,
    collect: (m, b) => collectGrouped(m, b),
  },
  mann_whitney: {
    title: 'Two Independent Samples (Mann-Whitney U)',
    procedure: 'mann_whitney',
    slots: [
      { key: 'vars', label: 'Test Variable(s)', multiple: true },
      { key: 'group', label: 'Grouping Variable', multiple: false },
    ],
    extras: groupValueFields,
    collect: (m, b) => collectGrouped(m, b),
  },
  anova_oneway: {
    title: 'One-Way ANOVA',
    procedure: 'anova_oneway',
    slots: [
      { key: 'vars', label: 'Dependent List', multiple: true },
      { key: 'factor', label: 'Factor', multiple: false },
    ],
    extras: (b) => {
      const fs = document.createElement('div');
      fs.className = 'dialog-options';
      fs.innerHTML = `
        <div class="dialog-options-label">Post Hoc Comparisons</div>
        <label><input type="radio" name="posthoc" value="none" checked /> None</label>
        <label><input type="radio" name="posthoc" value="lsd" /> LSD</label>
        <label><input type="radio" name="posthoc" value="bonferroni" /> Bonferroni</label>
        <label><input type="radio" name="posthoc" value="tukey" /> Tukey HSD</label>`;
      b.appendChild(fs);
    },
    collect: (m, b) => {
      const base = collectFactor(m);
      if (typeof base === 'string') return base;
      const posthoc =
        (b.querySelector('input[name="posthoc"]:checked') as HTMLInputElement)?.value ?? 'none';
      return { ...base, posthoc };
    },
  },
  crosstabs: {
    title: 'Crosstabs',
    procedure: 'crosstabs',
    slots: [
      { key: 'row', label: 'Row', multiple: false },
      { key: 'col', label: 'Column', multiple: false },
    ],
    collect: (m) => {
      const row = m.values('row');
      const col = m.values('col');
      if (row.length !== 1 || col.length !== 1) return 'Select one row and one column variable.';
      return { row: row[0], col: col[0] };
    },
  },
  reliability: {
    title: 'Reliability Analysis',
    procedure: 'reliability',
    slots: [{ key: 'vars', label: 'Items', multiple: true }],
    collect: (m) => {
      const vars = m.values('vars');
      if (vars.length < 2) return 'Select at least two items.';
      return { vars };
    },
  },
  glm_univariate: {
    title: 'Univariate (General Linear Model)',
    procedure: 'glm_univariate',
    slots: [
      { key: 'dependent', label: 'Dependent Variable', multiple: false },
      { key: 'factors', label: 'Fixed Factor(s)', multiple: true },
      { key: 'covariates', label: 'Covariate(s)', multiple: true },
    ],
    collect: (m) => {
      const dep = m.values('dependent');
      if (dep.length !== 1) return 'Select exactly one dependent variable.';
      const factors = m.values('factors');
      const covariates = m.values('covariates');
      if (factors.length === 0 && covariates.length === 0)
        return 'Add at least one factor or covariate.';
      return { dependent: dep[0], factors, covariates };
    },
  },
  glm_multivariate: {
    title: 'Multivariate (MANOVA)',
    procedure: 'glm_multivariate',
    slots: [
      { key: 'dependents', label: 'Dependent Variables', multiple: true },
      { key: 'factor', label: 'Fixed Factor', multiple: false },
    ],
    collect: (m) => {
      const dependents = m.values('dependents');
      const factor = m.values('factor');
      if (dependents.length < 2) return 'Select at least two dependent variables.';
      if (factor.length !== 1) return 'Select one fixed factor.';
      return { dependents, factor: factor[0] };
    },
  },
  mixed_model: {
    title: 'Linear Mixed Model (random intercept)',
    procedure: 'mixed_model',
    slots: [
      { key: 'dependent', label: 'Dependent Variable', multiple: false },
      { key: 'subject', label: 'Subject / Grouping (random)', multiple: false },
      { key: 'covariates', label: 'Covariate(s)', multiple: true },
    ],
    collect: (m) => {
      const dep = m.values('dependent');
      const subject = m.values('subject');
      if (dep.length !== 1) return 'Select exactly one dependent variable.';
      if (subject.length !== 1) return 'Select one subject/grouping variable.';
      return { dependent: dep[0], subject: subject[0], covariates: m.values('covariates') };
    },
  },
  survival_km: {
    title: 'Kaplan-Meier Survival',
    procedure: 'survival_km',
    slots: [
      { key: 'time', label: 'Time', multiple: false },
      { key: 'status', label: 'Status', multiple: false },
      { key: 'factor', label: 'Factor (optional)', multiple: false },
    ],
    extras: (b) => b.appendChild(numberField('Event value (status =)', 'eventValue', '1')),
    collect: (m, b) => {
      const time = m.values('time');
      const status = m.values('status');
      if (time.length !== 1) return 'Select one time variable.';
      if (status.length !== 1) return 'Select one status variable.';
      const eventValue = (b.querySelector('[data-key="eventValue"]') as HTMLInputElement).value.trim();
      if (eventValue === '') return 'Enter the value of the status variable that marks an event.';
      const factor = m.values('factor');
      const params: Record<string, unknown> = { time: time[0], status: status[0], eventValue };
      if (factor.length === 1) params['factor'] = factor[0];
      return params;
    },
  },
  cox_regression: {
    title: 'Cox Regression',
    procedure: 'cox_regression',
    slots: [
      { key: 'time', label: 'Time', multiple: false },
      { key: 'status', label: 'Status', multiple: false },
      { key: 'covariates', label: 'Covariates', multiple: true },
    ],
    extras: (b) => b.appendChild(numberField('Event value (status =)', 'eventValue', '1')),
    collect: (m, b) => {
      const time = m.values('time');
      const status = m.values('status');
      const covariates = m.values('covariates');
      if (time.length !== 1) return 'Select one time variable.';
      if (status.length !== 1) return 'Select one status variable.';
      if (covariates.length === 0) return 'Select at least one covariate.';
      const eventValue = (b.querySelector('[data-key="eventValue"]') as HTMLInputElement).value.trim();
      if (eventValue === '') return 'Enter the value of the status variable that marks an event.';
      return { time: time[0], status: status[0], covariates, eventValue };
    },
  },
  glm_repeated: {
    title: 'Repeated Measures',
    procedure: 'glm_repeated',
    slots: [{ key: 'vars', label: 'Within-Subject Levels', multiple: true }],
    collect: (m) => {
      const vars = m.values('vars');
      if (vars.length < 2) return 'Select at least two within-subject levels.';
      return { vars };
    },
  },
  factor: {
    title: 'Factor Analysis',
    procedure: 'factor',
    slots: [{ key: 'vars', label: 'Variables', multiple: true }],
    extras: (b) => {
      const fs = document.createElement('div');
      fs.className = 'dialog-options';
      fs.innerHTML = `
        <div class="dialog-options-label">Rotation</div>
        <label><input type="radio" name="rotation" value="none" checked /> None</label>
        <label><input type="radio" name="rotation" value="varimax" /> Varimax</label>`;
      const fixed = document.createElement('label');
      fixed.className = 'dialog-field';
      fixed.innerHTML = `<span>Fixed number of factors (blank = Kaiser)</span>`;
      const input = document.createElement('input');
      input.type = 'text';
      input.dataset['key'] = 'factors';
      input.className = 'dialog-input';
      fixed.appendChild(input);
      fs.appendChild(fixed);
      b.appendChild(fs);
    },
    collect: (m, b) => {
      const vars = m.values('vars');
      if (vars.length < 2) return 'Select at least two variables.';
      const rotation =
        (b.querySelector('input[name="rotation"]:checked') as HTMLInputElement)?.value ?? 'none';
      const raw = (b.querySelector('[data-key="factors"]') as HTMLInputElement).value.trim();
      const params: Record<string, unknown> = { vars, rotation };
      if (raw !== '') {
        const n = Number(raw);
        if (!Number.isInteger(n) || n < 1) return 'Fixed number of factors must be a positive integer.';
        params['factors'] = n;
      }
      return params;
    },
  },
  kruskal_wallis: {
    title: 'K Independent Samples (Kruskal-Wallis)',
    procedure: 'kruskal_wallis',
    slots: [
      { key: 'vars', label: 'Test Variable List', multiple: true },
      { key: 'factor', label: 'Grouping Variable', multiple: false },
    ],
    collect: (m) => collectFactor(m),
  },
  correlate: {
    title: 'Bivariate Correlations',
    procedure: 'correlate',
    slots: [{ key: 'vars', label: 'Variables', multiple: true }],
    extras: (b) => {
      const fs = document.createElement('div');
      fs.className = 'dialog-options';
      fs.innerHTML = `
        <div class="dialog-options-label">Correlation Coefficients</div>
        <label><input type="radio" name="corr-method" value="pearson" checked /> Pearson</label>
        <label><input type="radio" name="corr-method" value="spearman" /> Spearman</label>`;
      b.appendChild(fs);
    },
    collect: (m, b) => {
      const vars = m.values('vars');
      if (vars.length < 2) return 'Select at least two variables.';
      const method =
        (b.querySelector('input[name="corr-method"]:checked') as HTMLInputElement)?.value ??
        'pearson';
      return { vars, method };
    },
  },
  regression_linear: {
    title: 'Linear Regression',
    procedure: 'regression_linear',
    slots: [
      { key: 'dependent', label: 'Dependent', multiple: false },
      { key: 'independents', label: 'Independent(s)', multiple: true },
    ],
    collect: (m) => {
      const dep = m.values('dependent');
      const indep = m.values('independents');
      if (dep.length !== 1) return 'Select exactly one dependent variable.';
      if (indep.length === 0) return 'Select at least one independent variable.';
      return { dependent: dep[0], independents: indep };
    },
  },
};

// ── Public entry point ───────────────────────────────────────────────────────

/** Open the dialog for `procedure`. `onDone` runs after a result is produced. */
export function openProcedureDialog(procedure: string, onDone: () => void): void {
  if (!dataStore.get().loaded) {
    alert('Open a dataset before running an analysis.');
    return;
  }
  if (procedure === 'ttest_paired') {
    openPairsDialog('ttest_paired', 'Paired-Samples T Test', onDone);
    return;
  }
  if (procedure === 'wilcoxon') {
    openPairsDialog('wilcoxon', 'Two Related Samples (Wilcoxon)', onDone);
    return;
  }
  const spec = SPECS[procedure];
  if (!spec) {
    alert(`Procedure not available: ${procedure}`);
    return;
  }
  openSpecDialog(spec, onDone);
}

// ── Chart dialogs (Graphs menu) ──────────────────────────────────────────────

interface ChartSpec {
  title: string;
  kind: string;
  slots: Slot[];
  collect: (mover: VarMover) => Record<string, unknown> | string;
}

const CHART_SPECS: Record<string, ChartSpec> = {
  histogram: {
    title: 'Histogram',
    kind: 'histogram',
    slots: [{ key: 'var', label: 'Variable', multiple: false }],
    collect: (m) => requireOne(m, 'var'),
  },
  bar: {
    title: 'Bar Chart',
    kind: 'bar',
    slots: [{ key: 'var', label: 'Category Axis', multiple: false }],
    collect: (m) => requireOne(m, 'var'),
  },
  clustered_bar: {
    title: 'Clustered Bar Chart',
    kind: 'clustered_bar',
    slots: [
      { key: 'var', label: 'Category Axis', multiple: false },
      { key: 'cluster', label: 'Cluster By', multiple: false },
    ],
    collect: (m) => {
      const v = m.values('var');
      const cluster = m.values('cluster');
      if (v.length !== 1 || cluster.length !== 1) return 'Select a category and a cluster variable.';
      return { var: v[0], cluster: cluster[0] };
    },
  },
  line: {
    title: 'Line Chart',
    kind: 'line',
    slots: [{ key: 'vars', label: 'Variable(s)', multiple: true }],
    collect: (m) => {
      const vars = m.values('vars');
      if (vars.length === 0) return 'Select at least one variable.';
      return { vars };
    },
  },
  scatter: {
    title: 'Scatter Plot',
    kind: 'scatter',
    slots: [
      { key: 'y', label: 'Y Axis', multiple: false },
      { key: 'x', label: 'X Axis', multiple: false },
    ],
    collect: (m) => {
      const x = m.values('x');
      const y = m.values('y');
      if (x.length !== 1 || y.length !== 1) return 'Select one X and one Y variable.';
      return { x: x[0], y: y[0] };
    },
  },
  box: {
    title: 'Box Plot',
    kind: 'box',
    slots: [
      { key: 'vars', label: 'Variable(s)', multiple: true },
      { key: 'group', label: 'Category Axis (optional)', multiple: false },
    ],
    collect: (m) => {
      const vars = m.values('vars');
      if (vars.length === 0) return 'Select at least one variable.';
      const group = m.values('group');
      const params: Record<string, unknown> = { vars };
      if (group.length === 1) params['group'] = group[0];
      return params;
    },
  },
};

/** Open the chart dialog for `kind` (histogram/bar/scatter/box). */
export function openChartDialog(kind: string, onDone: () => void): void {
  if (!dataStore.get().loaded) {
    alert('Open a dataset before drawing a chart.');
    return;
  }
  const spec = CHART_SPECS[kind];
  if (!spec) {
    alert(`Chart not available: ${kind}`);
    return;
  }
  const modal = createModal(spec.title);
  const mover = new VarMover(modal.body, dataStore.get().variables, spec.slots);
  modal.onOk(async () => {
    const params = spec.collect(mover);
    if (typeof params === 'string') {
      alert(params);
      return false;
    }
    try {
      const chart = await window.electron.analysis.chart(spec.kind, params);
      outputStore.appendChart(chart as ChartData);
      syntaxStore.append(syntaxLine('CHART', spec.kind, params));
      onDone();
      return true;
    } catch (err) {
      alert(`Chart failed:\n${err}`);
      return false;
    }
  });
}

function requireOne(m: VarMover, key: string): Record<string, unknown> | string {
  const v = m.values(key);
  if (v.length !== 1) return 'Select one variable.';
  return { [key]: v[0] };
}

function openSpecDialog(spec: DialogSpec, onDone: () => void): void {
  const variables = dataStore.get().variables;
  const modal = createModal(spec.title);
  const mover = new VarMover(modal.body, variables, spec.slots);
  spec.extras?.(modal.body);

  modal.onOk(async () => {
    const params = spec.collect(mover, modal.body);
    if (typeof params === 'string') {
      alert(params);
      return false;
    }
    return runProcedure(spec.procedure, params, onDone);
  });
}

// ── Pairs dialog (paired t-test, Wilcoxon) ───────────────────────────────────

function openPairsDialog(procedure: string, title: string, onDone: () => void): void {
  const variables = dataStore.get().variables;
  const modal = createModal(title);

  const wrap = document.createElement('div');
  wrap.className = 'pairs-dialog';
  wrap.innerHTML = `
    <div class="pairs-pickers">
      <div class="var-col">
        <div class="var-col-label">Variable 1</div>
      </div>
      <div class="var-col">
        <div class="var-col-label">Variable 2</div>
      </div>
    </div>
    <button class="dialog-btn pairs-add">Add Pair →</button>
    <div class="var-col">
      <div class="var-col-label">Paired Variables</div>
    </div>`;
  const cols = wrap.querySelectorAll('.var-col');
  const sel1 = listBox();
  sel1.size = 6;
  const sel2 = listBox();
  sel2.size = 6;
  variables.forEach((v) => {
    sel1.appendChild(option(v));
    sel2.appendChild(option(v));
  });
  cols[0].appendChild(sel1);
  cols[1].appendChild(sel2);

  const pairsBox = listBox();
  pairsBox.size = 5;
  cols[2].appendChild(pairsBox);

  const pairs: [string, string][] = [];
  wrap.querySelector('.pairs-add')!.addEventListener('click', () => {
    const a = sel1.value;
    const b = sel2.value;
    if (!a || !b || a === b) {
      alert('Pick two different variables.');
      return;
    }
    pairs.push([a, b]);
    const opt = document.createElement('option');
    opt.textContent = `${a} — ${b}`;
    pairsBox.appendChild(opt);
  });
  // Double-click a pair to remove it.
  pairsBox.addEventListener('dblclick', () => {
    const i = pairsBox.selectedIndex;
    if (i >= 0) {
      pairs.splice(i, 1);
      pairsBox.remove(i);
    }
  });
  modal.body.appendChild(wrap);

  modal.onOk(async () => {
    if (pairs.length === 0) {
      alert('Add at least one pair.');
      return false;
    }
    return runProcedure(procedure, { pairs }, onDone);
  });
}

// ── Shared collect helpers ───────────────────────────────────────────────────

function requireVars(m: VarMover, key: string): Record<string, unknown> | string {
  const vars = m.values(key);
  if (vars.length === 0) return 'Select at least one variable.';
  return { [key]: vars };
}

function collectFactor(m: VarMover): Record<string, unknown> | string {
  const vars = m.values('vars');
  const factor = m.values('factor');
  if (vars.length === 0) return 'Select at least one test variable.';
  if (factor.length !== 1) return 'Select one grouping/factor variable.';
  return { vars, factor: factor[0] };
}

function collectGrouped(m: VarMover, b: HTMLElement): Record<string, unknown> | string {
  const vars = m.values('vars');
  const group = m.values('group');
  if (vars.length === 0) return 'Select at least one test variable.';
  if (group.length !== 1) return 'Select one grouping variable.';
  const g1 = (b.querySelector('[data-key="group1"]') as HTMLInputElement).value.trim();
  const g2 = (b.querySelector('[data-key="group2"]') as HTMLInputElement).value.trim();
  if (g1 === '' || g2 === '') return 'Enter both group values.';
  return { vars, group: group[0], group1: g1, group2: g2 };
}

function groupValueFields(b: HTMLElement): void {
  const fs = document.createElement('div');
  fs.className = 'dialog-options';
  fs.innerHTML = `<div class="dialog-options-label">Define Groups</div>`;
  fs.appendChild(numberRow('Group 1 value', 'group1'));
  fs.appendChild(numberRow('Group 2 value', 'group2'));
  b.appendChild(fs);
}

// ── Execution ────────────────────────────────────────────────────────────────

async function runProcedure(
  procedure: string,
  params: Record<string, unknown>,
  onDone: () => void,
): Promise<boolean> {
  try {
    const result = await window.electron.analysis.run(procedure, params);
    outputStore.appendAnalysis(result as Analysis);
    syntaxStore.append(syntaxLine('RUN', procedure, params));
    onDone();
    return true;
  } catch (err) {
    alert(`Analysis failed:\n${err}`);
    return false;
  }
}

// ── Small DOM helpers ────────────────────────────────────────────────────────

function listBox(): HTMLSelectElement {
  const sel = document.createElement('select');
  sel.multiple = true;
  sel.className = 'var-list';
  sel.size = 8;
  return sel;
}

function option(v: Variable): HTMLOptionElement {
  const o = document.createElement('option');
  o.value = v.name;
  o.textContent = v.label ? `${v.label} [${v.name}]` : v.name;
  return o;
}

function sortOptions(sel: HTMLSelectElement): void {
  const opts = Array.from(sel.options);
  opts.sort((a, b) => a.text.localeCompare(b.text));
  opts.forEach((o) => sel.appendChild(o));
}

function button(label: string, onClick: () => void): HTMLButtonElement {
  const btn = document.createElement('button');
  btn.className = 'var-move-btn';
  btn.textContent = label;
  btn.addEventListener('click', onClick);
  return btn;
}

function numberField(label: string, key: string, def: string): HTMLElement {
  const wrap = document.createElement('div');
  wrap.className = 'dialog-options';
  wrap.appendChild(numberRow(label, key, def));
  return wrap;
}

function numberRow(label: string, key: string, def = ''): HTMLElement {
  const row = document.createElement('label');
  row.className = 'dialog-field';
  row.innerHTML = `<span>${escapeHtml(label)}</span>`;
  const input = document.createElement('input');
  input.type = 'text';
  input.dataset['key'] = key;
  input.value = def;
  input.className = 'dialog-input';
  row.appendChild(input);
  return row;
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
