import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { ConfigError } from '../errors/error-types';

const binaries = new Map<string, string>();
const handoffRoots = new Set<string>();
const writers = new Map<string, { binary: string; committed: () => void }>();

export function registerFileWriter(
  root: string,
  binary: string,
  committed: () => void
): () => void {
  const writer = { binary: path.resolve(binary), committed };
  handoffRoots.add(root);
  writers.set(root, writer);
  return () => {
    if (writers.get(root) === writer) writers.delete(root);
  };
}

/** The executable adopted by the current write service for this root. */
export function activeFileWriterBinary(root: string): string | undefined {
  return writers.get(canonicalLeaseRoot(root))?.binary;
}

export interface FileLease {
  close(): void;
  descriptor?: number;
}

export class RegisteredWriteUnclearError extends ConfigError {}

/** Return false only when no native writer was selected or the executable could not launch. */
export function commitWithFileLease(
  root: string,
  id: string,
  lease: FileLease,
  bytes: string | Buffer,
  filename?: string
): boolean {
  const writer = writers.get(root);
  if (!writer || lease.descriptor === undefined) return false;
  const args = ['--registered-write', id, root];
  if (filename) args.push(filename);
  const result = spawnSync(writer.binary, args, {
    input: bytes,
    stdio: ['pipe', 'ignore', lease.descriptor],
    timeout: 5000,
    windowsHide: true,
  });
  if (result.status === 0) {
    writer.committed();
    return true;
  }
  if (result.error && (result.error as NodeJS.ErrnoException).code === 'ENOENT') return false;
  if (result.status !== 74 && result.status !== 75)
    throw new RegisteredWriteUnclearError('The private file write could not be confirmed.');
  throw new ConfigError('The private file could not be saved safely.');
}
/** Sweep key metadata while the caller holds the registered key-folder lease. */
export function sweepKeysWithFileLease(
  root: string,
  lease: FileLease,
  input: { provider: string; nowMs: number; named: string[] }
): number | undefined {
  const writer = writers.get(root);
  if (!writer || lease.descriptor === undefined) return undefined;
  const result = spawnSync(writer.binary, ['--sweep-orphan-keys', root], {
    input: JSON.stringify(input),
    stdio: ['pipe', 'pipe', lease.descriptor],
    timeout: 5000,
    maxBuffer: 64,
    encoding: 'utf8',
    windowsHide: true,
  });
  if (result.error && (result.error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
  const count = Number(result.stdout?.trim());
  if (result.status !== 0 || !Number.isSafeInteger(count) || count < 0)
    throw new ConfigError('The orphan key sweep could not be confirmed.');
  if (count > 0) writer.committed();
  return count;
}

type ProviderWrite =
  | ['codex-metadata']
  | ['codex-move', home: string, previous: string, next: string]
  | ['codex-login', home: string, profile: string, scope: 'saved' | 'live']
  | ['antigravity-credential', profile: string, fingerprint: string]
  | ['antigravity-index', generation: string];

/** Delegate a prevalidated provider write while the caller still owns its original lock. */
export function writeProviderFile(
  root: string,
  marker: string,
  operation: ProviderWrite,
  input: string | Buffer
): 'unavailable' | 'committed' | 'partial' {
  const canonical = canonicalLeaseRoot(root);
  const writer = writers.get(canonical);
  if (!writer) return 'unavailable';
  const { O_RDONLY, O_DIRECTORY, O_NOFOLLOW } = fs.constants;
  const fd = fs.openSync(marker, O_RDONLY | O_DIRECTORY | O_NOFOLLOW);
  try {
    const [name, ...args] = operation;
    const result = spawnSync(writer.binary, ['--provider-write', name, canonical, ...args], {
      input,
      stdio: ['pipe', 'ignore', fd],
      timeout: 5000,
      windowsHide: true,
    });
    if (result.status === 0) {
      writer.committed();
      return 'committed';
    }
    if (result.status === 78 && name === 'codex-login') return 'partial';
    if (result.error && (result.error as NodeJS.ErrnoException).code === 'ENOENT')
      return 'unavailable';
    throw new ConfigError('The provider file could not be saved safely.');
  } finally {
    fs.closeSync(fd);
  }
}

/** Scoped profile tree operations keep the activation lease through child exit. */
export async function writeProviderDirectory(
  root: string,
  marker: string,
  action:
    | ['codex-preserve', home: string, staged: string, backup: string]
    | ['codex-discard', home: string, target: string]
): Promise<boolean> {
  const canonical = canonicalLeaseRoot(root);
  const writer = writers.get(canonical);
  if (!writer) return false;
  const { O_RDONLY, O_DIRECTORY, O_NOFOLLOW } = fs.constants;
  const descriptor = await fs.promises.open(marker, O_RDONLY | O_DIRECTORY | O_NOFOLLOW);
  try {
    const [operation, ...args] = action;
    const child = spawn(writer.binary, ['--provider-write', operation, canonical, ...args], {
      stdio: ['ignore', 'ignore', descriptor.fd],
      windowsHide: true,
    });
    const committed = await new Promise<boolean>((resolve, reject) => {
      const refused = () =>
        reject(new ConfigError('The provider directory change could not be confirmed.'));
      child.once('error', (error: NodeJS.ErrnoException) => {
        if (error.code === 'ENOENT') resolve(false);
        else refused();
      });
      child.once('exit', (code) => (code === 0 ? resolve(true) : refused()));
    });
    if (committed) writer.committed();
    return committed;
  } finally {
    await descriptor.close();
  }
}

export function signInStaging(
  root: string,
  provider: 'codex' | 'antigravity',
  operation:
    | ['prepare', staging: string]
    | ['prepare', staging: string, shared: string]
    | ['cleanup', staging: string]
    | ['sweep', now: string]
    | ['marker-claim', name: string, temporary: string]
    | ['marker-remove', name: string, dev: string, ino: string],
  input?: string
): number | undefined {
  const canonical = canonicalLeaseRoot(root);
  const writer = writers.get(canonical);
  if (!writer) return undefined;
  const [action, ...args] = operation;
  const result = spawnSync(
    writer.binary,
    ['--' + provider + '-staging', action, canonical, ...args],
    {
      input,
      stdio: [input === undefined ? 'ignore' : 'pipe', 'pipe', 'ignore'],
      timeout: 5000,
      maxBuffer: 64,
      encoding: 'utf8',
      windowsHide: true,
    }
  );
  if (result.error && (result.error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
  const count = Number(result.stdout?.trim());
  if (result.status !== 0 || !Number.isSafeInteger(count) || count < 0)
    throw new ConfigError('The sign-in staging write could not be confirmed.');
  writer.committed();
  return count;
}

/** A pure policy evaluation scoped to the same opted-in service as the writers. */
export function lifecyclePlan(root: string, request: object): unknown {
  const writer = writers.get(canonicalLeaseRoot(root));
  if (!writer) return undefined;
  const result = spawnSync(writer.binary, ['--lifecycle-plan'], {
    input: JSON.stringify(request),
    stdio: ['pipe', 'pipe', 'ignore'],
    timeout: 5000,
    maxBuffer: 256 * 1024,
    encoding: 'utf8',
    windowsHide: true,
  });
  if (result.error && (result.error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
  if (result.status !== 0 || typeof result.stdout !== 'string') return undefined;
  try {
    return JSON.parse(result.stdout) as unknown;
  } catch {
    return undefined;
  }
}

const defaultBinary = path.resolve(__dirname, '../native/aac-http');

export function rememberFileLeaseBinary(root: string, binary: string, writesOn = false): void {
  const realRoot = canonicalLeaseRoot(root);
  binaries.set(realRoot, path.resolve(binary));
  if (writesOn) handoffRoots.add(realRoot);
}

export function canonicalLeaseRoot(root: string): string {
  try {
    return fs.realpathSync(root);
  } catch {
    return path.resolve(root);
  }
}

export function hasFileWriterHandoff(root: string): boolean {
  return handoffRoots.has(canonicalLeaseRoot(root));
}

function executable(file: string): boolean {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

/**
 * Take the same flock as aac-files. The helper locks the descriptor inherited
 * on stdin. The kernel keeps the lock with the open file description, including
 * after the helper exits, until this process closes its descriptor.
 */
export function claimFileLease(root: string, file: string): FileLease;
export function claimFileLease(root: string, file: string, folder: true): FileLease | null;
export function claimFileLease(root: string, file: string, folder = false): FileLease | null {
  const binary =
    binaries.get(canonicalLeaseRoot(root)) ?? process.env.AAC_RUST_SERVICE_BINARY ?? defaultBinary;
  const helper = executable(binary) ? binary : undefined;
  const flock =
    process.platform === 'linux' ? ['/usr/bin/flock', '/bin/flock'].find(executable) : undefined;
  if (!helper && !flock) {
    if (folder) return null;
    // A fresh legacy installation can reserve the lock name by mkdir. Rust's
    // open_lock refuses a directory there. Keep an existing inode for flock
    // readers that might already hold it.
    fs.mkdirSync(file, { mode: 0o700 });
    return { close: () => fs.rmdirSync(file) };
  }

  const { O_RDONLY, O_RDWR, O_CREAT, O_DIRECTORY, O_NOFOLLOW, O_NONBLOCK } = fs.constants;
  const access = folder ? O_RDONLY | O_DIRECTORY : O_RDWR | O_CREAT;
  const fd = fs.openSync(file, access | O_NOFOLLOW | O_NONBLOCK, 0o600);
  let locked = false;
  try {
    const metadata = fs.fstatSync(fd);
    const current = fs.lstatSync(file);
    if (
      !(folder ? metadata.isDirectory() : metadata.isFile()) ||
      current.isSymbolicLink() ||
      metadata.ino !== current.ino ||
      metadata.dev !== current.dev ||
      (process.getuid && metadata.uid !== process.getuid()) ||
      (metadata.mode & 0o077) !== 0
    )
      throw new ConfigError('Private file lease could not be acquired.');
    const options = { stdio: [fd, 'ignore', 'ignore'] as const, timeout: 500, windowsHide: true };
    const open = helper
      ? spawnSync(helper, ['--file-lease'], { ...options, stdio: [...options.stdio] })
      : null;
    locked =
      open?.status === 0 ||
      (!!flock &&
        open?.status !== 73 &&
        spawnSync(flock, ['-n', '0'], { ...options, stdio: [...options.stdio] }).status === 0);
    if (!locked) throw new ConfigError('Private file lease could not be acquired.');
    const lease: FileLease = {
      descriptor: fd,
      close: () => {
        delete lease.descriptor;
        fs.closeSync(fd);
      },
    };
    return lease;
  } finally {
    if (!locked) fs.closeSync(fd);
  }
}
