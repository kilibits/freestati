/**
 * Tauri bridge — recreates the `window.electron` API that the renderer
 * components were written against (originally backed by Electron's preload),
 * now backed by Tauri commands, events, and the dialog plugin.
 *
 * Importing this module for its side effect installs `window.electron`, so the
 * existing components (DataView, VariableView, FileExplorer, App, StatusBar)
 * keep working without changes.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';
import { homeDir } from '@tauri-apps/api/path';
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';

import type { LoadResult } from './types/dataset';

const DATA_FILTERS = [
  { name: 'Data Files', extensions: ['tab', 'tsv', 'csv', 'sav', 'zsav'] },
  { name: 'Tab-Delimited', extensions: ['tab', 'tsv'] },
  { name: 'CSV', extensions: ['csv'] },
  { name: 'SPSS', extensions: ['sav', 'zsav'] },
  { name: 'All Files', extensions: ['*'] },
];

const SAVE_FILTERS = [
  { name: 'Tab-Delimited', extensions: ['tab'] },
  { name: 'TSV', extensions: ['tsv'] },
  { name: 'CSV', extensions: ['csv'] },
];

/** Fire a menu listener and return a synchronous unsubscribe wrapper. */
function onChannel(channel: string, cb: () => void): () => void {
  const unlistenPromise = listen(channel, () => cb());
  return () => { void unlistenPromise.then((un) => un()); };
}

const electron = {
  // ── App info ──────────────────────────────────────────────────────────────
  getVersion: (): Promise<string> => getVersion(),
  getPlatform: (): Promise<string> => invoke('get_platform'),
  openExternal: (url: string): Promise<void> => invoke('open_external', { url }),

  // ── File operations ─────────────────────────────────────────────────────────
  file: {
    // Dialog only — returns a path so the caller can show a loading indicator
    // around the (potentially slow) load itself.
    async pickOpenPath(): Promise<string | null> {
      const path = await openDialog({ multiple: false, filters: DATA_FILTERS });
      return typeof path === 'string' ? path : null;
    },
    open(): Promise<LoadResult | null> {
      return this.pickOpenPath().then((path) =>
        path ? invoke<LoadResult>('load_file', { path }) : null,
      );
    },
    save: (path: string): Promise<{ ok: boolean }> => invoke('save_file', { path }),
    async saveAs(): Promise<{ ok: boolean; path: string } | null> {
      const path = await saveDialog({ filters: SAVE_FILTERS });
      if (!path) return null;
      return invoke('save_file', { path });
    },
  },

  // ── Dataset data access ─────────────────────────────────────────────────────
  data: {
    async getPage(
      offset: number,
      limit: number,
      query?: string,
    ): Promise<{ rows: Record<string, unknown>[]; total: number }> {
      const res = await invoke<{ rows_raw: string; total: number }>('get_page', { offset, limit, query });
      return { rows: JSON.parse(res.rows_raw), total: res.total };
    },
    getVariables: () => invoke('get_variables'),
    setVariableMeta: (varName: string, meta: Record<string, unknown>) =>
      invoke('set_variable_meta', { name: varName, meta }),
    updateCell: (row: number, col: string, value: unknown) =>
      invoke('update_cell', { row, col, value }),
  },

  // ── Statistical procedures & charts ─────────────────────────────────────────
  analysis: {
    run: (procedure: string, params: Record<string, unknown>) =>
      invoke('run_analysis', { procedure, params }),
    chart: (kind: string, params: Record<string, unknown>) =>
      invoke('run_chart', { kind, params }),
    async exportText(contents: string): Promise<string | null> {
      const path = await saveDialog({
        filters: [{ name: 'HTML', extensions: ['html'] }],
      });
      if (!path) return null;
      await invoke('save_text_file', { path, contents });
      return path;
    },
    async exportSvg(contents: string): Promise<string | null> {
      const path = await saveDialog({ filters: [{ name: 'SVG Image', extensions: ['svg'] }] });
      if (!path) return null;
      await invoke('save_text_file', { path, contents });
      return path;
    },
    async exportPng(bytes: number[]): Promise<string | null> {
      const path = await saveDialog({ filters: [{ name: 'PNG Image', extensions: ['png'] }] });
      if (!path) return null;
      await invoke('save_binary_file', { path, bytes });
      return path;
    },
  },

  // ── Generic command pass-through (e.g. python.execute('new_dataset')) ───────
  python: {
    execute: <T = unknown>(type: string, args?: Record<string, unknown>): Promise<T> =>
      invoke<T>(type, args ?? {}),
  },

  // ── Filesystem browsing ─────────────────────────────────────────────────────
  fs: {
    getHomePath: (): Promise<string> => homeDir(),
    async openFolder(): Promise<string | null> {
      const dir = await openDialog({ directory: true, multiple: false });
      return typeof dir === 'string' ? dir : null;
    },
    readDir: (path: string) =>
      invoke<Array<{ name: string; path: string; isDirectory: boolean; ext: string }>>('read_dir', { path }),
  },

  // ── Native menu events ──────────────────────────────────────────────────────
  menu: {
    on: (channel: string, cb: () => void): (() => void) => onChannel(channel, cb),
  },

  // ── Auto-updater (Tauri updater not wired yet — no-op stubs) ─────────────────
  updater: {
    installUpdate: (): Promise<void> => Promise.resolve(),
    onUpdateAvailable: (_cb: () => void): (() => void) => () => {},
    onUpdateDownloaded: (_cb: () => void): (() => void) => () => {},
  },
};

// Install the bridge. Cast through unknown because the global type is the full
// window.electron contract declared in global.d.ts.
(window as unknown as { electron: typeof electron }).electron = electron;

export {};
