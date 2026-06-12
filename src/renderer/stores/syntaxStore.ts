type Listener = () => void;

/**
 * Records the analyses/charts you run as replayable syntax lines, so a session
 * can be saved and re-run for reproducibility. Each line is either
 * `RUN <procedure> <jsonParams>` or `CHART <kind> <jsonParams>`.
 */
class SyntaxStore {
  private lines: string[] = [];
  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private notify(): void {
    this.listeners.forEach((fn) => fn());
  }

  getText(): string {
    return this.lines.join('\n');
  }

  append(line: string): void {
    this.lines.push(line);
    this.notify();
  }

  setText(text: string): void {
    this.lines = text.split('\n');
    this.notify();
  }

  clear(): void {
    this.lines = [];
    this.notify();
  }
}

export const syntaxStore = new SyntaxStore();
