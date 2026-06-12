import type { Analysis } from '../types/analysis';

type Listener = () => void;

/** Accumulates analysis results for the Output viewer (newest appended last). */
class OutputStore {
  private items: Analysis[] = [];
  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private notify(): void {
    this.listeners.forEach((fn) => fn());
  }

  get(): readonly Analysis[] {
    return this.items;
  }

  append(result: Analysis): void {
    this.items = [...this.items, result];
    this.notify();
  }

  clear(): void {
    this.items = [];
    this.notify();
  }
}

export const outputStore = new OutputStore();
