/** Starts or attaches to one private Rust boot. No PID from disk is ever signalled. */
import fs from 'fs';
import path from 'path';
import { randomBytes } from 'crypto';
import { spawn, type ChildProcess } from 'child_process';
import type http from 'http';
import { ConfigError } from '../errors/error-types';
import { requestRust, type RustConnection } from './rust-service-transport';

export interface RustBoot extends RustConnection {
  ccsDir: string;
  binary: string;
}

export interface RustLease {
  connection: RustBoot;
  close: () => void;
}

export const PUBLIC_HEALTH_PRINCIPAL = {
  kind: 'session',
  id: 'public-health',
  role: 'public',
} as const;

function privateDirectory(directory: string): void {
  try {
    fs.mkdirSync(directory, { mode: 0o700 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
  }
  const stat = fs.lstatSync(directory);
  if (!stat.isDirectory() || (stat.mode & 0o777) !== 0o700 || stat.uid !== process.getuid?.())
    throw new ConfigError('Rust service requires a private run directory');
}

function readBoot(file: string, ccsDir: string, binary: string): RustBoot | null {
  try {
    const stat = fs.lstatSync(file);
    if (
      !stat.isFile() ||
      stat.size > 4096 ||
      (stat.mode & 0o777) !== 0o600 ||
      stat.uid !== process.getuid?.()
    )
      return null;
    const boot = JSON.parse(fs.readFileSync(file, 'utf8')) as RustBoot;
    if (
      boot.ccsDir !== ccsDir ||
      boot.binary !== binary ||
      !/^[a-f0-9]{24}$/.test(boot.bootId) ||
      !/^[a-f0-9]{64}$/.test(boot.secret)
    )
      return null;
    const parent = path.join(ccsDir, 'run', `aac-http-${boot.bootId}`);
    if (boot.socketPath !== path.join(parent, 'http.sock')) return null;
    privateDirectory(parent);
    const socket = fs.lstatSync(boot.socketPath);
    if (!socket.isSocket() || (socket.mode & 0o777) !== 0o600 || socket.uid !== process.getuid?.())
      return null;
    return boot;
  } catch {
    return null;
  }
}

async function healthy(boot: RustBoot, active: Set<http.ClientRequest>): Promise<boolean> {
  try {
    const result = await requestRust(boot, '/api/health', PUBLIC_HEALTH_PRINCIPAL, active);
    return result.status === 200 && JSON.stringify(result.body) === '{"status":"ok"}';
  } catch {
    return false;
  }
}

/** The binary must already exist; this function never builds, downloads or installs it. */
export async function acquireRustService(
  privateRoot: string,
  binaryPath: string,
  active: Set<http.ClientRequest>
): Promise<RustLease | null> {
  if (process.platform === 'win32' || !fs.existsSync(binaryPath)) return null;
  const ccsDir = fs.realpathSync(privateRoot);
  const binary = fs.realpathSync(binaryPath);
  const run = path.join(ccsDir, 'run');
  privateDirectory(run);
  const descriptor = path.join(run, 'aac-http.json');
  const existing = readBoot(descriptor, ccsDir, binary);
  if (existing && (await healthy(existing, active)))
    return { connection: existing, close: () => {} };
  const bootId = randomBytes(12).toString('hex');
  const directory = path.join(run, `aac-http-${bootId}`);
  privateDirectory(directory);
  const boot: RustBoot = {
    ccsDir,
    binary,
    socketPath: path.join(directory, 'http.sock'),
    secret: randomBytes(32).toString('hex'),
    bootId,
  };
  let child: ChildProcess | undefined;
  let closed = false;
  const temporary = path.join(directory, 'descriptor.json');
  const removeOwnedFiles = () => {
    try {
      const stat = fs.lstatSync(descriptor);
      if (
        stat.isFile() &&
        stat.size <= 4096 &&
        JSON.parse(fs.readFileSync(descriptor, 'utf8')).bootId === bootId
      )
        fs.unlinkSync(descriptor);
    } catch {
      /* Descriptor missing, changed or unreadable. */
    }
    try {
      fs.rmSync(directory, { recursive: true, force: true });
    } catch {
      /* Best effort. */
    }
  };
  const close = () => {
    if (closed) return;
    closed = true;
    child?.stdin?.end();
    child?.kill('SIGTERM');
    if (!child?.pid || child.exitCode !== null || child.signalCode !== null) removeOwnedFiles();
    else {
      const timer = setTimeout(() => child?.kill('SIGKILL'), 1000);
      timer.unref();
      child.once('exit', () => {
        clearTimeout(timer);
        removeOwnedFiles();
      });
    }
  };
  try {
    child = spawn(binary, [], {
      stdio: ['pipe', 'ignore', 'ignore'],
      env: { PATH: process.env.PATH },
    });
    let failed = false;
    child.once('error', () => {
      failed = true;
      removeOwnedFiles();
    });
    child.stdin?.on('error', () => {
      failed = true;
    });
    child.once('exit', () => {
      failed = true;
      removeOwnedFiles();
    });
    child.stdin?.write(
      JSON.stringify({ ccsDir, socketPath: boot.socketPath, secret: boot.secret, bootId }) + '\n'
    );
    for (let attempt = 0; attempt < 40 && !failed && !closed; attempt++) {
      if (await healthy(boot, active)) {
        const socket = fs.lstatSync(boot.socketPath);
        if (
          !socket.isSocket() ||
          (socket.mode & 0o777) !== 0o600 ||
          socket.uid !== process.getuid?.()
        )
          break;
        fs.writeFileSync(temporary, JSON.stringify(boot), { flag: 'wx', mode: 0o600 });
        fs.renameSync(temporary, descriptor);
        return { connection: boot, close };
      }
      await new Promise<void>((resolve) => setTimeout(resolve, 25));
    }
  } catch {
    /* Fixed fallback log belongs to the caller, never log boot input. */
  }
  close();
  return null;
}
