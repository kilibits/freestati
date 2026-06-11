import { BrowserWindow, dialog, ipcMain } from 'electron';
import { pythonBridge } from './pythonBridge';

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
  ipcMain.handle('data:getPage', (_event, offset: number, limit: number) =>
    pythonBridge.execute('get_page', { offset, limit }),
  );

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
}
