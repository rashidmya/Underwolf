import { spawn, type ChildProcessByStdio } from 'node:child_process';
import { isAbsolute } from 'node:path';
import type { Writable, Readable } from 'node:stream';

// Own only the explicitly selected launcher. No shell or process-name cleanup.
export class InputSession {
  private readonly child: ChildProcessByStdio<Writable, Readable, null>;
  private stopped = false;
  constructor(command: string, consume: (line: string) => void, failed: (reason: string) => void, env: NodeJS.ProcessEnv = process.env) {
    if (!isAbsolute(command)) throw Error('input command must be absolute');
    this.child = spawn(command, [], { shell: false, stdio: ['pipe', 'pipe', 'inherit'], env });
    let buffer = '';
    const fail = (reason: string): void => { if (!this.stopped) { this.stop(); failed(reason); } };
    this.child.stdout.setEncoding('utf8');
    this.child.stdout.on('data', (chunk: string) => {
      if (this.stopped) return;
      buffer += chunk;
      try {
        for (;;) {
          const newline = buffer.indexOf('\n'); if (newline < 0) break;
          const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1);
          if (Buffer.byteLength(line) > 32768) throw Error('input line exceeds limit');
          consume(line);
          if (this.stopped) return;
        }
        if (Buffer.byteLength(buffer) > 32768) throw Error('input line exceeds limit');
      } catch (error) { fail(String(error)); }
    });
    this.child.on('error', error => fail(String(error)));
    this.child.stdin.on('error', error => fail(String(error)));
    this.child.on('close', (code, signal) => fail(buffer ? 'truncated input line' : `input launcher closed (${code ?? signal})`));
  }
  send(command: unknown): void {
    if (this.stopped) throw Error('input session stopped');
    const line = JSON.stringify(command);
    if (Buffer.byteLength(line) > 32768) throw Error('input command exceeds limit');
    if (!this.child.stdin.write(line + '\n')) { this.stop(); throw Error('input launcher backpressure'); }
  }
  stop(): void {
    if (this.stopped) return;
    this.stopped = true; this.child.stdin.end(); this.child.kill('SIGTERM');
    const child = this.child;
    const timer = setTimeout(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); }, 500);
    timer.unref(); child.once('close', () => clearTimeout(timer));
  }
}
