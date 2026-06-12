/**
 * Dependency-free SVG chart renderer for the Output viewer. The Rust engine
 * pre-aggregates the data (bins, category counts, five-number summaries, sampled
 * scatter points); this module only maps numbers to coordinates and draws.
 */
import type { ChartData } from '../types/analysis';

const NS = 'http://www.w3.org/2000/svg';
const W = 560;
const H = 360;
const M = { top: 28, right: 20, bottom: 56, left: 56 };
const PLOT_W = W - M.left - M.right;
const PLOT_H = H - M.top - M.bottom;

const COLORS = {
  accent: '#6366f1',
  accentFaint: 'rgba(99, 102, 241, 0.45)',
  text: '#e2e8f0',
  muted: '#64748b',
  grid: '#2a2d3e',
  axis: '#3d4466',
};

/** Qualitative palette for multi-series charts (line, clustered bar). */
const PALETTE = ['#6366f1', '#10b981', '#f59e0b', '#ef4444', '#06b6d4', '#a855f7', '#84cc16', '#ec4899'];

/** Render a chart to an SVG element. */
export function renderChart(chart: ChartData): SVGSVGElement {
  const svg = el('svg', {
    width: String(W),
    height: String(H),
    viewBox: `0 0 ${W} ${H}`,
    class: 'output-chart',
  }) as SVGSVGElement;

  switch (chart.kind) {
    case 'histogram':
      drawHistogram(svg, chart);
      break;
    case 'bar':
      drawBar(svg, chart);
      break;
    case 'scatter':
      drawScatter(svg, chart);
      break;
    case 'box':
      drawBox(svg, chart);
      break;
    case 'line':
      drawLine(svg, chart);
      break;
    case 'clustered_bar':
      drawClusteredBar(svg, chart);
      break;
  }
  return svg;
}

// ── Chart types ──────────────────────────────────────────────────────────────

function drawHistogram(svg: SVGSVGElement, chart: ChartData): void {
  const bins = (chart.payload['bins'] as { x0: number; x1: number; count: number }[]) ?? [];
  if (bins.length === 0) return;
  const maxCount = Math.max(...bins.map((b) => b.count), 1);
  const xMin = bins[0]!.x0;
  const xMax = bins[bins.length - 1]!.x1;
  const valueLabels = chart.payload['valueLabels'] as Record<string, string> | undefined;

  axes(svg, chart, xMin, xMax, 0, maxCount);
  const sx = (v: number) => M.left + ((v - xMin) / (xMax - xMin || 1)) * PLOT_W;
  const sy = (v: number) => M.top + PLOT_H - (v / maxCount) * PLOT_H;

  bins.forEach((b) => {
    const x = sx(b.x0);
    const w = Math.max(1, sx(b.x1) - sx(b.x0) - 1);
    const y = sy(b.count);
    svg.appendChild(
      el('rect', {
        x: String(x),
        y: String(y),
        width: String(w),
        height: String(M.top + PLOT_H - y),
        fill: COLORS.accentFaint,
        stroke: COLORS.accent,
      }),
    );
    
    // Attempt to show a label if the bin midpoint matches a defined value label.
    if (valueLabels) {
      const mid = (b.x0 + b.x1) / 2;
      const label = valueLabels[String(mid)];
      if (label) {
        svg.appendChild(
          text(x + w / 2, M.top + PLOT_H + 16, truncate(label, 10), {
            'text-anchor': 'middle',
            fill: COLORS.muted,
            'font-size': '10',
          }),
        );
      }
    }
  });
}

function drawBar(svg: SVGSVGElement, chart: ChartData): void {
  const cats = (chart.payload['categories'] as { label: string; count: number }[]) ?? [];
  if (cats.length === 0) return;
  const maxCount = Math.max(...cats.map((c) => c.count), 1);
  axes(svg, chart, 0, 1, 0, maxCount, /*numericX*/ false);

  const sy = (v: number) => M.top + PLOT_H - (v / maxCount) * PLOT_H;
  const slot = PLOT_W / cats.length;
  const bw = Math.min(slot * 0.7, 60);
  cats.forEach((c, i) => {
    const cx = M.left + slot * (i + 0.5);
    const y = sy(c.count);
    svg.appendChild(
      el('rect', {
        x: String(cx - bw / 2),
        y: String(y),
        width: String(bw),
        height: String(M.top + PLOT_H - y),
        fill: COLORS.accentFaint,
        stroke: COLORS.accent,
      }),
    );
    svg.appendChild(
      text(cx, M.top + PLOT_H + 16, truncate(c.label, 10), {
        'text-anchor': 'middle',
        fill: COLORS.muted,
        'font-size': '10',
      }),
    );
  });
}

function drawScatter(svg: SVGSVGElement, chart: ChartData): void {
  const points = (chart.payload['points'] as [number, number][]) ?? [];
  if (points.length === 0) return;
  const xs = points.map((p) => p[0]);
  const ys = points.map((p) => p[1]);
  const xMin = Math.min(...xs);
  const xMax = Math.max(...xs);
  const yMin = Math.min(...ys);
  const yMax = Math.max(...ys);
  axes(svg, chart, xMin, xMax, yMin, yMax);
  const sx = (v: number) => M.left + ((v - xMin) / (xMax - xMin || 1)) * PLOT_W;
  const sy = (v: number) => M.top + PLOT_H - ((v - yMin) / (yMax - yMin || 1)) * PLOT_H;

  points.forEach(([x, y]) => {
    svg.appendChild(
      el('circle', { cx: String(sx(x)), cy: String(sy(y)), r: '2.5', fill: COLORS.accentFaint }),
    );
  });

  // Least-squares fit line.
  const fit = chart.payload['fit'] as { slope: number; intercept: number } | undefined;
  if (fit && Number.isFinite(fit.slope)) {
    const y0 = fit.intercept + fit.slope * xMin;
    const y1 = fit.intercept + fit.slope * xMax;
    svg.appendChild(
      el('line', {
        x1: String(sx(xMin)),
        y1: String(sy(y0)),
        x2: String(sx(xMax)),
        y2: String(sy(y1)),
        stroke: COLORS.accent,
        'stroke-width': '1.5',
      }),
    );
    const r = chart.payload['r'] as number | undefined;
    if (typeof r === 'number') {
      svg.appendChild(
        text(W - M.right, M.top + 4, `r = ${r.toFixed(3)}`, {
          'text-anchor': 'end',
          fill: COLORS.muted,
          'font-size': '10',
        }),
      );
    }
  }
}

function drawBox(svg: SVGSVGElement, chart: ChartData): void {
  type Box = {
    label: string;
    min: number;
    q1: number;
    median: number;
    q3: number;
    max: number;
    outliers: number[];
  };
  const boxes = (chart.payload['boxes'] as Box[]) ?? [];
  if (boxes.length === 0) return;
  const all = boxes.flatMap((b) => [b.min, b.max, ...b.outliers]);
  const yMin = Math.min(...all);
  const yMax = Math.max(...all);
  axes(svg, chart, 0, 1, yMin, yMax, /*numericX*/ false);
  const sy = (v: number) => M.top + PLOT_H - ((v - yMin) / (yMax - yMin || 1)) * PLOT_H;

  const slot = PLOT_W / boxes.length;
  const bw = Math.min(slot * 0.5, 56);
  boxes.forEach((b, i) => {
    const cx = M.left + slot * (i + 0.5);
    // Whiskers.
    line(svg, cx, sy(b.min), cx, sy(b.q1));
    line(svg, cx, sy(b.q3), cx, sy(b.max));
    line(svg, cx - bw / 4, sy(b.min), cx + bw / 4, sy(b.min));
    line(svg, cx - bw / 4, sy(b.max), cx + bw / 4, sy(b.max));
    // Box.
    svg.appendChild(
      el('rect', {
        x: String(cx - bw / 2),
        y: String(sy(b.q3)),
        width: String(bw),
        height: String(Math.max(1, sy(b.q1) - sy(b.q3))),
        fill: COLORS.accentFaint,
        stroke: COLORS.accent,
      }),
    );
    // Median.
    line(svg, cx - bw / 2, sy(b.median), cx + bw / 2, sy(b.median), COLORS.text, 1.5);
    // Outliers.
    b.outliers.forEach((o) => {
      svg.appendChild(el('circle', { cx: String(cx), cy: String(sy(o)), r: '2.5', fill: 'none', stroke: COLORS.muted }));
    });
    svg.appendChild(
      text(cx, M.top + PLOT_H + 16, truncate(b.label, 12), {
        'text-anchor': 'middle',
        fill: COLORS.muted,
        'font-size': '10',
      }),
    );
  });
}

function drawLine(svg: SVGSVGElement, chart: ChartData): void {
  type Series = { label: string; points: [number, number][] };
  const series = (chart.payload['series'] as Series[]) ?? [];
  const pts = series.flatMap((s) => s.points);
  if (pts.length === 0) return;
  const xs = pts.map((p) => p[0]);
  const ys = pts.map((p) => p[1]);
  const xMin = Math.min(...xs);
  const xMax = Math.max(...xs);
  const yMin = Math.min(...ys);
  const yMax = Math.max(...ys);
  axes(svg, chart, xMin, xMax, yMin, yMax);
  const sx = (v: number) => M.left + ((v - xMin) / (xMax - xMin || 1)) * PLOT_W;
  const sy = (v: number) => M.top + PLOT_H - ((v - yMin) / (yMax - yMin || 1)) * PLOT_H;

  series.forEach((s, i) => {
    const color = PALETTE[i % PALETTE.length]!;
    const d = s.points.map((p, j) => `${j === 0 ? 'M' : 'L'}${sx(p[0])},${sy(p[1])}`).join(' ');
    svg.appendChild(el('path', { d, fill: 'none', stroke: color, 'stroke-width': '1.5' }));
  });
  if (series.length > 1) legend(svg, series.map((s) => s.label));
}

function drawClusteredBar(svg: SVGSVGElement, chart: ChartData): void {
  type Series = { label: string; counts: number[] };
  const categories = (chart.payload['categories'] as string[]) ?? [];
  const series = (chart.payload['series'] as Series[]) ?? [];
  if (categories.length === 0 || series.length === 0) return;
  const maxCount = Math.max(...series.flatMap((s) => s.counts), 1);
  axes(svg, chart, 0, 1, 0, maxCount, /*numericX*/ false);
  const sy = (v: number) => M.top + PLOT_H - (v / maxCount) * PLOT_H;

  const slot = PLOT_W / categories.length;
  const groupW = Math.min(slot * 0.8, 72);
  const barW = groupW / series.length;
  categories.forEach((cat, ci) => {
    const cx = M.left + slot * (ci + 0.5);
    const x0 = cx - groupW / 2;
    series.forEach((s, si) => {
      const color = PALETTE[si % PALETTE.length]!;
      const y = sy(s.counts[ci] ?? 0);
      svg.appendChild(
        el('rect', {
          x: String(x0 + si * barW),
          y: String(y),
          width: String(Math.max(1, barW - 1)),
          height: String(M.top + PLOT_H - y),
          fill: color,
        }),
      );
    });
    svg.appendChild(
      text(cx, M.top + PLOT_H + 16, truncate(cat, 10), {
        'text-anchor': 'middle',
        fill: COLORS.muted,
        'font-size': '10',
      }),
    );
  });
  legend(svg, series.map((s) => s.label));
}

/** Top-right legend swatches for multi-series charts. */
function legend(svg: SVGSVGElement, labels: string[]): void {
  labels.forEach((label, i) => {
    const y = M.top + 4 + i * 15;
    const color = PALETTE[i % PALETTE.length]!;
    svg.appendChild(el('rect', { x: String(W - M.right - 110), y: String(y - 8), width: '10', height: '10', fill: color }));
    svg.appendChild(
      text(W - M.right - 95, y, truncate(label, 16), { fill: COLORS.text, 'font-size': '10' }),
    );
  });
}

// ── Axes & helpers ───────────────────────────────────────────────────────────

function axes(
  svg: SVGSVGElement,
  chart: ChartData,
  xMin: number,
  xMax: number,
  yMin: number,
  yMax: number,
  numericX = true,
): void {
  // Y grid + ticks.
  const yTicks = niceTicks(yMin, yMax, 5);
  yTicks.forEach((t) => {
    const y = M.top + PLOT_H - ((t - yMin) / (yMax - yMin || 1)) * PLOT_H;
    svg.appendChild(el('line', { x1: String(M.left), y1: String(y), x2: String(M.left + PLOT_W), y2: String(y), stroke: COLORS.grid }));
    svg.appendChild(text(M.left - 6, y + 3, fmt(t), { 'text-anchor': 'end', fill: COLORS.muted, 'font-size': '10' }));
  });

  // Axis lines.
  svg.appendChild(el('line', { x1: String(M.left), y1: String(M.top), x2: String(M.left), y2: String(M.top + PLOT_H), stroke: COLORS.axis }));
  svg.appendChild(el('line', { x1: String(M.left), y1: String(M.top + PLOT_H), x2: String(M.left + PLOT_W), y2: String(M.top + PLOT_H), stroke: COLORS.axis }));

  // X ticks for numeric axes (categorical axes label their own bars).
  if (numericX) {
    const xTicks = niceTicks(xMin, xMax, 5);
    xTicks.forEach((t) => {
      const x = M.left + ((t - xMin) / (xMax - xMin || 1)) * PLOT_W;
      svg.appendChild(text(x, M.top + PLOT_H + 16, fmt(t), { 'text-anchor': 'middle', fill: COLORS.muted, 'font-size': '10' }));
    });
  }

  // Axis labels.
  if (chart.xLabel) {
    svg.appendChild(text(M.left + PLOT_W / 2, H - 8, chart.xLabel, { 'text-anchor': 'middle', fill: COLORS.text, 'font-size': '11' }));
  }
  if (chart.yLabel) {
    const t = text(14, M.top + PLOT_H / 2, chart.yLabel, { 'text-anchor': 'middle', fill: COLORS.text, 'font-size': '11' });
    t.setAttribute('transform', `rotate(-90 14 ${M.top + PLOT_H / 2})`);
    svg.appendChild(t);
  }
}

function line(svg: SVGSVGElement, x1: number, y1: number, x2: number, y2: number, stroke = COLORS.accent, width = 1): void {
  svg.appendChild(el('line', { x1: String(x1), y1: String(y1), x2: String(x2), y2: String(y2), stroke, 'stroke-width': String(width) }));
}

function el(tag: string, attrs: Record<string, string>): SVGElement {
  const node = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
  return node;
}

function text(x: number, y: number, str: string, attrs: Record<string, string>): SVGElement {
  const node = el('text', { x: String(x), y: String(y), ...attrs });
  node.textContent = str;
  return node;
}

/** "Nice" evenly-spaced tick values spanning [min, max]. */
function niceTicks(min: number, max: number, count: number): number[] {
  if (!Number.isFinite(min) || !Number.isFinite(max) || min === max) return [min];
  const range = max - min;
  const rawStep = range / count;
  const mag = Math.pow(10, Math.floor(Math.log10(rawStep)));
  const norm = rawStep / mag;
  const step = (norm >= 5 ? 5 : norm >= 2 ? 2 : 1) * mag;
  const start = Math.ceil(min / step) * step;
  const ticks: number[] = [];
  for (let t = start; t <= max + step * 0.5; t += step) ticks.push(t);
  return ticks;
}

function fmt(v: number): string {
  if (!Number.isFinite(v)) return '';
  if (Number.isInteger(v)) return String(v);
  return Number(v.toFixed(3)).toString();
}

function truncate(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}
