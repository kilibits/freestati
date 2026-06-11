import { BrowserWindow, dialog, ipcMain } from 'electron';
import { promises as fsp } from 'fs';
import { homedir } from 'os';
import path from 'path';
import { pythonBridge } from './pythonBridge';

const DATA_EXTS = new Set(['.tab', '.tsv', '.csv', '.xlsx', '.xls', '.sav', '.dta', '.sas7bdat']);

export function registerFileHandlers(getWin: () => BrowserWindow | null): void {
  ipcMain.handle('file:open', async () => {
    const win = getWin();
    const { filePaths, canceled } = await dialog.showOpenDialog(win ?? ({} as BrowserWindow), {
      title: 'Open Data File',
      filters: [
        {
          name: 'Supported Data Files',
          extensions: ['tab', 'tsv', 'csv', 'xlsx', 'xls', 'sav', 'dta', 'sas7bdat'],
        },
        { name: 'Tab-Delimited (.tab, .tsv)', extensions: ['tab', 'tsv'] },
        { name: 'CSV (.csv)', extensions: ['csv'] },
        { name: 'Excel (.xlsx, .xls)', extensions: ['xlsx', 'xls'] },
        { name: 'SPSS (.sav)', extensions: ['sav'] },
        { name: 'Stata (.dta)', extensions: ['dta'] },
        { name: 'SAS (.sas7bdat)', extensions: ['sas7bdat'] },
        { name: 'All Files', extensions: ['*'] },
      ],
      properties: ['openFile'],
    });
    if (canceled || !filePaths[0]) return null;
    return pythonBridge.execute('load_file', { path: filePaths[0] });
  });

  ipcMain.handle('file:save', (_event, filePath: string) =>
    pythonBridge.execute('save_file', { path: filePath }),
  );

  ipcMain.handle('file:saveAs', async () => {
    const win = getWin();
    const { filePath, canceled } = await dialog.showSaveDialog(win ?? ({} as BrowserWindow), {
      title: 'Save Data File',
      filters: [
        { name: 'Tab-Delimited (.tab)', extensions: ['tab'] },
        { name: 'CSV (.csv)', extensions: ['csv'] },
        { name: 'Excel (.xlsx)', extensions: ['xlsx'] },
        { name: 'SPSS (.sav)', extensions: ['sav'] },
      ],
    });
    if (canceled || !filePath) return null;
    return pythonBridge.execute('save_file', { path: filePath });
  });

  // ── Data paging (drives AG Grid infinite row model) ───────────────────────
  // Python returns rows_raw (a JSON string from Polars' Rust serializer) to
  // avoid Python dict allocation.  We parse it here in the main process so
  // the renderer receives the normal { rows, total } shape.
  ipcMain.handle('data:getPage', async (_event, offset: number, limit: number) => {
    const result = await pythonBridge.execute<{ rows_raw?: string; rows?: unknown[]; total: number }>(
      'get_page', { offset, limit },
    );
    if (result?.rows_raw != null) {
      result.rows = JSON.parse(result.rows_raw);
      delete result.rows_raw;
    }
    return result;
  });

  ipcMain.handle('data:getVariables', () =>
    pythonBridge.execute('get_variables', {}),
  );

  ipcMain.handle('data:setVariableMeta', (_event, varName: string, meta: Record<string, unknown>) =>
    pythonBridge.execute('set_variable_meta', { varName, meta }),
  );

  ipcMain.handle('data:updateCell', (_event, row: number, col: string, value: unknown) =>
    pythonBridge.execute('update_cell', { row, col, value }),
  );

  // ── Generic pass-through for analysis commands ────────────────────────────
  ipcMain.handle('python:execute', (_event, type: string, args: Record<string, unknown>) =>
    pythonBridge.execute(type, args),
  );

  // ── File-system browsing (for the sidebar explorer) ───────────────────────
  ipcMain.handle('fs:getHomePath', () => homedir());

  ipcMain.handle('fs:openFolder', async () => {
    const win = getWin();
    const { filePaths, canceled } = await dialog.showOpenDialog(
      win ?? ({} as BrowserWindow),
      { title: 'Open Folder', properties: ['openDirectory'] },
    );
    return canceled ? null : (filePaths[0] ?? null);
  });

  ipcMain.handle('fs:readDir', async (_event, dirPath: string) => {
    const entries = await fsp.readdir(dirPath, { withFileTypes: true });
    return entries
      .filter(e => e.isDirectory() || DATA_EXTS.has(path.extname(e.name).toLowerCase()))
      .sort((a, b) => {
        // Directories first, then files alphabetically
        if (a.isDirectory() !== b.isDirectory()) return a.isDirectory() ? -1 : 1;
        return a.name.localeCompare(b.name);
      })
      .map(e => ({
        name: e.name,
        path: path.join(dirPath, e.name),
        isDirectory: e.isDirectory(),
        ext: path.extname(e.name).toLowerCase(),
      }));
  });
}
