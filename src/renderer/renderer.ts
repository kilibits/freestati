// Import for side effect: installs window.electron backed by Tauri.
// Must run before any component touches window.electron.
import './bridge';
import { App } from './components/App';

document.addEventListener('DOMContentLoaded', () => {
  new App().mount();
  initSidebarResizer();
});

function initSidebarResizer(): void {
  const resizer = document.getElementById('sidebar-resizer')!;
  const sidebar = document.getElementById('sidebar')!;
  let dragging = false;
  let startX = 0;
  let startW = 0;

  resizer.addEventListener('mousedown', (e) => {
    dragging = true;
    startX = e.clientX;
    startW = sidebar.offsetWidth;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
  });

  document.addEventListener('mousemove', (e) => {
    if (!dragging) return;
    const delta = e.clientX - startX;
    const newW = Math.max(160, Math.min(480, startW + delta));
    sidebar.style.width = `${newW}px`;
    localStorage.setItem('freestati:sidebarWidth', String(newW));
  });

  document.addEventListener('mouseup', () => {
    if (!dragging) return;
    dragging = false;
    document.body.style.cursor = '';
    document.body.style.userSelect = '';
  });

  // Restore saved width
  const saved = localStorage.getItem('freestati:sidebarWidth');
  if (saved) sidebar.style.width = `${saved}px`;
}
