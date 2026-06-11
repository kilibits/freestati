import { contextBridge, ipcRenderer } from 'electron';

contextBridge.exposeInMainWorld('electron', {
  // ── App info ──────────────────────────────────────────────────────────────
  getVersion: (): Promise<string> => ipcRenderer.invoke('app:getVersion'),
  getPlatform: (): Promise<string> => ipcRenderer.invoke('app:getPlatform'),
  openExternal: (url: string): Promise<void> => ipcRenderer.invoke('app:openExternal', url),

  // ── File operations ───────────────────────────────────────────────────────
  file: {
    open: () => ipcRenderer.invoke('file:open'),
    save: (filePath: string) => ipcRenderer.invoke('file:save', filePath),
    saveAs: () => ipcRenderer.invoke('file:saveAs'),
  },

  // ── Dataset data access ───────────────────────────────────────────────────
  data: {
    getPage: (offset: number, limit: number) =>
      ipcRenderer.invoke('data:getPage', offset, limit),
    getVariables: () => ipcRenderer.invoke('data:getVariables'),
    setVariableMeta: (varName: string, meta: Record<string, unknown>) =>
      ipcRenderer.invoke('data:setVariableMeta', varName, meta),
    updateCell: (row: number, col: string, value: unknown) =>
      ipcRenderer.invoke('data:updateCell', row, col, value),
  },

  // ── File-system browsing ─────────────────────────────────────────────────
  fs: {
    getHomePath: (): Promise<string> => ipcRenderer.invoke('fs:getHomePath'),
    openFolder: (): Promise<string | null> => ipcRenderer.invoke('fs:openFolder'),
    readDir: (dirPath: string): Promise<Array<{ name: string; path: string; isDirectory: boolean; ext: string }>> =>
      ipcRenderer.invoke('fs:readDir', dirPath),
  },

  // ── Generic analysis pass-through ─────────────────────────────────────────
  python: {
    execute: <T = unknown>(type: string, args?: Record<string, unknown>): Promise<T> =>
      ipcRenderer.invoke('python:execute', type, args ?? {}),
  },

  // ── Native menu events ────────────────────────────────────────────────────
  menu: {
    on: (channel: string, cb: () => void): (() => void) => {
      ipcRenderer.on(channel, cb);
      return () => ipcRenderer.removeListener(channel, cb);
    },
  },

  // ── Auto-updater ──────────────────────────────────────────────────────────
  updater: {
    installUpdate: (): Promise<void> => ipcRenderer.invoke('updater:installUpdate'),
    onUpdateAvailable: (cb: () => void): (() => void) => {
      ipcRenderer.on('updater:updateAvailable', cb);
      return () => ipcRenderer.removeListener('updater:updateAvailable', cb);
    },
    onUpdateDownloaded: (cb: () => void): (() => void) => {
      ipcRenderer.on('updater:updateDownloaded', cb);
      return () => ipcRenderer.removeListener('updater:updateDownloaded', cb);
    },
  },
});
