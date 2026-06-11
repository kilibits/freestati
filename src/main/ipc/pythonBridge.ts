import { ChildProcess, spawn } from 'child_process';
import { randomUUID } from 'crypto';
import path from 'path';
import { app } from 'electron';
import { decode, encode } from '@msgpack/msgpack';

type Pending = { resolve: (v: unknown) => void; reject: (e: Error) => void };

/**
 * Reassembles length-prefixed msgpack frames from a raw byte stream.
 * Protocol: [uint32 BE length][msgpack payload] repeated.
 */
class FrameReader {
  private buf = Buffer.alloc(0);

  push(chunk: Buffer, onMessage: (msg: unknown) => void): void {
    this.buf = Buffer.from(this.buf.length ? Buffer.concat([this.buf, chunk]) : chunk);

    while (true) {
      if (this.buf.length < 4) break;
      const len = this.buf.readUInt32BE(0);
      if (this.buf.length < 4 + len) break;

      const payload = this.buf.subarray(4, 4 + len);
      // Avoid a full copy on every frame — slice the remainder
      this.buf = Buffer.from(this.buf.subarray(4 + len));

      try {
        onMessage(decode(payload));
      } catch (err) {
        console.error('[Bridge] frame decode error:', err);
      }
    }
  }
}

export class PythonBridge {
  private proc: ChildProcess | null = null;
  private pending = new Map<string, Pending>();
  private readyCallbacks: (() => void)[] = [];
  private isReady = false;
  private startError: string | null = null;
  private reader = new FrameReader();

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

    this.proc.stdout!.on('data', (chunk: Buffer) => {
      this.reader.push(chunk, (raw) => this.handleMessage(raw as Record<string, unknown>));
    });

    // engine.py already prefixes its stderr lines with "[Python]"; don't double it
    this.proc.stderr?.on('data', (chunk: Buffer) =>
      console.error(chunk.toString().trimEnd()),
    );

    this.proc.on('exit', (code) => {
      console.warn(`[Python] exited (code=${code})`);
      this.isReady = false;
      this.proc = null;
      const err = new Error('Python process exited unexpectedly');
      for (const p of this.pending.values()) p.reject(err);
      this.pending.clear();
    });
  }

  private handleMessage(msg: Record<string, unknown>): void {
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

      // Encode request as msgpack with 4-byte length prefix
      const payload = Buffer.from(encode({ id, type, args }));
      const header = Buffer.allocUnsafe(4);
      header.writeUInt32BE(payload.length, 0);
      this.proc!.stdin!.write(Buffer.concat([header, payload]));
    });
  }

  stop(): void {
    this.proc?.kill();
    this.proc = null;
  }
}

export const pythonBridge = new PythonBridge();
