import { ChildProcess, spawn } from 'child_process';
import { createInterface } from 'readline';
import { randomUUID } from 'crypto';
import path from 'path';
import { app } from 'electron';

type Pending = { resolve: (v: unknown) => void; reject: (e: Error) => void };

export class PythonBridge {
  private proc: ChildProcess | null = null;
  private pending = new Map<string, Pending>();
  private readyCallbacks: (() => void)[] = [];
  private isReady = false;
  private startError: string | null = null;

  start(): void {
    const scriptPath = app.isPackaged
      ? path.join(process.resourcesPath, 'python', 'engine.py')
      : path.join(app.getAppPath(), 'src', 'main', 'python', 'engine.py');

    const bin = process.platform === 'win32' ? 'python' : 'python3';

    try {
      this.proc = spawn(bin, [scriptPath], { stdio: ['pipe', 'pipe', 'pipe'] });
    } catch (err) {
      this.startError = `Failed to spawn ${bin}: ${err}`;
      console.error('[Python]', this.startError);
      return;
    }

    const rl = createInterface({ input: this.proc.stdout! });

    rl.on('line', (raw) => {
      let msg: Record<string, unknown>;
      try { msg = JSON.parse(raw); } catch { return; }

      if (msg['type'] === 'ready') {
        this.isReady = true;
        this.readyCallbacks.forEach(cb => cb());
        this.readyCallbacks = [];
        return;
      }

      const id = msg['id'] as string;
      const p = this.pending.get(id);
      if (!p) return;
      this.pending.delete(id);

      if (msg['error']) {
        p.reject(new Error(msg['error'] as string));
      } else {
        p.resolve(msg['result']);
      }
    });

    this.proc.stderr?.on('data', (chunk: Buffer) =>
      console.error('[Python stderr]', chunk.toString().trim()),
    );

    this.proc.on('exit', (code) => {
      console.warn(`[Python] process exited (code=${code})`);
      this.isReady = false;
      this.proc = null;
      // Reject any still-pending requests
      for (const p of this.pending.values()) {
        p.reject(new Error('Python process exited unexpectedly'));
      }
      this.pending.clear();
    });
  }

  private waitReady(): Promise<void> {
    if (this.startError) return Promise.reject(new Error(this.startError));
    if (this.isReady) return Promise.resolve();
    return new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error('Python startup timed out')), 15_000);
      this.readyCallbacks.push(() => { clearTimeout(timeout); resolve(); });
    });
  }

  async execute<T = unknown>(type: string, args: Record<string, unknown> = {}): Promise<T> {
    await this.waitReady();
    if (!this.proc?.stdin) throw new Error('Python process not running');
    return new Promise<T>((resolve, reject) => {
      const id = randomUUID();
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.proc!.stdin!.write(JSON.stringify({ id, type, args }) + '\n');
    });
  }

  stop(): void {
    this.proc?.kill();
    this.proc = null;
  }
}

export const pythonBridge = new PythonBridge();
