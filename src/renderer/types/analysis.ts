/** Result model returned by the Rust `run_analysis` command. */

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
