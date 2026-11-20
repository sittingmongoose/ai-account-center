/** Starts only the explicitly selected Rust public fixture on a random loopback port. */
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:net';
import { createInterface } from 'node:readline';

async function ready(child: ChildProcess, timeoutMs: number): Promise<string> {
  assert(child.stdout && child.stderr);
  const output = createInterface({ input: child.stdout });
  try {
    return await new Promise((resolve, reject) => {
      const timeout = setTimeout(
        () => reject(new Error('Rust TCP listener did not start')),
        timeoutMs
      );
      child.once('error', reject);
      child.once('exit', (status) => reject(new Error(`Rust TCP listener exited with ${status}`)));
      output.on('line', (line) => {
        const address = /^aac-http public listening on (127\.0\.0\.1:\d+)$/.exec(line)?.[1];
        if (address) resolve('http://' + address);
      });
      output.once('close', () => clearTimeout(timeout));
    });
  } finally {
    output.close();
    child.stdout.resume();
  }
}

async function stop(child: ChildProcess, gracefulStdin = false): Promise<void> {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, 'exit');
  const deadline = setTimeout(() => child.kill('SIGKILL'), 5_000);
  try {
    if (gracefulStdin) child.stdin?.end();
    else child.kill();
    await exited;
  } finally {
    clearTimeout(deadline);
  }
}

export async function startRustFixture(
  binary: string,
  root: string,
  env: NodeJS.ProcessEnv,
  eventPipe = false,
  mode: 'fixture' | 'cut' | 'cut-rust-only' = 'fixture',
  gracefulStdin = false
) {
  const args = ['--public'];
  const cut = mode !== 'fixture';
  if (cut) {
    const reserve = createServer();
    const listening = once(reserve, 'listening');
    reserve.listen(0, '127.0.0.1');
    await listening;
    const address = reserve.address();
    assert(address && typeof address === 'object');
    await new Promise<void>((resolve, reject) =>
      reserve.close((error) => (error ? reject(error) : resolve()))
    );
    args.splice(
      0,
      1,
      'dashboard',
      '--no-open',
      '--host',
      '127.0.0.1',
      '--port',
      String(address.port)
    );
  }
  const child = spawn(binary, args, {
    env: {
      ...env,
      CCS_DIR: root,
      AAC_RUST_PUBLIC_LISTEN: '127.0.0.1:0',
      AAC_RUST_PUBLIC_EVENT_STDIN: eventPipe ? '1' : '0',
      ...(gracefulStdin ? { AAC_RUST_CUT_FIXTURE_STOP_STDIN: '1' } : {}),
      AAC_TS_SIDECAR: mode === 'cut-rust-only' ? 'off' : env.AAC_TS_SIDECAR,
    },
    stdio: [eventPipe || gracefulStdin ? 'pipe' : 'ignore', 'pipe', 'pipe'],
    windowsHide: true,
  });
  const audit: string[] = [];
  child.stderr?.on('data', (chunk: Buffer) => {
    if (audit.length < 1024) audit.push(chunk.toString());
  });
  try {
    const url = await ready(child, cut ? 22_000 : 10_000);
    return { url, audit, pipe: child.stdin, stop: () => stop(child, gracefulStdin) };
  } catch (error) {
    await stop(child);
    throw new Error(audit.join('').trim() || String(error), { cause: error });
  }
}

/** A fixture supplies its own CCS overrides. */
export function fixtureEnvironment(): NodeJS.ProcessEnv {
  return Object.fromEntries(
    Object.entries(process.env).filter(([name]) => !name.startsWith('CCS_'))
  );
}
