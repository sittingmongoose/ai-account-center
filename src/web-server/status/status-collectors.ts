/**
 * The Status tab's other inputs, each read-only and each with its own failure word:
 * - the pm-mail registry (threads that do not report yet);
 * - `browser-check slots --json` (browsers, recordings, GPU on every machine);
 * - `simlock status --json` (phone capacity and leases; never tokens);
 * - a machine reading (CPU, memory, system disk): the local computer from /proc and statfs, the
 *   Mac, Windows and Nas1 with one short read-only command over the dashboard's existing ssh
 *   aliases (the app-update hosts).
 * Parsers are pure and exported for tests; nothing here writes anywhere.
 */
import { execFile, type ChildProcess } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { NetworkError, ValidationError } from '../../errors/error-types';
import { DeadlineExceededError, withProcessDeadline } from '../../utils/settle-deadline';
import { APP_UPDATE_SSH_HOSTS } from '../services/app-update-hosts';
import { isSafeUsageSshAlias } from '../services/additional-usage-transport';
import { PERSISTENT_SHARE_ERROR, TransientMisses } from './agent-reports-reader';
import { findSecret, MACHINES, type MachineId } from './report-schema';
import { readRegularFile } from './share-read';
import { realTimestamp } from './schema-helpers';

// ---------------------------------------------------------------------------------------------
// Commands

export interface CommandResult {
  code: number | null;
  stdout: string;
  timedOut: boolean;
  /** The program was not found. */
  missing: boolean;
}

export type CommandRunner = (
  file: string,
  args: string[],
  options: {
    timeoutMs: number;
    maxBytes: number;
    env?: NodeJS.ProcessEnv;
    input?: string;
    /** Time after the timeout's SIGTERM before SIGKILL and settling. */
    graceMs?: number;
  }
) => Promise<CommandResult>;

/**
 * execFile without a shell; never rejects, and always settles: a child that ignores the timeout's
 * SIGTERM is killed after the grace, so a caller's `collecting` or `ringing` flag always clears.
 */
export const runCommand: CommandRunner = (file, args, options) => {
  let child: ChildProcess | undefined;
  const work = new Promise<CommandResult>((resolve) => {
    try {
      child = execFile(
        file,
        args,
        {
          encoding: 'utf8',
          timeout: options.timeoutMs,
          maxBuffer: options.maxBytes,
          windowsHide: true,
          env: options.env ?? childEnv(),
        },
        (error, stdout) => {
          const err = error as
            | (NodeJS.ErrnoException & { killed?: boolean; code?: unknown })
            | null;
          resolve({
            code: err ? (typeof err.code === 'number' ? err.code : null) : 0,
            stdout: typeof stdout === 'string' ? stdout : '',
            timedOut: err?.killed === true,
            missing: err?.code === 'ENOENT',
          });
        }
      );
      child.stdin?.on('error', () => undefined);
      child.stdin?.end(options.input ?? '');
    } catch {
      resolve({ code: null, stdout: '', timedOut: false, missing: true });
    }
  });
  return withProcessDeadline(
    work,
    () => child,
    options.timeoutMs,
    () => new DeadlineExceededError('Command'),
    options.graceMs
  ).catch(() => ({ code: null, stdout: '', timedOut: true, missing: false }));
};

/** The dashboard's environment with Node's own folder on PATH (tool scripts start with `env node`). */
export function childEnv(extra: NodeJS.ProcessEnv = {}): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, ...extra };
  // A child must never bind itself to this server's Claude session or T3 thread.
  delete env.CLAUDE_CODE_SESSION_ID;
  delete env.T3CODE_THREAD_ID;
  const dirs = (env.PATH ?? '').split(path.delimiter).filter(Boolean);
  for (const dir of [path.dirname(process.execPath), path.join(os.homedir(), '.local', 'bin')])
    if (!dirs.includes(dir)) dirs.push(dir);
  env.PATH = dirs.join(path.delimiter);
  return env;
}

/** The first of PATH and the fallbacks that holds `name`, else the bare name. */
export function findExecutable(name: string, fallbacks: string[] = []): string {
  const dirs = (childEnv().PATH ?? '').split(path.delimiter).filter(Boolean);
  for (const candidate of [...dirs.map((dir) => path.join(dir, name)), ...fallbacks]) {
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      return candidate;
    } catch {
      /* next */
    }
  }
  return name;
}

/** A failed command in fixed words. */
export function commandFailure(tool: string, result: CommandResult): string {
  if (result.missing) return `${tool} is not installed here.`;
  if (result.timedOut) return `${tool} did not answer in time.`;
  return `${tool} failed.`;
}

function num(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function int(value: unknown): number | null {
  const result = num(value);
  return result === null ? null : Math.round(result);
}

function shortText(value: unknown, max: number): string | null {
  if (typeof value !== 'string') return null;
  const clean = value.replace(/[\u0000-\u001f\u007f-\u009f]/g, ' ').trim();
  if (!clean || findSecret(clean)) return null;
  return clean.length > max ? `${clean.slice(0, max - 1)}...` : clean;
}

// ---------------------------------------------------------------------------------------------
// pm-mail registry

export const DEFAULT_PM_MAIL_REGISTRY = '/mnt/Cursor/Inbox/registry';

export function pmMailRegistryDir(): string {
  return process.env.AAC_PM_MAIL_REGISTRY_DIR?.trim() || DEFAULT_PM_MAIL_REGISTRY;
}

export interface RegistryThread {
  name: string;
  kind: string | null;
  machine: MachineId | null;
  about: string | null;
  /** ISO time the thread last used pm-mail. */
  lastSeen: string | null;
  /** When AAC last read this row from its file (epoch ms); set by the reader. */
  readAt?: number;
  /** The file's current problem: while set, this row is the last good reading (from `readAt`). */
  problem?: ReadProblem | null;
}

/** One named part of a source (a file, a host, a worker) that failed while the rest was read. */
export interface ReadProblem {
  /** The file name, host label or worker label, as shown. */
  item: string;
  /** Fixed words, or the tool's own short text with secrets refused. */
  reason: string;
  since: number;
}

/** The next problem for an item, keeping `since` while the reason stays the same. */
export function nextProblem(
  before: ReadProblem | null | undefined,
  item: string,
  reason: string,
  now: number
): ReadProblem {
  return { item, reason, since: before?.reason === reason ? before.since : now };
}

/** A registry host name or working folder -> one of the four machines, or null. */
export function registryMachine(host: unknown, cwd: unknown): MachineId | null {
  const h = typeof host === 'string' ? host.toLowerCase() : '';
  if (h) {
    if (h.includes('nas1')) return 'nas1';
    if (h.includes('mac') || h.endsWith('.local')) return 'mac';
    if (h.includes('ubuntu') || h === 'vm') return 'vm';
    if (h.includes('win') || h.includes('pc') || h.includes('desktop')) return 'windows';
  }
  const dir = typeof cwd === 'string' ? cwd : '';
  if (dir.startsWith('/Users/') || dir.startsWith('/Volumes/')) return 'mac';
  if (/^[A-Za-z]:[\\/]/.test(dir) || dir.startsWith('\\\\')) return 'windows';
  if (dir.startsWith('/data/') || dir.includes('nas1')) return 'nas1';
  if (dir.startsWith('/home/') || dir.startsWith('/mnt/')) return 'vm';
  return null;
}

/** One registry file -> a thread row, or null when it is not one. */
export function parseRegistryEntry(raw: unknown): RegistryThread | null {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
  const rec = raw as Record<string, unknown>;
  if (typeof rec.name !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(rec.name))
    return null;
  // pm-mail writes `YYYY-MM-DDTHH:MM:SSZ`; a value that isn't a real calendar moment (Feb 30 would roll over) is unknown
  const lastSeen =
    typeof rec.last_seen === 'string' && realTimestamp(rec.last_seen)
      ? new Date(Date.parse(rec.last_seen)).toISOString()
      : null;
  return {
    name: rec.name,
    kind: shortText(rec.kind, 32),
    machine: registryMachine(rec.machine, rec.cwd),
    about: shortText(rec.about, 300),
    lastSeen,
  };
}

export interface RegistryScan {
  /** One row per file read well at least once; a row whose file now fails carries its `problem`. */
  threads: RegistryThread[];
  /** Every file that failed this scan, named (with or without an earlier good row). */
  problems: ReadProblem[];
  changed: boolean;
}

const REGISTRY_FILE_CAP = 64 * 1024;

/**
 * Rescans the registry folder: stats every file, reads only changed ones. Never writes. A file that
 * fails (unreadable, not JSON, too large, not a regular file) is a named problem: its last good row
 * is kept with its own read time and the problem, and the file is tried again next scan.
 */
export class PmMailRegistryReader {
  private readonly files = new Map<
    string,
    { key: string; thread: RegistryThread | null; readAt: number; problem: ReadProblem | null }
  >();
  /** Files with no good reading yet, by name. */
  private readonly failing = new Map<string, ReadProblem>();
  /** Listed files whose stat said ENOENT: the rename-over race once, a named problem twice in a row. */
  private readonly misses = new TransientMisses();

  constructor(private readonly dir: () => string = pmMailRegistryDir) {}

  /** Resolves the thread rows, or throws a fixed-word Error when the folder cannot be listed. */
  async scan(now: number = Date.now()): Promise<RegistryScan> {
    const dir = this.dir();
    let names: string[];
    try {
      names = await fs.promises.readdir(dir);
    } catch (error) {
      throw new NetworkError(
        (error as NodeJS.ErrnoException)?.code === 'ENOENT'
          ? 'The pm-mail registry folder is missing.'
          : 'The pm-mail registry folder could not be listed.'
      );
    }
    let changed = false;
    const seen = new Set<string>();
    const fail = (name: string, reason: string, since = now) => {
      const entry = this.files.get(name);
      if (entry) {
        const problem = nextProblem(entry.problem, name, reason, since);
        if (entry.problem?.reason !== reason) changed = true;
        entry.problem = problem;
        // Read again next scan, whatever the file's stat says then.
        entry.key = '';
        return;
      }
      const before = this.failing.get(name);
      if (before?.reason !== reason) changed = true;
      this.failing.set(name, nextProblem(before, name, reason, since));
    };
    for (const name of names) {
      if (name.startsWith('.') || !name.endsWith('.json')) continue;
      const file = path.join(dir, name);
      let stat: fs.Stats;
      try {
        stat = await fs.promises.stat(file);
      } catch (error) {
        seen.add(name);
        if ((error as NodeJS.ErrnoException)?.code === 'ENOENT') {
          // Gone between the listing and the stat: pm-mail's rename-over. Keep the last row for this
          // scan (the next listing says whether it was removed); listed and gone again is a problem.
          const since = this.misses.miss(name, now);
          if (since !== null) fail(name, PERSISTENT_SHARE_ERROR, since);
          continue;
        }
        this.misses.clear(name);
        fail(name, 'could not be read');
        continue;
      }
      this.misses.clear(name);
      seen.add(name);
      if (!stat.isFile()) {
        fail(name, 'is not a regular file');
        continue;
      }
      if (stat.size > REGISTRY_FILE_CAP) {
        fail(name, 'is larger than the 64 KB cap');
        continue;
      }
      const key = `${stat.size}:${stat.mtimeMs}:${stat.ino}:${stat.ctimeMs}`;
      const entry = this.files.get(name);
      if (entry?.key === key && entry.problem === null) continue;
      let raw: unknown;
      try {
        const bytes = await readRegularFile(file, REGISTRY_FILE_CAP);
        try {
          raw = JSON.parse(bytes.toString('utf8'));
        } catch {
          fail(name, 'is not valid JSON');
          continue;
        }
      } catch {
        fail(name, 'could not be read');
        continue;
      }
      // A file that is JSON but no thread entry is not a problem: pm-mail keeps other files here.
      const thread = parseRegistryEntry(raw);
      if (
        !entry ||
        entry.problem !== null ||
        JSON.stringify(entry.thread) !== JSON.stringify(thread)
      )
        changed = true;
      this.files.set(name, { key, thread, readAt: now, problem: null });
      this.failing.delete(name);
    }
    this.misses.keepOnly(seen);
    for (const name of [...this.files.keys()])
      if (!seen.has(name)) {
        this.files.delete(name);
        changed = true;
      }
    for (const name of [...this.failing.keys()])
      if (!seen.has(name)) {
        this.failing.delete(name);
        changed = true;
      }
    const threads = [...this.files.values()].flatMap((entry) =>
      entry.thread ? [{ ...entry.thread, readAt: entry.readAt, problem: entry.problem }] : []
    );
    const problems = [
      ...[...this.files.values()].flatMap((entry) => (entry.problem ? [entry.problem] : [])),
      ...this.failing.values(),
    ].sort((x, y) => x.item.localeCompare(y.item));
    return { threads, problems, changed };
  }
}

// ---------------------------------------------------------------------------------------------
// browser-check slots

export interface BrowserSession {
  id: string | null;
  /** Host and path only (no query string or fragment); a file URL shows its path. */
  url: string | null;
  owner: string | null;
  recording: boolean;
  ageSeconds: number | null;
}

export interface BrowserMachine {
  machine: MachineId;
  label: string | null;
  browsers: { running: number | null; max: number | null };
  recordings: { running: number | null; max: number | null };
  gpuPct: number | null;
  memGb: number | null;
  sessions: BrowserSession[];
  /**
   * The host's Simlock phones as browser-check reported them (running counts reserved ones). Null:
   * browser-check reported `phones: null` (unknown, never zero); undefined: it left the field out.
   */
  phones?: Partial<
    Record<'ios' | 'android', { running: number | null; max: number | null }>
  > | null;
  /**
   * What browser-check said is wrong with counts it did read: `display_problem` (no browser can
   * start there), `snapshot` (counted from session records while the backend was busy), and a
   * null phone report on a machine that runs a Simlock worker. Each is a named problem.
   */
  notes?: string[];
  /** browser-check could not read this host (`state.error`): the counts above are unknown. */
  error: string | null;
  /** Set by the service: when these counts were read (epoch ms), null before the first good reading. */
  readAt?: number | null;
  /** Set by the service: this host's current failure; while set the counts are the last good ones. */
  problem?: ReadProblem | null;
}

/** A session URL as host and path only. */
export function sessionUrl(value: unknown): string | null {
  if (typeof value !== 'string' || !value) return null;
  try {
    const url = new URL(value);
    if (url.protocol === 'file:') return shortText(decodeURIComponent(url.pathname), 200);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return null;
    return shortText(`${url.host}${url.pathname}`, 200);
  } catch {
    return null;
  }
}

/** `browser-check slots --json` -> one row per known machine. */
export function parseBrowserSlots(raw: unknown): BrowserMachine[] {
  const rows = (raw as { rows?: unknown } | null)?.rows;
  if (!Array.isArray(rows))
    throw new ValidationError('browser-check answered in an unknown shape.');
  const result: BrowserMachine[] = [];
  for (const row of rows) {
    const rec = (row ?? {}) as Record<string, unknown>;
    const machine = (rec.machine ?? {}) as Record<string, unknown>;
    const name = machine.name;
    if (typeof name !== 'string' || !(MACHINES as readonly string[]).includes(name)) continue;
    const state = (rec.state ?? {}) as Record<string, unknown>;
    // browser-check answers a host it could not read with `state: { error }` (and `free: null`), exit 0.
    if (state.error !== undefined && state.error !== null) {
      result.push({
        machine: name as MachineId,
        label: shortText(machine.label, 80),
        browsers: { running: null, max: null },
        recordings: { running: null, max: null },
        gpuPct: null,
        memGb: null,
        sessions: [],
        error: shortText(state.error, 160) ?? 'browser-check could not read this machine',
      });
      continue;
    }
    const caps = (state.caps ?? {}) as Record<string, unknown>;
    const sessions = Array.isArray(state.sessions) ? state.sessions : [];
    const phones = parseSlotPhones(state.phones);
    const notes: string[] = [];
    const display = shortText(state.display_problem, 200);
    if (display) notes.push(`browsers cannot start here: ${display}`);
    // `snapshot` is browser-check's own words for counts it took from session records while the
    // backend was busy. It carries no time: the counts are this reading's, so the read time stands.
    const snapshot = shortText(state.snapshot, 200);
    if (snapshot) notes.push(snapshot);
    if (phones === null && SIMLOCK_MACHINES.includes(name as MachineId))
      notes.push('reported no phone count (phones: null), so phones here are unknown');
    result.push({
      machine: name as MachineId,
      label: shortText(machine.label, 80),
      browsers: { running: int(state.browsers), max: int(caps.max_browsers) },
      recordings: { running: int(state.recordings), max: int(caps.max_recordings) },
      gpuPct: int(state.gpu_util),
      memGb: num(state.mem_gb) === null ? null : Math.round((state.mem_gb as number) * 10) / 10,
      sessions: sessions.slice(0, 50).map((session) => {
        const s = (session ?? {}) as Record<string, unknown>;
        return {
          id: typeof s.id === 'string' && /^[A-Za-z0-9_-]{1,64}$/.test(s.id) ? s.id : null,
          url: sessionUrl(s.url),
          owner: shortText(s.owner, 120),
          recording: s.recording === true,
          ageSeconds: int(s.age_s),
        };
      }),
      ...(phones === undefined ? {} : { phones }),
      notes,
      error: null,
    });
  }
  return result;
}

/** The machines that run a Simlock worker; elsewhere browser-check's `phones: null` is the normal answer. */
const SIMLOCK_MACHINES: readonly MachineId[] = ['vm', 'mac', 'nas1'];

/** browser-check's `phones`: null stays null and a missing count stays null, never zero. */
function parseSlotPhones(value: unknown): BrowserMachine['phones'] {
  if (value === undefined) return undefined;
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return null;
  const phones: NonNullable<BrowserMachine['phones']> = {};
  for (const kind of ['ios', 'android'] as const) {
    const row = (value as Record<string, unknown>)[kind];
    if (!row || typeof row !== 'object') continue;
    const r = row as Record<string, unknown>;
    phones[kind] = { running: int(r.running), max: int(r.max) };
  }
  return phones;
}

/**
 * One browser-check answer merged with the last: a host that answered keeps its new counts and read
 * time; a host that failed keeps its last good counts with their read time and the failure, or has
 * unknown counts and no read time when it never answered.
 */
export function mergeBrowserRows(
  before: BrowserMachine[] | null,
  rows: BrowserMachine[],
  now: number
): BrowserMachine[] {
  return rows.map((row) => {
    const last = before?.find((b) => b.machine === row.machine) ?? null;
    if (row.error === null) {
      // Counts read now, with what browser-check said is wrong with them named.
      const notes = row.notes ?? [];
      const problem =
        notes.length > 0
          ? nextProblem(last?.problem, row.label ?? row.machine, notes.join('; '), now)
          : null;
      return { ...row, readAt: now, problem };
    }
    const problem = nextProblem(last?.problem, row.label ?? row.machine, row.error, now);
    if (last && typeof last.readAt === 'number')
      return { ...last, label: row.label ?? last.label, error: row.error, problem };
    return { ...row, readAt: null, problem };
  });
}

// ---------------------------------------------------------------------------------------------
// simlock status

export interface SimlockCapacity {
  platform: string;
  running: number | null;
  max: number | null;
  reserved: number | null;
  overLimit: boolean;
}

export interface SimlockLease {
  device: string;
  deviceLabel: string | null;
  platform: string | null;
  holder: string | null;
  since: string | null;
  expiresAt: string | null;
  /** The lease is on a worker the gateway cannot reach: what it last knew, not a current lease. */
  lastKnown: boolean;
}

/** A gateway worker that is not connected: its capacity is left out, its devices and leases are its last known. */
export interface SimlockWorkerProblem {
  item: string;
  reason: string;
  lastSeenAt: string | null;
}

export interface SimlockStatus {
  capacity: SimlockCapacity[];
  leases: SimlockLease[];
  problems: SimlockWorkerProblem[];
}

function isoFromMs(value: unknown): string | null {
  const ms = num(value);
  return ms === null || ms <= 0 ? null : new Date(ms).toISOString();
}

/** `simlock status --json` -> capacity and leases. Lease ids, owner ids and tokens are not kept. */
export function parseSimlockStatus(raw: unknown): SimlockStatus {
  if (!raw || typeof raw !== 'object')
    throw new ValidationError('simlock answered in an unknown shape.');
  const rec = raw as Record<string, unknown>;
  // Simlock leaves out devices, leases and capacity while it starts: that is no reading, not an empty fleet.
  const health = ((rec.daemon ?? {}) as Record<string, unknown>).health;
  if (health === 'starting')
    throw new NetworkError('Simlock is starting and has not read its devices yet.');
  if (health === 'failed') throw new NetworkError('The Simlock daemon reports that it failed.');
  if (rec.daemon !== undefined && (rec.capacity === undefined || !Array.isArray(rec.leases)))
    throw new ValidationError('simlock answered without its capacity and leases.');
  const capacityRaw = (rec.capacity ?? {}) as Record<string, unknown>;
  if (typeof capacityRaw !== 'object')
    throw new ValidationError('simlock answered in an unknown shape.');
  const offline = new Set<string>();
  const problems: SimlockWorkerProblem[] = [];
  for (const worker of Array.isArray(rec.workers) ? rec.workers : []) {
    const w = (worker ?? {}) as Record<string, unknown>;
    if (typeof w.id !== 'string' || w.connection === 'connected') continue;
    offline.add(w.id);
    problems.push({
      item: shortText(w.label, 80) ?? shortText(w.id, 80) ?? 'A worker',
      reason:
        w.connection === 'incompatible'
          ? 'incompatible with the gateway; its devices and leases are the last known'
          : 'disconnected; its devices and leases are the last known',
      lastSeenAt: isoFromMs(w.lastSeenAt),
    });
  }
  const capacity: SimlockCapacity[] = [];
  for (const [platform, value] of Object.entries(capacityRaw)) {
    if (!/^[a-z0-9-]{1,20}$/.test(platform) || !value || typeof value !== 'object') continue;
    const row = value as Record<string, unknown>;
    capacity.push({
      platform,
      running: int(row.running),
      max: int(row.maxRunning ?? row.limit),
      reserved: int(row.reserved),
      overLimit: row.overLimit === true,
    });
  }
  const devices = new Map<string, { model: string | null; platform: string | null }>();
  for (const device of Array.isArray(rec.devices) ? rec.devices : []) {
    const d = (device ?? {}) as Record<string, unknown>;
    const spec = (d.spec ?? {}) as Record<string, unknown>;
    if (typeof d.id === 'string')
      devices.set(d.id, {
        model: shortText(spec.model, 60),
        platform: shortText(spec.platform, 20),
      });
  }
  const leases: SimlockLease[] = [];
  for (const lease of Array.isArray(rec.leases) ? rec.leases : []) {
    const l = (lease ?? {}) as Record<string, unknown>;
    const device = shortText(l.deviceId, 80);
    if (!device) continue;
    const worker = (l.worker ?? {}) as Record<string, unknown>;
    leases.push({
      device,
      deviceLabel: devices.get(device)?.model ?? null,
      platform: devices.get(device)?.platform ?? null,
      holder: shortText(worker.label, 80) ?? shortText(l.requesterId, 80),
      since: isoFromMs(l.grantedAt),
      expiresAt: isoFromMs(l.ttlDeadline),
      lastKnown: typeof l.workerId === 'string' && offline.has(l.workerId),
    });
  }
  return { capacity, leases, problems };
}

// ---------------------------------------------------------------------------------------------
// Machines

export interface MachineSample {
  cores: number | null;
  load1: number | null;
  /** CPU busy percent; null until two readings exist on a host that reports CPU times. */
  busyPct: number | null;
  memAvailableBytes: number | null;
  memTotalBytes: number | null;
  disk: { mount: string; freeBytes: number; totalBytes: number } | null;
  /**
   * Why the system disk could not be read this time, in fixed words; the rest of the reading
   * stands. Absent when the disk was read.
   */
  diskError?: string;
  /**
   * Set by the service: when `disk` was read (epoch ms). Older than the sample when the last disk
   * read failed and the service kept the earlier disk reading.
   */
  diskReadAt?: number | null;
  /** /proc/stat totals, for the busy percent between two readings. */
  cpuTimes: { busy: number; total: number } | null;
}

type HostOs = 'linux' | 'mac' | 'windows';

export const REMOTE_MACHINES: Readonly<Record<Exclude<MachineId, 'vm'>, HostOs>> = Object.freeze({
  mac: 'mac',
  windows: 'windows',
  nas1: 'linux',
});

const LINUX_PROBE = String.raw`export LC_ALL=C; printf 'cores=%s\n' "$(getconf _NPROCESSORS_ONLN)"; printf 'load=%s\n' "$(cut -d' ' -f1 /proc/loadavg)"; awk '/^MemTotal:/{print "memTotalKb="$2} /^MemAvailable:/{print "memAvailKb="$2}' /proc/meminfo; head -n1 /proc/stat | sed 's/^cpu */cpu=/'; df -Pk / | awk 'NR==2{print "diskTotalKb="$2; print "diskFreeKb="$4; print "mount="$6}'`;

const MAC_PROBE = String.raw`export LC_ALL=C; printf 'cores=%s\n' "$(sysctl -n hw.ncpu)"; printf 'load=%s\n' "$(sysctl -n vm.loadavg | awk '{print $2}')"; printf 'memTotalB=%s\n' "$(sysctl -n hw.memsize)"; vm_stat | awk '/page size of/{print "pageSize="$8} /^Pages (free|inactive|speculative|purgeable):/{v=$NF; sub(/\.$/,"",v); print "pagesAvail="v}'; printf 'cpuPct=%s\n' "$(ps -A -o %cpu= | awk '{s+=$1} END {print s+0}')"; d=/System/Volumes/Data; [ -d "$d" ] || d=/; df -Pk "$d" | awk 'NR==2{print "diskTotalKb="$2; print "diskFreeKb="$4; print "mount="$6}'`;

const WINDOWS_PROBE_SCRIPT = [
  "$ErrorActionPreference = 'Stop'",
  "$ProgressPreference = 'SilentlyContinue'",
  '$os = Get-CimInstance Win32_OperatingSystem',
  '$cpu = @(Get-CimInstance Win32_Processor)',
  '$drive = $env:SystemDrive',
  '$disk = Get-CimInstance Win32_LogicalDisk -Filter ("DeviceID=\'" + $drive + "\'")',
  '"cores=" + [int](($cpu | Measure-Object NumberOfLogicalProcessors -Sum).Sum)',
  '"cpuPct=" + [int](($cpu | Measure-Object LoadPercentage -Average).Average)',
  '"memTotalKb=" + [int64]$os.TotalVisibleMemorySize',
  '"memAvailKb=" + [int64]$os.FreePhysicalMemory',
  '"diskTotalB=" + [int64]$disk.Size',
  '"diskFreeB=" + [int64]$disk.FreeSpace',
  '"mount=" + $drive',
].join('; ');

/** The one read-only command a host runs per reading. */
export function machineProbeCommand(hostOs: HostOs): string {
  if (hostOs === 'linux') return LINUX_PROBE;
  if (hostOs === 'mac') return MAC_PROBE;
  return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(WINDOWS_PROBE_SCRIPT, 'utf16le').toString('base64')}`;
}

function linuxCpuTimes(line: string | undefined): MachineSample['cpuTimes'] {
  if (!line) return null;
  const values = line.trim().split(/\s+/).map(Number);
  if (values.length < 4 || values.some((value) => !Number.isFinite(value))) return null;
  const total = values.slice(0, 8).reduce((sum, value) => sum + value, 0);
  const idle = values[3] + (values[4] ?? 0);
  return { busy: total - idle, total };
}

/** A probe's `key=value` lines -> a sample; null when the essentials are missing. */
export function parseMachineProbe(hostOs: HostOs, stdout: string): MachineSample | null {
  const values = new Map<string, string>();
  for (const line of stdout.split(/\r?\n/)) {
    const at = line.indexOf('=');
    if (at > 0) values.set(line.slice(0, at).trim(), line.slice(at + 1).trim());
  }
  const n = (key: string): number | null => {
    const raw = values.get(key);
    if (raw === undefined || raw === '') return null;
    const value = Number(raw);
    return Number.isFinite(value) && value >= 0 ? value : null;
  };
  const cores = n('cores');
  const kib = (key: string): number | null => {
    const value = n(key);
    return value === null ? null : value * 1024;
  };
  let memTotal = kib('memTotalKb') ?? n('memTotalB');
  let memAvail = kib('memAvailKb');
  if (hostOs === 'mac') {
    const pageSize = n('pageSize');
    const pages = stdout
      .split(/\r?\n/)
      .filter((line) => line.startsWith('pagesAvail='))
      .map((line) => Number(line.slice('pagesAvail='.length)))
      .filter((value) => Number.isFinite(value));
    memAvail =
      pageSize !== null && pages.length > 0 ? pages.reduce((a, b) => a + b, 0) * pageSize : null;
  }
  const diskTotal = kib('diskTotalKb') ?? n('diskTotalB');
  const diskFree = kib('diskFreeKb') ?? n('diskFreeB');
  if (memTotal === 0) memTotal = null;
  if (cores === null && memTotal === null && diskTotal === null) return null;
  const cpuPct = n('cpuPct');
  const diskRead = diskTotal !== null && diskFree !== null && diskTotal > 0;
  return {
    cores,
    load1: n('load'),
    busyPct:
      cpuPct === null
        ? null
        : Math.min(100, Math.round(hostOs === 'mac' && cores ? cpuPct / cores : cpuPct)),
    memAvailableBytes: memAvail,
    memTotalBytes: memTotal,
    disk: diskRead
      ? {
          mount: shortText(values.get('mount'), 80) ?? '/',
          freeBytes: diskFree,
          totalBytes: diskTotal,
        }
      : null,
    // Every probe prints the system disk: an answer without it is a partial reading, not "no disk".
    ...(diskRead ? {} : { diskError: 'The machine did not report its system disk.' }),
    cpuTimes: hostOs === 'linux' ? linuxCpuTimes(values.get('cpu')) : null,
  };
}

/** The busy percent between two /proc/stat readings. */
export function busyBetween(
  before: MachineSample['cpuTimes'],
  after: MachineSample['cpuTimes']
): number | null {
  if (!before || !after) return null;
  const total = after.total - before.total;
  if (total <= 0) return null;
  return Math.max(0, Math.min(100, Math.round(((after.busy - before.busy) / total) * 100)));
}

/** Which of the four machines this server runs on, by the same host names the registry uses. */
export function localMachineId(
  host: string = os.hostname(),
  platform: NodeJS.Platform = process.platform
): MachineId {
  if (platform === 'darwin') return 'mac';
  if (platform === 'win32') return 'windows';
  return registryMachine(host, null) === 'nas1' ? 'nas1' : 'vm';
}

/** This computer, read directly (no process). */
export async function readLocalMachine(
  options: { statfs?: (target: string) => Promise<fs.StatsFs> } = {}
): Promise<MachineSample> {
  let memAvail: number | null = os.freemem();
  let cpuTimes: MachineSample['cpuTimes'] = null;
  try {
    const meminfo = await fs.promises.readFile('/proc/meminfo', 'utf8');
    const match = /^MemAvailable:\s+(\d+)\s+kB/m.exec(meminfo);
    if (match) memAvail = Number(match[1]) * 1024;
    const stat = await fs.promises.readFile('/proc/stat', 'utf8');
    cpuTimes = linuxCpuTimes(/^cpu\s+(.*)$/m.exec(stat)?.[1]);
  } catch {
    /* Not Linux: the os module's numbers stand. */
  }
  let disk: MachineSample['disk'] = null;
  let diskError: string | undefined;
  try {
    const stats = await (options.statfs ?? fs.promises.statfs)('/');
    disk = {
      mount: '/',
      freeBytes: Number(stats.bavail) * Number(stats.bsize),
      totalBytes: Number(stats.blocks) * Number(stats.bsize),
    };
  } catch {
    // The rest of the reading stands; the service keeps the earlier disk reading, named as failed.
    disk = null;
    diskError = 'The system disk could not be read.';
  }
  return {
    cores: os.cpus().length || null,
    load1: process.platform === 'win32' ? null : os.loadavg()[0],
    busyPct: null,
    memAvailableBytes: memAvail,
    memTotalBytes: os.totalmem(),
    disk,
    ...(diskError ? { diskError } : {}),
    cpuTimes,
  };
}

/** The ssh alias for a remote machine: the same fixed aliases the app-update jobs use. */
export function machineSshAlias(machine: Exclude<MachineId, 'vm'>): string | null {
  const alias = APP_UPDATE_SSH_HOSTS[machine];
  return alias && isSafeUsageSshAlias(alias) ? alias : null;
}

/** One reading of a remote machine over ssh; throws a fixed-word Error when it does not answer. */
export async function readRemoteMachine(
  machine: Exclude<MachineId, 'vm'>,
  run: CommandRunner = runCommand
): Promise<MachineSample> {
  const alias = machineSshAlias(machine);
  if (!alias) throw new ValidationError('No ssh host is configured for this machine.');
  const result = await run(
    'ssh',
    [
      '-T',
      '-o',
      'BatchMode=yes',
      '-o',
      'ConnectTimeout=5',
      '-o',
      'ConnectionAttempts=1',
      '-o',
      'ServerAliveInterval=5',
      '-o',
      'ServerAliveCountMax=2',
      '--',
      alias,
      machineProbeCommand(REMOTE_MACHINES[machine]),
    ],
    { timeoutMs: 20_000, maxBytes: 64 * 1024 }
  );
  if (result.code !== 0) throw new NetworkError(commandFailure('ssh', result));
  const sample = parseMachineProbe(REMOTE_MACHINES[machine], result.stdout);
  if (!sample) throw new NetworkError('The machine answered without a reading.');
  return sample;
}
