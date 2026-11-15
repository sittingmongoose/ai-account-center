/** Starts or attaches to one private Rust boot. No PID from disk is ever signalled. */
import fs from 'fs';
import net from 'net';
import path from 'path';
import { createHash, randomBytes } from 'crypto';
import { spawn, type ChildProcess } from 'child_process';
import type http from 'http';
import { ConfigError } from '../errors/error-types';
import { requestRust, rustAgent, type RustConnection } from './rust-service-transport';

/** The private socket protocol this front door speaks (aac-http `identity::PROTOCOL`). */
export const RUST_SERVICE_PROTOCOL = 2;

/** What a running service is: the build check on attach (L1). */
export interface RustBuild {
  commit: string;
  sha256: string;
  protocol: number;
  storeSchema: number;
}

export interface RustBoot extends RustConnection {
  ccsDir: string;
  binary: string;
  build: RustBuild;
}

export interface RustLease {
  connection: RustBoot;
  /** True when this front door spawned the boot (its stdin pipe keeps it alive). */
  owned: boolean;
  /** The owned child's PID (for its RSS); never read from disk. */
  pid: number | null;
  close: () => void;
  /** Called once when an owned child exits without `close()` (a crash or kill). */
  onExit: (listener: (code: number | null, signal: NodeJS.Signals | null) => void) => void;
}

/** Why no lease: logged by the caller with the route-free `rust.service.*` events. */
export type RustAcquireFailure =
  'platform' | 'binary' | 'socket_path' | 'spawn' | 'unhealthy' | 'build_mismatch';

export const PUBLIC_HEALTH_PRINCIPAL = {
  kind: 'session',
  id: 'public-health',
  role: 'public',
} as const;

/** A Unix socket path must fit `sun_path` (108 bytes on Linux, 104 on macOS); keep a margin. */
export const SOCKET_PATH_LIMIT = 100;

function uid(): number {
  return process.getuid?.() ?? -1;
}

function isPrivateDirectory(directory: string): boolean {
  try {
    const stat = fs.lstatSync(directory);
    return stat.isDirectory() && (stat.mode & 0o777) === 0o700 && stat.uid === uid();
  } catch {
    return false;
  }
}

function privateDirectory(directory: string): void {
  try {
    fs.mkdirSync(directory, { mode: 0o700 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
  }
  if (!isPrivateDirectory(directory))
    throw new ConfigError('Rust service requires a private run directory');
}

/** The first 12 hex digits of SHA-256 of the canonical root (aac-http `short_name`). */
export function shortName(ccsDir: string): string {
  return createHash('sha256').update(ccsDir).digest('hex').slice(0, 12);
}

/** Folders to create, outermost first, and the socket inside the last one. */
export interface SocketLayout {
  folders: string[];
  socketPath: string;
}

/**
 * Where a boot's socket lives (L1, Fable L2): `<ccsDir>/run/aac-http-<id>/http.sock` while that
 * fits; otherwise `aac-http-<id>/http.sock` under a short private folder bound to the root by its
 * hash: `$AAC_RUST_RUN_DIR/aac-<hash12>` (Docker sets `/run/aac`), then
 * `$XDG_RUNTIME_DIR/aac-<hash12>`, then `/tmp/aac-<uid>/<hash12>`. Null when none fits.
 */
export function socketLayout(
  ccsDir: string,
  bootId: string,
  env: NodeJS.ProcessEnv = process.env
): SocketLayout | null {
  const boot = `aac-http-${bootId}`;
  const run = path.join(ccsDir, 'run');
  const fits = (socketPath: string) => Buffer.byteLength(socketPath) <= SOCKET_PATH_LIMIT;
  const classic = path.join(run, boot, 'http.sock');
  if (fits(classic)) return { folders: [path.join(run, boot)], socketPath: classic };
  const hash = shortName(ccsDir);
  const candidates: string[][] = [];
  for (const base of [env.AAC_RUST_RUN_DIR, env.XDG_RUNTIME_DIR])
    if (base && path.isAbsolute(base)) candidates.push([path.join(base, `aac-${hash}`)]);
  const owner = path.join('/tmp', `aac-${uid()}`);
  candidates.push([owner, path.join(owner, hash)]);
  for (const folders of candidates) {
    const parent = folders[folders.length - 1];
    const socketPath = path.join(parent, boot, 'http.sock');
    if (fits(socketPath)) return { folders: [...folders, path.join(parent, boot)], socketPath };
  }
  return null;
}

/** sha256 of the binary file, cached by its identity so a restart does not hash it again. */
const binaryHashes = new Map<string, { key: string; sha256: string }>();
export function binarySha256(binary: string): string {
  const stat = fs.statSync(binary);
  const key = `${stat.dev}:${stat.ino}:${stat.size}:${stat.mtimeMs}`;
  const cached = binaryHashes.get(binary);
  if (cached?.key === key) return cached.sha256;
  const sha256 = createHash('sha256').update(fs.readFileSync(binary)).digest('hex');
  binaryHashes.set(binary, { key, sha256 });
  return sha256;
}

export function parseBuild(text: string | null | undefined): RustBuild | null {
  if (typeof text !== 'string' || text.length > 512) return null;
  try {
    const value = JSON.parse(text) as Record<string, unknown>;
    if (
      !value ||
      typeof value !== 'object' ||
      Object.keys(value).sort().join() !== 'commit,protocol,sha256,storeSchema' ||
      typeof value.commit !== 'string' ||
      !/^(?:[a-f0-9]{7,40}|unknown)$/.test(value.commit) ||
      typeof value.sha256 !== 'string' ||
      !/^[a-f0-9]{64}$/.test(value.sha256) ||
      !Number.isSafeInteger(value.protocol) ||
      !Number.isSafeInteger(value.storeSchema)
    )
      return null;
    return value as unknown as RustBuild;
  } catch {
    return null;
  }
}

function sameBuild(a: RustBuild, b: RustBuild): boolean {
  return (
    a.commit === b.commit &&
    a.sha256 === b.sha256 &&
    a.protocol === b.protocol &&
    a.storeSchema === b.storeSchema
  );
}

/**
 * The build check: the running boot is the binary this front door would start, speaking its
 * protocol. A boot left by an in-place upgrade (another sha256) is never attached.
 */
export function buildMatches(build: RustBuild | null, binarySha: string): build is RustBuild {
  return !!build && build.sha256 === binarySha && build.protocol === RUST_SERVICE_PROTOCOL;
}

function readBoot(file: string, ccsDir: string, binary: string): RustBoot | null {
  try {
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.size > 4096 || (stat.mode & 0o777) !== 0o600 || stat.uid !== uid())
      return null;
    const boot = JSON.parse(fs.readFileSync(file, 'utf8')) as RustBoot;
    if (
      boot.ccsDir !== ccsDir ||
      boot.binary !== binary ||
      !/^[a-f0-9]{24}$/.test(boot.bootId) ||
      !/^[a-f0-9]{64}$/.test(boot.secret) ||
      !parseBuild(JSON.stringify(boot.build))
    )
      return null;
    const layout = socketLayout(ccsDir, boot.bootId);
    if (!layout || boot.socketPath !== layout.socketPath) return null;
    if (!layout.folders.every(isPrivateDirectory)) return null;
    const socket = fs.lstatSync(boot.socketPath);
    if (!socket.isSocket() || (socket.mode & 0o777) !== 0o600 || socket.uid !== uid()) return null;
    return {
      ccsDir,
      binary,
      socketPath: boot.socketPath,
      secret: boot.secret,
      bootId: boot.bootId,
      build: boot.build,
    };
  } catch {
    return null;
  }
}

/** The health reply's build identity, or null when the boot is not healthy. */
async function healthBuild(
  boot: RustConnection,
  active: Set<http.ClientRequest>
): Promise<RustBuild | null> {
  try {
    const result = await requestRust(boot, '/api/health', PUBLIC_HEALTH_PRINCIPAL, active);
    return result.status === 200 && JSON.stringify(result.body) === '{"status":"ok"}'
      ? parseBuild(result.build)
      : null;
  } catch {
    return null;
  }
}

/** True when nothing answers on `socketPath` (refused or missing). */
function refuses(socketPath: string): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = net.connect(socketPath);
    const timer = setTimeout(() => {
      socket.destroy();
      resolve(false);
    }, 500);
    socket.once('connect', () => {
      clearTimeout(timer);
      socket.destroy();
      resolve(false);
    });
    socket.once('error', (error: NodeJS.ErrnoException) => {
      clearTimeout(timer);
      resolve(error.code === 'ECONNREFUSED' || error.code === 'ENOENT');
    });
  });
}

/**
 * Remove boot folders left by killed owners: `aac-http-<id>` folders (in `<ccsDir>/run` and in
 * the short folder) older than a minute whose socket refuses a connection. Only private folders
 * of this account are touched; no process is signalled.
 */
export async function sweepStaleBoots(ccsDir: string, keep: string | null): Promise<number> {
  const parents = new Set<string>([path.join(ccsDir, 'run')]);
  const short = socketLayout(ccsDir, '0'.repeat(24));
  if (short) parents.add(path.dirname(path.dirname(short.socketPath)));
  let swept = 0;
  for (const parent of parents) {
    let names: string[];
    try {
      names = fs.readdirSync(parent);
    } catch {
      continue;
    }
    for (const name of names) {
      if (!/^aac-http-[a-f0-9]{24}$/.test(name) || name === keep) continue;
      const folder = path.join(parent, name);
      try {
        const stat = fs.lstatSync(folder);
        if (!isPrivateDirectory(folder) || Date.now() - stat.mtimeMs < 60_000) continue;
        if (!(await refuses(path.join(folder, 'http.sock')))) continue;
        fs.rmSync(folder, { recursive: true, force: true });
        swept++;
      } catch {
        /* Changed meanwhile; the next start looks again. */
      }
    }
  }
  return swept;
}

export interface AcquireResult {
  lease: RustLease | null;
  failure: RustAcquireFailure | null;
  /** True when a healthy boot was found but its build differed (L1 build check). */
  refusedAttach: boolean;
  swept: number;
}

/** The binary must already exist; this function never builds, downloads or installs it. */
export async function acquireRustService(
  privateRoot: string,
  binaryPath: string,
  active: Set<http.ClientRequest>
): Promise<RustLease | null> {
  return (await acquireRustServiceDetailed(privateRoot, binaryPath, active)).lease;
}

export async function acquireRustServiceDetailed(
  privateRoot: string,
  binaryPath: string,
  active: Set<http.ClientRequest>
): Promise<AcquireResult> {
  const result: AcquireResult = { lease: null, failure: null, refusedAttach: false, swept: 0 };
  if (process.platform === 'win32') return { ...result, failure: 'platform' };
  if (!fs.existsSync(binaryPath)) return { ...result, failure: 'binary' };
  const ccsDir = fs.realpathSync(privateRoot);
  const binary = fs.realpathSync(binaryPath);
  const sha256 = binarySha256(binary);
  const run = path.join(ccsDir, 'run');
  privateDirectory(run);
  const descriptor = path.join(run, 'aac-http.json');
  const existing = readBoot(descriptor, ccsDir, binary);
  if (existing) {
    const agent = rustAgent();
    const build = await healthBuild({ ...existing, agent }, active);
    if (build && sameBuild(build, existing.build) && buildMatches(build, sha256))
      return {
        ...result,
        lease: {
          connection: { ...existing, agent },
          owned: false,
          pid: null,
          close: () => agent.destroy(),
          onExit: () => {},
        },
      };
    agent.destroy();
    // A healthy boot of another build stays with its owner; this front door starts its own.
    if (build) result.refusedAttach = true;
  }
  result.swept = await sweepStaleBoots(ccsDir, existing ? `aac-http-${existing.bootId}` : null);
  const bootId = randomBytes(12).toString('hex');
  const layout = socketLayout(ccsDir, bootId);
  if (!layout) return { ...result, failure: 'socket_path' };
  for (const folder of layout.folders) privateDirectory(folder);
  const directory = layout.folders[layout.folders.length - 1];
  const agent = rustAgent();
  const boot: RustBoot = {
    ccsDir,
    binary,
    socketPath: layout.socketPath,
    secret: randomBytes(32).toString('hex'),
    bootId,
    // Filled from the service's own health reply once it answers.
    build: { commit: 'unknown', sha256, protocol: RUST_SERVICE_PROTOCOL, storeSchema: 0 },
    agent,
  };
  let child: ChildProcess | undefined;
  let closed = false;
  const exitListeners: ((code: number | null, signal: NodeJS.Signals | null) => void)[] = [];
  // Beside the descriptor: the socket folder may be on another filesystem (a tmpfs), where a
  // rename into run/ would fail.
  const temporary = path.join(run, `.aac-http-${bootId}.json`);
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
      fs.rmSync(temporary, { force: true });
    } catch {
      /* Best effort. */
    }
    // A short socket folder of this root goes too once no other boot of the root uses it.
    if (layout.folders.length > 1)
      try {
        fs.rmdirSync(layout.folders[layout.folders.length - 2]);
      } catch {
        /* Not empty, or already gone. */
      }
  };
  const close = () => {
    if (closed) return;
    closed = true;
    agent.destroy();
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
  let failure: RustAcquireFailure = 'unhealthy';
  try {
    child = spawn(binary, [], {
      stdio: ['pipe', 'ignore', 'ignore'],
      env: { PATH: process.env.PATH },
    });
    let failed = false;
    child.once('error', () => {
      failed = true;
      failure = 'spawn';
      removeOwnedFiles();
    });
    child.stdin?.on('error', () => {
      failed = true;
    });
    child.once('exit', (code, signal) => {
      failed = true;
      removeOwnedFiles();
      if (!closed) for (const listener of exitListeners) listener(code, signal);
    });
    child.stdin?.write(
      JSON.stringify({ ccsDir, socketPath: boot.socketPath, secret: boot.secret, bootId }) + '\n'
    );
    // The service hashes its own image before it binds; allow a slow disk about 5 s.
    for (let attempt = 0; attempt < 200 && !failed && !closed; attempt++) {
      const build = fs.existsSync(boot.socketPath) ? await healthBuild(boot, active) : null;
      if (build) {
        if (!buildMatches(build, sha256)) {
          // The file changed between hashing and spawning, or speaks another protocol.
          failure = 'build_mismatch';
          break;
        }
        const socket = fs.lstatSync(boot.socketPath);
        if (!socket.isSocket() || (socket.mode & 0o777) !== 0o600 || socket.uid !== uid()) break;
        boot.build = build;
        const { agent: _agent, ...stored } = boot;
        fs.writeFileSync(temporary, JSON.stringify(stored), { flag: 'wx', mode: 0o600 });
        fs.renameSync(temporary, descriptor);
        const owned = child;
        return {
          ...result,
          lease: {
            connection: boot,
            owned: true,
            pid: owned.pid ?? null,
            close,
            onExit: (listener) => exitListeners.push(listener),
          },
        };
      }
      await new Promise<void>((resolve) => setTimeout(resolve, 25));
    }
  } catch {
    /* Fixed fallback log belongs to the caller, never log boot input. */
  }
  close();
  return { ...result, failure };
}
