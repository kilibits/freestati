import { dataStore } from '../stores/dataStore';

export class StatusBar {
  private el!: HTMLElement;
  private unsub: (() => void) | null = null;

  mount(container: HTMLElement): void {
    this.el = document.createElement('div');
    this.el.className = 'status-bar';
    container.appendChild(this.el);
    this.render();
    this.unsub = dataStore.subscribe(() => this.render());
  }

  private render(): void {
    const { loaded, filename, rowCount, colCount, modified } = dataStore.get();
    if (!loaded) {
      this.el.textContent = 'No data loaded';
      return;
    }
    const parts = [
      filename,
      `N = ${rowCount.toLocaleString()}`,
      `${colCount} variable${colCount !== 1 ? 's' : ''}`,
      modified ? '● Modified' : '',
    ].filter(Boolean);
    this.el.textContent = parts.join('   ');
  }

  destroy(): void {
    this.unsub?.();
  }
}
