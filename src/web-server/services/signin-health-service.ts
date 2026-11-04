import { execFile } from 'child_process';
import * as path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import type { SnapshotSignInProfile } from '../agent-usage/agent-usage-snapshot';
import {
  configuredUsageHosts,
  localUsagePython,
  signinHelperBundle,
  usageHelperCommand,
} from './usage-failover-transport';

/**
 * Sign-in expiry health for every Claude OAuth login on the machines AAC
 * collects from. The collector (scripts/account-usage/signin_expiry.py) reads
 * only the two OAuth times and token presence from each config dir — token
 * values are never read or transported. Remote machines run the collector
 * from an in-memory stdin bundle, so nothing is ever installed on their
 * disks; the hosts are the same configured ones every other usage transport
 * uses. Results are cached for at most five minutes, never past a published
 * deadline: a stamp that crosses after collection drops the cache on read
 * (deadlines under the 30 s floor age out with the floor), while stamps
 * already past at collection are facts about the past and keep the cache.
 * One collection pass only: a row whose deadline crossed while its hosts
 * were still collecting is downgraded to unavailable in that same pass, so
 * the whole fetch always settles inside the publisher's slot.
 */

export interface SignInProfile extends SnapshotSignInProfile {
  /** The machine this report came from; present on every published row. */
  machine: string;
  /** A failed or unreachable host names itself instead of disappearing. */
  error?: string;
}

export interface SignInHealth {
  generatedAt: string;
  machines: Record<string, SignInProfile[]>;
}

const CACHE_MIN_MS = 30_000;
const CACHE_MAX_MS = 5 * 60 * 1000;
/** Each host's collection settles inside this; the publisher's slot is longer. */
const HOST_LIMIT_MS = 45_000;
const COLLECTOR = 'signin_expiry.py';
/** Accepted as-is; anything else is sanitized to a safe bounded identifier. */
const NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_.@+ -]{0,63}$/;
/** The non-secret Codex auth.json field names an unsupported row may list:
 * an arbitrary key from a malformed (or older remote) report could itself
 * be a credential, so nothing else publishes. Mirrors the collector. */
const CODEX_AUTH_FIELDS = new Set([
  'OPENAI_API_KEY',
  'auth_mode',
  'last_refresh',
  'tokens',
  'access_token',
  'refresh_token',
  'id_token',
  'account_id',
  'agent_identity',
  'personal_access_token',
  'bedrock_api_key',
  'bedrock_access_keys',
]);
/** A published timestamp must parse as a real calendar time; anything else reads as absent. */
const STAMP_PATTERN =
  /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/;

/** C0, DEL and C1 controls, Unicode line/paragraph separators, and the
 * bidi controls and marks (including U+061C, U+200E/U+200F and the BOM):
 * none of them ever publish. */
const CONTROL = /[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u2028-\u202e\u2066-\u2069\ufeff]/;

/** A surrogate code unit with no partner: how a non-UTF-8 directory name
 * arrives through JSON. UTF-8 output (the snapshot markdown) would
 * replace it with U+FFFD, silently retargeting a published path. */
const UNPAIRED_SURROGATE = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/;

/** The only failure words ever published for a failed collection. Raw
 * stderr names routing details (the resolved hostname or address behind
 * an ssh alias, the remote user), so it is classified, never quoted. */
export function collectorFailureWord(stderr: string): string {
  const text = stderr.toLowerCase();
  if (text.includes('timed out') || text.includes('timeout')) return 'collection timed out';
  if (text.includes('connection refused')) return 'connection refused';
  if (text.includes('permission denied')) return 'machine sign-in refused';
  if (
    text.includes('could not resolve') ||
    text.includes('no such host') ||
    text.includes('name or service not known') ||
    text.includes('no route to host') ||
    text.includes('network is unreachable') ||
    text.includes('connection reset') ||
    text.includes('host is down')
  )
    return 'host unreachable';
  return 'collector failed';
}

const FAILURE_WORDS = new Set([
  'collection timed out',
  'connection refused',
  'machine sign-in refused',
  'host unreachable',
  'collector failed',
  'no profiles reported',
]);

/** The publication boundary for every rejection, setup failures included:
 * a bundle read or process spawn can throw errors quoting absolute paths
 * or the ssh target, so only the fixed words above ever publish. */
export function publishableFailureReason(reason: unknown): string {
  return typeof reason === 'string' && FAILURE_WORDS.has(reason) ? reason : 'collector failed';
}

function runCollector(
  command: string,
  args: string[],
  timeoutMs: number,
  input?: string
): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = execFile(
      command,
      args,
      { timeout: timeoutMs, maxBuffer: 1024 * 1024 },
      (error, stdout, stderr) => {
        if (error && stdout.length === 0) {
          reject(new Error(collectorFailureWord(stderr ?? '')));
          return;
        }
        resolve(stdout);
      }
    );
    child.on('error', () => {
      /* handled through the empty-stdout branch above */
    });
    if (input !== undefined) {
      child.stdin?.on('error', () => {
        /* ssh exit is handled without remote text */
      });
      child.stdin?.end(input);
    }
  });
}

/** A safe bounded identifier: an unreadable name never deletes the row. */
export function sanitizeName(raw: unknown): string {
  if (typeof raw !== 'string') return 'unnamed';
  const cleaned = raw
    .replace(/[^\w.@+ -]/g, '_')
    .trim()
    .slice(0, 64);
  return cleaned || 'unnamed';
}

function stamp(value: unknown): string | null {
  if (typeof value !== 'string' || value.length > 40) return null;
  const parts = STAMP_PATTERN.exec(value);
  if (!parts) return null;
  const at = Date.parse(value);
  if (Number.isNaN(at)) return null;
  // Calendar, not just shape, in every offset form: 2026-02-30 must not
  // publish however it is suffixed. The components name a local wall time,
  // so they are validated as a date in themselves, offset aside.
  const [year, month, day, hour, minute, second] = parts.slice(1).map(Number);
  const wall = new Date(Date.UTC(year, month - 1, day, hour, minute, second));
  if (
    wall.getUTCFullYear() !== year ||
    wall.getUTCMonth() !== month - 1 ||
    wall.getUTCDate() !== day ||
    wall.getUTCHours() !== hour ||
    wall.getUTCMinutes() !== minute ||
    wall.getUTCSeconds() !== second
  )
    return null;
  return value;
}

/** A repair command that survives publication byte-exact or not at all:
 * never rewritten, never truncated mid-quote. A command containing a
 * control character (a newline inside a quoted path, say) or a backtick
 * cannot be rendered on one markdown line without changing what it does,
 * so — like an overlong one — it is omitted entirely: half or altered
 * commands are worse than none. */
export function publishableFixCommand(value: unknown): string {
  if (typeof value !== 'string') return '';
  // Paste-to-shell text also refuses the bidi set inside CONTROL: those
  // characters cannot change what the quoted command does, but they can
  // change how it reads.
  // The length bound leaves room for the longest measured forms —
  // the scoped Windows command (save/clear/restore of every selector
  // in a try/finally, plus the USERPROFILE fallback expression)
  // measures 711 characters with no path at all, 878 carrying a
  // 64-character account name and 888 a 180-character literal path.
  // Longer extremes (a near-255-character directory name) omit whole
  // rather than truncate, like every other unpublishable shape.
  return CONTROL.test(value) ||
    UNPAIRED_SURROGATE.test(value) ||
    value.includes('`') ||
    value.length > 1000
    ? ''
    : value;
}

/** The per-profile errors that ever publish: the collector's fixed
 * literals, plus this service's own downgrade wording. An allowlist, not
 * a screen — a separator-joined or relative path (`path=/home/…`,
 * `home/user/…`), an address or a credential shape can dodge a boundary
 * heuristic, but no unlisted text of any shape passes here. Anything
 * else collapses to the fixed word, the same discipline host-level
 * errors already get. */
const PROFILE_ERRORS = new Set([
  'profile directory resolves outside the allowed roots',
  'credentials file missing or unreadable',
  'expiry times unreadable',
  'keychain item unreadable',
  'keychain item holds no readable credentials',
  'keychain locked or access not allowed',
  'keychain state could not be checked',
  'auth file missing or unreadable',
  "profile discovery was incomplete; this machine's list may be missing homes",
  'sign-in state changed during collection; recheck pending',
]);

export function publishableError(value: unknown): string | null {
  if (typeof value !== 'string' || value === '') return null;
  return PROFILE_ERRORS.has(value) ? value : 'unavailable';
}

/** The published DTO is allowlisted; a changed helper cannot add fields or
 * break a row past its bounds — bounds clip, they never delete. */
export function allowProfile(raw: unknown, machine: string): SignInProfile | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const row = raw as Record<string, unknown>;
  const states = ['ok', 'warn', 'critical', 'logged_out', 'unavailable', 'unsupported'];
  if (typeof row.state !== 'string' || !states.includes(row.state)) return null;
  const provider = row.provider === 'codex' ? 'codex' : 'claude';
  const clippedDir =
    typeof row.configDir === 'string' &&
    !CONTROL.test(row.configDir) &&
    !UNPAIRED_SURROGATE.test(row.configDir)
      ? row.configDir.slice(0, 200)
      : undefined;
  // The clip itself must not split a surrogate pair at the bound.
  const configDir =
    clippedDir !== undefined && /[\ud800-\udbff]$/.test(clippedDir)
      ? clippedDir.slice(0, -1)
      : clippedDir;
  const error = row.state === 'unavailable' ? publishableError(row.error) : null;
  const profile: SignInProfile = {
    name:
      typeof row.name === 'string' && NAME_PATTERN.test(row.name)
        ? row.name
        : sanitizeName(row.name),
    provider,
    platform: machine,
    machine,
    configDir,
    accessExpiresAt: stamp(row.accessExpiresAt),
    refreshExpiresAt: stamp(row.refreshExpiresAt),
    loggedOutAt: stamp(row.loggedOutAt),
    state: row.state as SignInProfile['state'],
    fixCommand: publishableFixCommand(row.fixCommand),
    ...(error !== null ? { error } : {}),
    ...(row.state === 'unsupported' && Array.isArray(row.fieldsAvailable)
      ? {
          fieldsAvailable: row.fieldsAvailable
            .filter((f): f is string => typeof f === 'string' && CODEX_AUTH_FIELDS.has(f))
            .slice(0, 32),
        }
      : {}),
  };
  return profile;
}

function unavailableHost(machine: string, error: string): SignInProfile[] {
  return [
    {
      name: machine,
      provider: 'claude',
      platform: machine,
      machine,
      state: 'unavailable',
      fixCommand: '',
      error: error.replace(/\s+/g, ' ').trim().slice(0, 120),
    },
  ];
}

async function collectLocal(): Promise<string> {
  // The packaged helper, resolved from this module like the sibling usage
  // transports, run by this host's own installed python when it has one,
  // scoped to the dashboard's configured CCS directory (CCS_HOME and
  // --config-dir installs keep their saved-instance homes and venv).
  const helper = path.resolve(__dirname, '../../../scripts/account-usage', COLLECTOR);
  const ccsDir = getCcsDir();
  return runCollector(localUsagePython({ ccsDir }), [helper, '--ccs-dir', ccsDir], 20_000);
}

type UsageHostEntry = Awaited<ReturnType<typeof configuredUsageHosts>>[number];

async function collectRemote(host: UsageHostEntry): Promise<string> {
  // The collector runs from an in-memory stdin bundle: helper source never
  // lands on the remote disk, and a clean host reports instead of failing.
  const bundle = await signinHelperBundle();
  return runCollector(
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
      'ServerAliveCountMax=1',
      '--',
      host.sshHost as string,
      usageHelperCommand(host.platform),
    ],
    30_000,
    JSON.stringify({ request: {}, helperSource: bundle })
  );
}

export function parseRows(stdout: string, machine: string): SignInProfile[] {
  try {
    const value = JSON.parse(stdout);
    if (value && value.schemaVersion === 1 && Array.isArray(value.profiles)) {
      const rows: SignInProfile[] = value.profiles
        .map((row: unknown) => allowProfile(row, machine))
        .filter((row: SignInProfile | null): row is SignInProfile => row !== null);
      // Uniqueness is allocated on the final published form, after
      // sanitization and clipping: names that sanitize alike, and a
      // raw name equal to a generated suffix, still publish
      // distinctly — a name-keyed consumer must never lose a login.
      // Every published name is reserved up front so a generated
      // suffix never lands on another row's own name.
      const reserved = new Set(rows.map((r) => r.name));
      const used = new Set<string>();
      for (const r of rows) {
        if (!used.has(r.name)) {
          used.add(r.name);
          continue;
        }
        let n = 2;
        let candidate = `${r.name.slice(0, 64 - `-${n}`.length)}-${n}`;
        while (used.has(candidate) || reserved.has(candidate)) {
          n += 1;
          candidate = `${r.name.slice(0, 64 - `-${n}`.length)}-${n}`;
        }
        r.name = candidate;
        used.add(candidate);
      }
      return rows;
    }
  } catch {
    /* a partial or unreadable answer reads as unavailable */
  }
  return [];
}

interface CacheEntry {
  /** When collection started: only deadlines crossing after this moment
   * invalidate the cache — stamps already past at collection are facts
   * about the past (an idle login's expired access token, a logged-out
   * row's end), not news, and must not force a recollection every read. */
  collectedAt: number;
  until: number;
  health: Promise<SignInHealth>;
}

let cache: CacheEntry | null = null;

/** Whether one stamp of a live row has crossed by `now`. Access and
 * refresh stamps count only inside (since, now]: already past at
 * collection they are facts the row's state already accounts for. The
 * known login end is different: a live row whose end is already past
 * here cannot be a live future fact under any clock, so a remote whose
 * clock runs behind still counts as crossed — and the same holds for
 * a past refresh end when the row names no login end at all. */
function stampCrossed(value: string | null | undefined, since: number, now: number): boolean {
  if (!value) return false;
  const at = Date.parse(value);
  return !Number.isNaN(at) && at > since && at <= now;
}

export function deadlineCrossedForRow(
  row: {
    accessExpiresAt?: string | null;
    refreshExpiresAt?: string | null;
    loggedOutAt?: string | null;
  },
  since: number,
  now: number
): boolean {
  const end = row.loggedOutAt ? Date.parse(row.loggedOutAt) : NaN;
  if (!Number.isNaN(end) && end <= now) return true;
  // With no named login end, a refresh end already past at `now` is the
  // same skew shape through the other stamp: no single-clock collector
  // emits it live (a past refresh end with no derivable end reads
  // logged_out at the source), so it cannot be a live fact here either.
  // A named future end exempts it — that is the legitimate dying
  // window, living on its last access token until the named end.
  if (Number.isNaN(end)) {
    const refresh = row.refreshExpiresAt ? Date.parse(row.refreshExpiresAt) : NaN;
    if (!Number.isNaN(refresh) && refresh <= now) return true;
  }
  return (
    stampCrossed(row.accessExpiresAt, since, now) || stampCrossed(row.refreshExpiresAt, since, now)
  );
}

export function hasPassedDeadline(health: SignInHealth, since: number, now: number): boolean {
  for (const rows of Object.values(health.machines)) {
    for (const row of rows) {
      // Only a live state's claim can be falsified by a crossing, exactly
      // as in downgradeCrossedRows: a terminal row's crossed end was
      // already observed as terminal and must not force a recollection.
      if (row.state !== 'ok' && row.state !== 'warn' && row.state !== 'critical') continue;
      if (deadlineCrossedForRow(row, since, now)) return true;
    }
  }
  return false;
}

/** The earliest future state deadline across a report, or null. */
export function nextDeadlineAt(health: SignInHealth, nowMs: number): number | null {
  let earliest: number | null = null;
  for (const rows of Object.values(health.machines)) {
    for (const row of rows) {
      for (const value of [row.accessExpiresAt, row.refreshExpiresAt, row.loggedOutAt]) {
        if (!value) continue;
        const at = Date.parse(value);
        if (Number.isNaN(at) || at <= nowMs) continue;
        if (earliest === null || at < earliest) earliest = at;
      }
    }
  }
  return earliest;
}

/** Cache until the next published deadline — a cached state must not
 * outlive the time it names — but never shorter than the floor; a deadline
 * under the floor is caught by the crossed-deadline check on read instead. */
export function cacheTtlFor(health: SignInHealth, nowMs: number): number {
  const deadline = nextDeadlineAt(health, nowMs);
  if (deadline === null) return CACHE_MAX_MS;
  return Math.max(CACHE_MIN_MS, Math.min(CACHE_MAX_MS, deadline - nowMs));
}

export function getSignInHealth(): Promise<SignInHealth> {
  const now = Date.now();
  if (cache && now < cache.until) {
    // A deadline that crossed after collection — including one that passed
    // while its own collection was still running — drops the cache on read.
    const entry = cache;
    return entry.health.then((value) => {
      if (!hasPassedDeadline(value, entry.collectedAt, Date.now())) return value;
      // Concurrent waiters share one recollection: only the waiter that
      // still owns the cache slot replaces it; the rest join the fresh one.
      if (cache !== entry) return cache ? cache.health : refresh();
      cache = null;
      return refresh();
    });
  }
  return refresh();
}

/** One collection pass. Hosts race their own settlement deadline, so a
 * single wedged machine cannot hold allSettled past the publisher's slot
 * and wipe successful machines' rows with it. */
async function collectOnce(): Promise<SignInHealth> {
  const machines: Record<string, SignInProfile[]> = {};
  const hosts = await configuredUsageHosts();
  const results = await Promise.allSettled(
    hosts.map((host) => {
      const runner = host.sshHost ? collectRemote(host) : collectLocal();
      return new Promise<string>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('collection timed out')), HOST_LIMIT_MS);
        timer.unref?.();
        runner.then(
          (value) => {
            clearTimeout(timer);
            resolve(value);
          },
          (error) => {
            clearTimeout(timer);
            reject(error instanceof Error ? error : new Error(String(error)));
          }
        );
      });
    })
  );
  hosts.forEach((host, index) => {
    const result = results[index];
    if (result.status === 'fulfilled') {
      const rows = parseRows(result.value, host.host);
      if (rows.length > 0) {
        machines[host.host] = rows;
        return;
      }
    }
    // Every rejection passes the allowlist boundary: a setup failure (the
    // bundle read, a spawn error) can quote absolute paths or the ssh
    // target, so its text never reaches the published error either.
    const reason =
      result.status === 'rejected'
        ? publishableFailureReason(
            result.reason instanceof Error ? result.reason.message : result.reason
          )
        : 'no profiles reported';
    machines[host.host] = unavailableHost(host.host, reason);
  });
  if (!machines.ubuntu) {
    // A dashboard hosted on the Mac or Windows collects no Ubuntu route:
    // the machine names itself rather than silently disappearing.
    machines.ubuntu = unavailableHost('ubuntu', 'no collection route from this host');
  }
  return { generatedAt: new Date().toISOString(), machines };
}

/** Rows whose state may already be stale: a live state's deadline that
 * crossed while collection was running. Terminal and unreadable rows make
 * no claim a crossing can falsify, so they stand as collected. */
export function downgradeCrossedRows(health: SignInHealth, since: number, now: number): boolean {
  let downgraded = false;
  for (const rows of Object.values(health.machines)) {
    for (const row of rows) {
      if (row.state !== 'ok' && row.state !== 'warn' && row.state !== 'critical') continue;
      const crossed = deadlineCrossedForRow(row, since, now);
      if (!crossed) continue;
      // The state was computed against times that have already passed; say
      // so instead of publishing it as current. The stamps go with it, so
      // the row cannot re-trigger collection on every later read.
      row.state = 'unavailable';
      row.accessExpiresAt = null;
      row.refreshExpiresAt = null;
      row.loggedOutAt = null;
      row.error = 'sign-in state changed during collection; recheck pending';
      downgraded = true;
    }
  }
  return downgraded;
}

function refresh(): Promise<SignInHealth> {
  const now = Date.now();
  const entry: CacheEntry = {
    collectedAt: now,
    until: now + CACHE_MAX_MS,
    health: undefined as unknown as Promise<SignInHealth>,
  };
  entry.health = (async (): Promise<SignInHealth> => {
    let value: SignInHealth;
    try {
      // One pass, always inside the publisher's slot: rows a deadline
      // crossed mid-collection are downgraded in place, never recollected
      // here, and the short floor below lets the next sample recheck them.
      value = await collectOnce();
    } catch (error) {
      // A collection that could not even resolve its hosts is a failed
      // collection, not an empty success: the rejection reaches the
      // snapshot publisher, which publishes its named collection-
      // failed row. Swallowing it into an empty map would silently
      // drop the whole sign-in section. The retry waits only the floor.
      entry.until = Date.now() + CACHE_MIN_MS;
      throw error;
    }
    const settledAt = Date.now();
    const downgraded = downgradeCrossedRows(value, entry.collectedAt, settledAt);
    entry.until = settledAt + (downgraded ? CACHE_MIN_MS : cacheTtlFor(value, settledAt));
    return value;
  })();
  cache = entry;
  return entry.health;
}

export function invalidateSignInHealthCache(): void {
  cache = null;
}
