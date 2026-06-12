import type { LoadResult, Variable } from './types/dataset';
import type { Analysis, ChartData } from './types/analysis';

// This file imports types, making it a module. To augment the global `Window`
// from a module the declaration must live inside `declare global`.
declare global {
  interface Window {
    electron: {
    getVersion(): Promise<string>;
    getPlatform(): Promise<string>;
    openExternal(url: string): Promise<void>;

    file: {
      pickOpenPath(): Promise<string | null>;
      open(): Promise<LoadResult | null>;
      save(filePath: string): Promise<{ ok: boolean }>;
      saveAs(): Promise<{ ok: boolean; path: string } | null>;
    };

    data: {
      getPage(offset: number, limit: number, search?: string): Promise<{ rows: Record<string, unknown>[]; total: number }>;
      getVariables(): Promise<{ variables: Variable[] }>;
      setVariableMeta(varName: string, meta: Partial<Variable>): Promise<{ ok: boolean }>;
      updateCell(row: number, col: string, value: unknown): Promise<{ ok: boolean }>;
    };

    analysis: {
      run(procedure: string, params: Record<string, unknown>): Promise<Analysis>;
      chart(kind: string, params: Record<string, unknown>): Promise<ChartData>;
      exportText(contents: string): Promise<string | null>;
      exportSvg(contents: string): Promise<string | null>;
      exportPng(bytes: number[]): Promise<string | null>;
    };

    fs: {
      getHomePath(): Promise<string>;
      openFolder(): Promise<string | null>;
      readDir(dirPath: string): Promise<Array<{ name: string; path: string; isDirectory: boolean; ext: string }>>;
    };

    python: {
      execute<T = unknown>(type: string, args?: Record<string, unknown>): Promise<T>;
    };

    menu: {
      on(channel: string, cb: () => void): () => void;
    };

    updater: {
      installUpdate(): Promise<void>;
      onUpdateAvailable(cb: () => void): () => void;
      onUpdateDownloaded(cb: () => void): () => void;
    };
    };
  }
}

export {};
