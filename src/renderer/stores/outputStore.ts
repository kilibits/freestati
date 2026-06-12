import type { Analysis, ChartData, OutputItem } from '../types/analysis';

type Listener = () => void;

/** Accumulates analysis results and charts for the Output viewer. */
class OutputStore {
  private items: OutputItem[] = [];
  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private notify(): void {
    this.listeners.forEach((fn) => fn());
  }

  get(): readonly OutputItem[] {
    return this.items;
  }

  appendAnalysis(analysis: Analysis): void {
    this.items = [...this.items, { kind: 'analysis', analysis }];
    this.notify();
  }

  appendChart(chart: ChartData): void {
    this.items = [...this.items, { kind: 'chart', chart }];
    this.notify();
  }

  clear(): void {
    this.items = [];
    this.notify();
  }
}

export const outputStore = new OutputStore();
