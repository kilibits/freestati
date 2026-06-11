export type VarType = 'numeric' | 'string' | 'date';
export type MeasureLevel = 'scale' | 'ordinal' | 'nominal';
export type VarRole = 'input' | 'target' | 'both' | 'none' | 'partition' | 'split';
export type Alignment = 'left' | 'center' | 'right';

export interface Variable {
  name: string;
  label: string;
  type: VarType;
  width: number;
  decimals: number;
  columns: number;
  align: Alignment;
  valueLabels: Record<string, string>;
  missingValues: (number | string)[];
  measureLevel: MeasureLevel;
  role: VarRole;
}

export interface LoadResult {
  rowCount: number;
  colCount: number;
  variables: Variable[];
  filename: string;
  path: string;
}

export interface DatasetState {
  loaded: boolean;
  filename: string;
  path: string | null;
  rowCount: number;
  colCount: number;
  variables: Variable[];
  modified: boolean;
}
