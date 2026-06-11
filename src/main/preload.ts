import { contextBridge, ipcRenderer } from 'electron';

// Expose a safe, narrow API surface to the renderer via window.electron
contextBridge.exposeInMainWorld('electron', {
  getVersion: (): Promise<string> => ipcRenderer.invoke('app:getVersion'),
  getPlatform: (): Promise<string> => ipcRenderer.invoke('app:getPlatform'),
  openExternal: (url: string): Promise<void> => ipcRenderer.invoke('app:openExternal', url),

  updater: {
    installUpdate: (): Promise<void> => ipcRenderer.invoke('updater:installUpdate'),
    onUpdateAvailable: (cb: () => void) => {
      ipcRenderer.on('updater:updateAvailable', cb);
      return () => ipcRenderer.removeListener('updater:updateAvailable', cb);
    },
    onUpdateDownloaded: (cb: () => void) => {
      ipcRenderer.on('updater:updateDownloaded', cb);
      return () => ipcRenderer.removeListener('updater:updateDownloaded', cb);
    },
  },
});
