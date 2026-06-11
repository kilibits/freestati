import { app, BrowserWindow, ipcMain, Menu, MenuItemConstructorOptions, shell } from 'electron';
import { autoUpdater } from 'electron-updater';
import path from 'path';
import { pythonBridge } from './ipc/pythonBridge';
import { registerFileHandlers } from './ipc/fileHandlers';

const isDev = !app.isPackaged;
let mainWindow: BrowserWindow | null = null;

// ── Window ────────────────────────────────────────────────────────────────────

function createWindow(): BrowserWindow {
  mainWindow = new BrowserWindow({
    width: 1280,
    height: 800,
    minWidth: 900,
    minHeight: 550,
    show: false,
    titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'default',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });

  mainWindow.loadFile(path.join(__dirname, '../renderer/index.html'));
  mainWindow.once('ready-to-show', () => mainWindow?.show());

  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    shell.openExternal(url);
    return { action: 'deny' };
  });

  mainWindow.on('closed', () => { mainWindow = null; });

  if (isDev) mainWindow.webContents.openDevTools({ mode: 'bottom' });

  return mainWindow;
}

// ── Native menu ───────────────────────────────────────────────────────────────

function buildMenu(): void {
  const send = (channel: string) => () => mainWindow?.webContents.send(channel);
  const isMac = process.platform === 'darwin';

  const template: MenuItemConstructorOptions[] = [
    ...(isMac ? [{ role: 'appMenu' as const }] : []),
    {
      label: 'File',
      submenu: [
        { label: 'New Dataset', accelerator: 'CmdOrCtrl+N', click: send('menu:file:new') },
        { type: 'separator' },
        { label: 'Open…', accelerator: 'CmdOrCtrl+O', click: send('menu:file:open') },
        { type: 'separator' },
        { label: 'Save', accelerator: 'CmdOrCtrl+S', click: send('menu:file:save') },
        { label: 'Save As…', accelerator: 'CmdOrCtrl+Shift+S', click: send('menu:file:saveAs') },
        { type: 'separator' },
        isMac ? { role: 'close' as const } : { role: 'quit' as const },
      ],
    },
    {
      label: 'Edit',
      submenu: [
        { role: 'undo' as const },
        { role: 'redo' as const },
        { type: 'separator' },
        { role: 'cut' as const },
        { role: 'copy' as const },
        { role: 'paste' as const },
        { role: 'selectAll' as const },
      ],
    },
    {
      label: 'Data',
      submenu: [
        { label: 'Sort Cases…', enabled: false },
        { label: 'Select Cases…', enabled: false },
        { type: 'separator' },
        { label: 'Merge Files', enabled: false },
        { label: 'Aggregate…', enabled: false },
      ],
    },
    {
      label: 'Analyze',
      submenu: [
        {
          label: 'Descriptive Statistics',
          submenu: [
            { label: 'Frequencies…', enabled: false },
            { label: 'Descriptives…', enabled: false },
            { label: 'Explore…', enabled: false },
            { label: 'Crosstabs…', enabled: false },
          ],
        },
        {
          label: 'Compare Means',
          submenu: [
            { label: 'One-Sample T Test…', enabled: false },
            { label: 'Independent-Samples T Test…', enabled: false },
            { label: 'Paired-Samples T Test…', enabled: false },
            { label: 'One-Way ANOVA…', enabled: false },
          ],
        },
        {
          label: 'Correlate',
          submenu: [
            { label: 'Bivariate…', enabled: false },
            { label: 'Partial…', enabled: false },
          ],
        },
        {
          label: 'Regression',
          submenu: [
            { label: 'Linear…', enabled: false },
            { label: 'Binary Logistic…', enabled: false },
          ],
        },
        {
          label: 'Nonparametric Tests',
          submenu: [
            { label: 'Chi-Square…', enabled: false },
            { label: 'Mann-Whitney U…', enabled: false },
            { label: 'Kruskal-Wallis…', enabled: false },
            { label: 'Wilcoxon…', enabled: false },
          ],
        },
        { type: 'separator' },
        { label: 'Factor Analysis…', enabled: false },
        { label: 'Cluster Analysis…', enabled: false },
        { label: 'Reliability Analysis…', enabled: false },
      ],
    },
    {
      label: 'Graphs',
      submenu: [
        { label: 'Histogram…', enabled: false },
        { label: 'Bar Chart…', enabled: false },
        { label: 'Scatter Plot…', enabled: false },
        { label: 'Box Plot…', enabled: false },
      ],
    },
    {
      label: 'View',
      submenu: [
        { label: 'File Explorer', accelerator: 'CmdOrCtrl+Shift+E', click: send('menu:view:explorer') },
        { type: 'separator' },
        { label: 'Data View', accelerator: 'CmdOrCtrl+D', click: send('menu:view:dataView') },
        { label: 'Variable View', accelerator: 'CmdOrCtrl+Shift+D', click: send('menu:view:variableView') },
        { type: 'separator' },
        { role: 'reload' as const },
        { role: 'toggleDevTools' as const },
        { type: 'separator' },
        { role: 'resetZoom' as const },
        { role: 'zoomIn' as const },
        { role: 'zoomOut' as const },
      ],
    },
    {
      role: 'help',
      submenu: [
        { label: 'About FreeStati', click: send('menu:help:about') },
      ],
    },
  ];

  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

// ── App lifecycle ─────────────────────────────────────────────────────────────

app.whenReady().then(() => {
  pythonBridge.start();
  buildMenu();
  createWindow();
  registerFileHandlers(() => mainWindow);

  ipcMain.handle('app:getVersion', () => app.getVersion());
  ipcMain.handle('app:getPlatform', () => process.platform);
  ipcMain.handle('app:openExternal', (_e, url: string) => shell.openExternal(url));
  ipcMain.handle('updater:installUpdate', () => autoUpdater.quitAndInstall());

  if (!isDev) {
    autoUpdater.checkForUpdatesAndNotify();
    autoUpdater.on('update-available', () =>
      mainWindow?.webContents.send('updater:updateAvailable'),
    );
    autoUpdater.on('update-downloaded', () =>
      mainWindow?.webContents.send('updater:updateDownloaded'),
    );
  }

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    pythonBridge.stop();
    app.quit();
  }
});

app.on('before-quit', () => pythonBridge.stop());

app.on('web-contents-created', (_e, contents) => {
  contents.on('will-navigate', (event) => event.preventDefault());
});
