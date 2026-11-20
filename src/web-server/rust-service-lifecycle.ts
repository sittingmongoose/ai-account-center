import { rustComposeSelected, rustGroupOn } from './rust-service-groups';
/** Starts or attaches to one private Rust boot. No PID from disk is ever signalled. */
import fs from 'fs';
import net from 'net';
import path from 'path';
import { createHash, randomBytes } from 'crypto';
import { spawn, type ChildProcess } from 'child_process';
import type http from 'http';
import { ConfigError } from '../errors/error-types';
import {
  requestRust,
  PUBLIC_HEALTH_PRINCIPAL,
  rustAgent,
  type RustConnection,
  type RustRenewalBoot,
  type RustStoreBoot,
  type RustStoreBootInput,
} from './rust-service-transport';
import { privateFileOwners, type PrivateFileRole } from '../shared/private-files';
import { rustServiceEnvironment } from './rust-service-env';
import { agentTransport } from './services/agent-verbs';

/** The private socket protocol this front door speaks (aac-http `identity::PROTOCOL`). */
export const RUST_SERVICE_PROTOCOL = 3;

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
  /** Each registered private file's writer (L2), as this front door resolved it at start. */
  files: Readonly<Record<string, PrivateFileRole>>;
  /** R3: the store host's config this boot was started with; null keeps the store in TS. */
  store: RustStoreBoot | null;
  /** R4: the owned readers' config this boot was started with; null keeps them in TS. */
  owned: RustOwnedBoot | null;
  /** G10: the renewal timer's homes this boot was started with; null keeps the timer in TS. */
  renewal: RustRenewalBoot | null;
  /** The same private service owns lifecycle and its collection generation. */
  nativeLifecycle?: true;
  /** The private collection producer also triggers the native usage publisher. */
  nativeUsageSnapshot?: true;
}

/**
 * The boot's store config for one spawn or attach: the supervisor's input plus this root's
 * store folder. Two boots attach only on the same config, like the file owners: the store's
 * one writer must never be shared by front doors that resolved it differently.
 */
export function expectedStoreBoot(
  ccsDir: string,
  input: RustStoreBootInput | null | undefined
): RustStoreBoot | null {
  return input ? { dir: path.join(ccsDir, 'store'), ...input } : null;
}

/** R4: the owned Status readers and Scoreboard config (the service's `OwnedBoot`). */
export interface RustOwnedBoot {
  reportsRoot: string;
  registryDir: string;
  status: boolean;
  scoreboard: boolean;
}

/** The same owned config in a stable spelling, for the attach check. */
function ownedKey(owned: unknown): string {
  return storeKey(owned);
}

/** The same store config in a stable spelling, for the attach check. */
function storeKey(store: unknown): string {
  if (!store || typeof store !== 'object' || Array.isArray(store)) return '';
  return JSON.stringify(
    Object.entries(store as Record<string, unknown>).sort(([a], [b]) =>
      a < b ? -1 : a > b ? 1 : 0
    )
  );
}

/** The same renewal config in a stable spelling, for the attach check. */
function renewalKey(renewal: unknown): string {
  if (!renewal || typeof renewal !== 'object' || Array.isArray(renewal)) return '';
  return JSON.stringify(
    Object.entries(renewal as Record<string, unknown>).sort(([a], [b]) =>
      a < b ? -1 : a > b ? 1 : 0
    )
  );
}

/** The same owners in a stable spelling, for the attach check. */
function ownersKey(files: unknown): string {
  if (!files || typeof files !== 'object' || Array.isArray(files)) return '';
  return JSON.stringify(Object.entries(files).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
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

export { PUBLIC_HEALTH_PRINCIPAL } from './rust-service-transport';

/** A Unix socket path must fit `sun_path` (108 bytes on Linux, 104 on macOS); keep a margin. */
export const SOCKET_PATH_LIMIT = 100;

function uid(): number {
  return process.getuid?.() ?? -1;
}

async function isPrivateDirectory(directory: string): Promise<boolean> {
  try {
    const stat = await fs.promises.lstat(directory);
    return stat.isDirectory() && (stat.mode & 0o777) === 0o700 && stat.uid === uid();
  } catch {
    return false;
  }
}

async function privateDirectory(directory: string): Promise<void> {
  try {
    await fs.promises.mkdir(directory, { mode: 0o700 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
  }
  const stat = await fs.promises.lstat(directory);
  if (!stat.isDirectory() || (stat.mode & 0o777) !== 0o700 || stat.uid !== uid())
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
export async function binarySha256(binary: string, signal?: AbortSignal): Promise<string> {
  // O_NONBLOCK prevents a pathname swap to a FIFO from blocking a libuv worker forever.
  const file = await fs.promises.open(binary, fs.constants.O_RDONLY | fs.constants.O_NONBLOCK);
  try {
    const stat = await file.stat();
    if (!stat.isFile()) throw new ConfigError('Rust service image must be a regular file');
    signal?.throwIfAborted();
    const key = `${stat.dev}:${stat.ino}:${stat.size}:${stat.mtimeMs}`;
    const cached = binaryHashes.get(binary);
    if (cached?.key === key) return cached.sha256;
    const hash = createHash('sha256');
    const buffer = Buffer.alloc(64 * 1024);
    for (;;) {
      signal?.throwIfAborted();
      const { bytesRead } = await file.read(buffer, 0, buffer.length, null);
      if (!bytesRead) break;
      hash.update(buffer.subarray(0, bytesRead));
    }
    signal?.throwIfAborted();
    const sha256 = hash.digest('hex');
    binaryHashes.set(binary, { key, sha256 });
    return sha256;
  } finally {
    await file.close();
  }
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

/** Validate the opened descriptor, not a pathname that could change before the read. */
async function privateDescriptor(file: string): Promise<string | null> {
  const handle = await fs.promises.open(
    file,
    fs.constants.O_RDONLY | fs.constants.O_NONBLOCK | fs.constants.O_NOFOLLOW
  );
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > 4096 || (stat.mode & 0o777) !== 0o600 || stat.uid !== uid())
      return null;
    const buffer = Buffer.alloc(4097);
    let size = 0;
    while (size < buffer.length) {
      const { bytesRead } = await handle.read(buffer, size, buffer.length - size, null);
      if (!bytesRead) break;
      size += bytesRead;
    }
    return size <= 4096 ? buffer.subarray(0, size).toString('utf8') : null;
  } finally {
    await handle.close();
  }
}

// Async cleanup and publication must not interleave a boot-id check with another
// boot's rename in this process. Admission below also bounds these mutation waiters.
const descriptorMutations = new Map<string, Promise<void>>();
function mutateDescriptor(file: string, change: () => Promise<void>): Promise<void> {
  const pending = (descriptorMutations.get(file) ?? Promise.resolve())
    .catch(() => undefined)
    .then(change);
  descriptorMutations.set(file, pending);
  void pending
    .finally(() => {
      if (descriptorMutations.get(file) === pending) descriptorMutations.delete(file);
    })
    .catch(() => undefined);
  return pending;
}

// Keep stalled libuv operations bounded across retries, even after the caller's budget expires.
let acquiring = 0;
let cleaning = 0;
const FILESYSTEM_LIMIT = 2;
function cleanupFiles(cleanup: () => Promise<void>): void {
  // A later stale-boot sweep reclaims files if cleanup is already saturated. Never queue
  // unbounded filesystem work on the HTTP process or hold shutdown waiting for a disk.
  if (cleaning >= FILESYSTEM_LIMIT) return;
  cleaning++;
  void cleanup()
    .catch(() => undefined)
    .finally(() => cleaning--);
}

async function readBoot(
  file: string,
  ccsDir: string,
  binary: string,
  store: RustStoreBoot | null = null,
  owned: RustOwnedBoot | null = null,
  renewal: RustRenewalBoot | null = null
): Promise<RustBoot | null> {
  try {
    const text = await privateDescriptor(file);
    if (text === null) return null;
    const boot = JSON.parse(text) as RustBoot;
    if (
      boot.ccsDir !== ccsDir ||
      boot.binary !== binary ||
      !/^[a-f0-9]{24}$/.test(boot.bootId) ||
      !/^[a-f0-9]{64}$/.test(boot.secret) ||
      !parseBuild(JSON.stringify(boot.build)) ||
      // A boot started with other file owners is never attached (L2): one writer per file.
      ownersKey(boot.files) !== ownersKey(privateFileOwners()) ||
      // R3: likewise a boot hosting the store for another config (or none) is never attached.
      storeKey(boot.store) !== storeKey(store) ||
      // R4: likewise a boot running the owned readers for another config (or none).
      ownedKey(boot.owned) !== ownedKey(owned) ||
      // G10: likewise a boot running the renewal timer for another config (or none).
      renewalKey(boot.renewal) !== renewalKey(renewal)
    )
      return null;
    const layout = socketLayout(ccsDir, boot.bootId);
    if (!layout || boot.socketPath !== layout.socketPath) return null;
    if (!(await Promise.all(layout.folders.map(isPrivateDirectory))).every(Boolean)) return null;
    const socket = await fs.promises.lstat(boot.socketPath);
    if (!socket.isSocket() || (socket.mode & 0o777) !== 0o600 || socket.uid !== uid()) return null;
    return {
      ccsDir,
      binary,
      socketPath: boot.socketPath,
      secret: boot.secret,
      bootId: boot.bootId,
      build: boot.build,
      files: boot.files,
      store: boot.store ?? null,
      owned: boot.owned ?? null,
      renewal: boot.renewal ?? null,
      ...(boot.nativeLifecycle === true ? { nativeLifecycle: true } : {}),
      ...(boot.nativeUsageSnapshot === true ? { nativeUsageSnapshot: true } : {}),
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
export async function sweepStaleBoots(
  ccsDir: string,
  keep: string | null,
  signal?: AbortSignal
): Promise<number> {
  const parents = new Set<string>([path.join(ccsDir, 'run')]);
  const short = socketLayout(ccsDir, '0'.repeat(24));
  if (short) parents.add(path.dirname(path.dirname(short.socketPath)));
  let swept = 0;
  for (const parent of parents) {
    let names: string[];
    try {
      names = await fs.promises.readdir(parent);
    } catch {
      continue;
    }
    for (const name of names) {
      signal?.throwIfAborted();
      if (!/^aac-http-[a-f0-9]{24}$/.test(name) || name === keep) continue;
      const folder = path.join(parent, name);
      try {
        const stat = await fs.promises.lstat(folder);
        if (!(await isPrivateDirectory(folder)) || Date.now() - stat.mtimeMs < 60_000) continue;
        if (!(await refuses(path.join(folder, 'http.sock')))) continue;
        signal?.throwIfAborted();
        await fs.promises.rm(folder, { recursive: true, force: true });
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
  active: Set<http.ClientRequest>,
  store?: RustStoreBootInput | null,
  renewal?: RustRenewalBoot | null
): Promise<RustLease | null> {
  return (
    await acquireRustServiceDetailed(
      privateRoot,
      binaryPath,
      active,
      undefined,
      store,
      undefined,
      renewal
    )
  ).lease;
}

export async function acquireRustServiceDetailed(
  privateRoot: string,
  binaryPath: string,
  active: Set<http.ClientRequest>,
  signal?: AbortSignal,
  storeInput?: RustStoreBootInput | null,
  ownedInput?: RustOwnedBoot | null,
  renewalInput?: RustRenewalBoot | null
): Promise<AcquireResult> {
  if (acquiring >= FILESYSTEM_LIMIT)
    return { lease: null, failure: 'unhealthy', refusedAttach: false, swept: 0 };
  acquiring++;
  const budget = AbortSignal.timeout(5000);
  const stopped = signal ? AbortSignal.any([signal, budget]) : budget;
  const pending = acquireWithinBudget(
    privateRoot,
    binaryPath,
    active,
    stopped,
    storeInput,
    ownedInput,
    renewalInput
  ).finally(() => acquiring--);
  // Async filesystem work may finish late; every continuation checks the signal before adoption.
  return new Promise((resolve) => {
    const aborted = () =>
      resolve({ lease: null, failure: 'unhealthy', refusedAttach: false, swept: 0 });
    stopped.addEventListener('abort', aborted, { once: true });
    if (stopped.aborted) aborted();
    void pending
      .then(resolve, () =>
        resolve({ lease: null, failure: 'binary', refusedAttach: false, swept: 0 })
      )
      .finally(() => stopped.removeEventListener('abort', aborted));
  });
}

async function acquireWithinBudget(
  privateRoot: string,
  binaryPath: string,
  active: Set<http.ClientRequest>,
  signal: AbortSignal,
  storeInput?: RustStoreBootInput | null,
  ownedInput?: RustOwnedBoot | null,
  renewalInput?: RustRenewalBoot | null
): Promise<AcquireResult> {
  const result: AcquireResult = { lease: null, failure: null, refusedAttach: false, swept: 0 };
  if (process.platform === 'win32') return { ...result, failure: 'platform' };
  const ccsDir = await fs.promises.realpath(privateRoot);
  signal.throwIfAborted();
  const binary = await fs.promises.realpath(binaryPath);
  const sha256 = await binarySha256(binary, signal);
  signal.throwIfAborted();
  const run = path.join(ccsDir, 'run');
  await privateDirectory(run);
  signal.throwIfAborted();
  const descriptor = path.join(run, 'aac-http.json');
  const store = expectedStoreBoot(ccsDir, storeInput);
  const owned = ownedInput ?? null;
  const renewal = renewalInput ?? null;
  const existing = await readBoot(descriptor, ccsDir, binary, store, owned, renewal);
  signal.throwIfAborted();
  if (existing) {
    const agent = rustAgent();
    const build = await healthBuild({ ...existing, agent }, active);
    if (signal.aborted) {
      agent.destroy();
      signal.throwIfAborted();
    }
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
  result.swept = await sweepStaleBoots(
    ccsDir,
    existing ? `aac-http-${existing.bootId}` : null,
    signal
  );
  signal.throwIfAborted();
  const bootId = randomBytes(12).toString('hex');
  const layout = socketLayout(ccsDir, bootId);
  if (!layout) return { ...result, failure: 'socket_path' };
  const directory = layout.folders[layout.folders.length - 1];
  const agent = rustAgent();
  const files = privateFileOwners();
  const boot: RustBoot = {
    ccsDir,
    binary,
    socketPath: layout.socketPath,
    secret: randomBytes(32).toString('hex'),
    bootId,
    // Filled from the service's own health reply once it answers.
    build: { commit: 'unknown', sha256, protocol: RUST_SERVICE_PROTOCOL, storeSchema: 0 },
    files,
    store,
    owned,
    renewal,
    agent,
  };
  let child: ChildProcess | undefined;
  let closed = false;
  const exitListeners: ((code: number | null, signal: NodeJS.Signals | null) => void)[] = [];
  // Beside the descriptor: the socket folder may be on another filesystem (a tmpfs), where a
  // rename into run/ would fail.
  const temporary = path.join(run, `.aac-http-${bootId}.json`);
  const removeOwnedFiles = () =>
    cleanupFiles(async () => {
      try {
        await mutateDescriptor(descriptor, async () => {
          const text = await privateDescriptor(descriptor);
          if (text !== null && JSON.parse(text).bootId === bootId)
            await fs.promises.unlink(descriptor);
        });
      } catch {
        /* Descriptor missing, changed or unreadable. */
      }
      try {
        await fs.promises.rm(directory, { recursive: true, force: true });
        await fs.promises.rm(temporary, { force: true });
      } catch {
        /* Best effort; a later sweep reclaims leftovers. */
      }
      if (layout.folders.length > 1)
        try {
          await mutateDescriptor(descriptor, () =>
            fs.promises.rmdir(layout.folders[layout.folders.length - 2])
          );
        } catch {
          /* Not empty, or already gone. */
        }
    });
  const close = () => {
    if (closed) return;
    closed = true;
    signal.removeEventListener('abort', close);
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
  signal.addEventListener('abort', close, { once: true });
  try {
    signal.throwIfAborted();
    // Serialize this front door's creation and cleanup; Rust shutdown is independent.
    await mutateDescriptor(descriptor, async () => {
      signal.throwIfAborted();
      // A Rust shutdown can remove an empty shared parent after lstat. Retry at most
      // twice, recreating and revalidating every layout component before any spawn.
      for (let attempt = 0; attempt < 3; attempt++) {
        try {
          for (const folder of layout.folders) {
            await privateDirectory(folder);
            signal.throwIfAborted();
          }
          break;
        } catch (error) {
          if (attempt === 2 || (error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
          signal.throwIfAborted();
        }
      }
    });
    child = spawn(binary, [], {
      stdio: ['pipe', 'ignore', 'ignore'],
      env: {
        // Windows already returned above; the default re-detects the platform in its own scope.
        ...rustServiceEnvironment(process.env, undefined, agentTransport() === 'on'),
        AAC_RUST_NATIVE_APP_UPDATES:
          rustComposeSelected() && rustGroupOn('app-updates') ? '1' : '0',
        AAC_RUST_GROUP_CLAUDE_OPEN: rustGroupOn('claude-open') ? 'on' : 'off',
        AAC_AGENT_PACKAGE_ROOT: path.resolve(__dirname, '../..'),
        AAC_AGENT_CATALOG: path.resolve(__dirname, '../../config/agent-artifacts.json'),
        AAC_AGENT_TRANSPORT: process.env.AAC_AGENT_TRANSPORT,
        AAC_AGENT_IDENTITY_FILE: process.env.AAC_AGENT_IDENTITY_FILE,
        AAC_AGENT_KNOWN_HOSTS_FILE: process.env.AAC_AGENT_KNOWN_HOSTS_FILE,
      },
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
      JSON.stringify({
        ccsDir,
        socketPath: boot.socketPath,
        secret: boot.secret,
        bootId,
        files: boot.files,
        store: boot.store,
        owned: boot.owned,
        renewal: boot.renewal,
      }) + '\n'
    );
    // The service hashes its own image before it binds; allow a slow disk about 5 s.
    for (let attempt = 0; attempt < 200 && !failed && !closed; attempt++) {
      const build = await healthBuild(boot, active);
      if (closed || signal.aborted) break;
      if (build) {
        if (!buildMatches(build, sha256)) {
          // The file changed between hashing and spawning, or speaks another protocol.
          failure = 'build_mismatch';
          break;
        }
        const socket = await fs.promises.lstat(boot.socketPath);
        if (!socket.isSocket() || (socket.mode & 0o777) !== 0o600 || socket.uid !== uid()) break;
        signal.throwIfAborted();
        boot.build = build;
        const { agent: _agent, ...stored } = boot;
        await fs.promises.writeFile(temporary, JSON.stringify(stored), { flag: 'wx', mode: 0o600 });
        await mutateDescriptor(descriptor, async () => {
          signal.throwIfAborted();
          await fs.promises.rename(temporary, descriptor);
        });
        signal.throwIfAborted();
        signal.removeEventListener('abort', close);
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
  // A cancelled mkdir/write/rename may have completed after close's first cleanup.
  removeOwnedFiles();
  return { ...result, failure };
}
