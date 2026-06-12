/** Result model returned by the Rust `run_analysis` / `run_chart` commands. */

/** One rendered table: title, column headers, and rows of heterogeneous cells. */
export interface OutTable {
  title: string;
  columns: string[];
  /** Cells are strings (labels), numbers (statistics), or null (n/a). */
  rows: (string | number | null)[][];
  footnote?: string;
}

/** A complete procedure result: a heading plus one or more tables. */
export interface Analysis {
  title: string;
  tables: OutTable[];
}

/** Computed chart data; `payload` shape depends on `kind`. */
export interface ChartData {
  title: string;
  kind: 'histogram' | 'bar' | 'scatter' | 'box';
  xLabel: string;
  yLabel: string;
  payload: Record<string, unknown>;
}

/** An item shown in the Output viewer — a statistical result or a chart. */
export type OutputItem =
  | { kind: 'analysis'; analysis: Analysis }
  | { kind: 'chart'; chart: ChartData };
