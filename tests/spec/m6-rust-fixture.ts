/** Starts only the explicitly selected Rust public fixture on a random loopback port. */
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';

async function ready(child: ChildProcess): Promise<string> {
  assert(child.stdout && child.stderr);
  const output = createInterface({ input: child.stdout });
  try {
    return await new Promise((resolve, reject) => {
      const timeout = setTimeout(
        () => reject(new Error('Rust TCP listener did not start')),
        10_000
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

async function stop(child: ChildProcess): Promise<void> {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, 'exit');
  const deadline = setTimeout(() => child.kill('SIGKILL'), 5_000);
  try {
    child.kill();
    await exited;
  } finally {
    clearTimeout(deadline);
  }
}

export async function startRustFixture(
  binary: string,
  root: string,
  env: NodeJS.ProcessEnv,
  eventPipe = false
) {
  const child = spawn(binary, ['--public'], {
    env: {
      ...env,
      CCS_DIR: root,
      AAC_RUST_PUBLIC_LISTEN: '127.0.0.1:0',
      AAC_RUST_PUBLIC_EVENT_STDIN: eventPipe ? '1' : '0',
    },
    stdio: [eventPipe ? 'pipe' : 'ignore', 'pipe', 'pipe'],
    windowsHide: true,
  });
  const audit: string[] = [];
  child.stderr?.on('data', (chunk: Buffer) => {
    if (audit.length < 1024) audit.push(chunk.toString());
  });
  try {
    const url = await ready(child);
    return { url, audit, pipe: child.stdin, stop: () => stop(child) };
  } catch (error) {
    await stop(child);
    throw error;
  }
}

/** A fixture supplies its own CCS overrides. */
export function fixtureEnvironment(): NodeJS.ProcessEnv {
  return Object.fromEntries(
    Object.entries(process.env).filter(([name]) => !name.startsWith('CCS_'))
  );
}
