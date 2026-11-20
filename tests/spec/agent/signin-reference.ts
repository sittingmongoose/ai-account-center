// Pure TypeScript reference for the native sign-in projection fixture.
import type { SnapshotSignInProfile } from '../../../src/web-server/agent-usage/agent-usage-markdown';
import type {
  ClaudeSignInRenewal,
  ClaudeSignInRenewalRow,
} from '../../../src/web-server/services/claude-signin-renewal';

export interface SignInProfile extends SnapshotSignInProfile {
  /** The machine this report came from; present on every published row. */
  machine: string;
  /** A failed or unreachable host names itself instead of disappearing. */
  error?: string;
}

export interface SignInHealth {
  generatedAt: string;
  machines: Record<string, SignInProfile[]>;
  /** The renewal runner's own state, when one publishes into the snapshot folder. */
  renewal?: ClaudeSignInRenewal;
}

const WARN_MS = 24 * 3600 * 1000;
const CRITICAL_MS = 4 * 3600 * 1000;

/** One runner row in the collector's shape, with the renewal fields beside it. */
export function renewalProfile(row: ClaudeSignInRenewalRow, now: number): SignInProfile {
  const end = row.logoutAt ? Date.parse(row.logoutAt) : NaN;
  const state: SignInProfile['state'] =
    row.state === 'logged_out' || row.state === 'no_refresh_token'
      ? 'logged_out'
      : row.state !== 'ok'
        ? 'unavailable'
        : !Number.isFinite(end)
          ? 'ok'
          : end <= now
            ? 'logged_out'
            : end - now < CRITICAL_MS
              ? 'critical'
              : end - now < WARN_MS
                ? 'warn'
                : 'ok';
  return {
    name: row.profile,
    provider: 'claude',
    platform: row.host,
    machine: row.host,
    accessExpiresAt: null,
    refreshExpiresAt: row.signInEndsAt,
    loggedOutAt: row.logoutAt,
    state,
    fixCommand: '',
    ...renewalFields(row),
  };
}

function renewalFields(row: ClaudeSignInRenewalRow): Partial<SignInProfile> {
  return {
    email: row.email,
    autoRenew: row.autoRenew,
    renewFrom: row.renewFrom,
    lastRenewAt: row.lastRenewAt,
    lastResult: row.lastResult,
    nextVerifyAt: row.nextVerifyAt,
    reverts: row.reverts,
  };
}

/**
 * While the runner is current, its rows are the truth for every computer it read: it reads the
 * Mac's Keychain from the desktop session (ssh cannot) and covers Nas1. A computer it could not
 * read keeps this service's own rows, with the renewal fields added where the profile names match.
 */
export function mergeRenewal(
  machines: Record<string, SignInProfile[]>,
  renewal: ClaudeSignInRenewal | null,
  now: number
): Record<string, SignInProfile[]> {
  if (!renewal || renewal.stale) return machines;
  const merged: Record<string, SignInProfile[]> = { ...machines };
  const readOk = new Set(renewal.hosts.filter((h) => !h.error).map((h) => h.host));
  const byHost = new Map<string, ClaudeSignInRenewalRow[]>();
  for (const row of renewal.signIns) byHost.set(row.host, [...(byHost.get(row.host) ?? []), row]);
  for (const [host, rows] of byHost) {
    if (readOk.has(host)) {
      const others = (merged[host] ?? []).filter((r) => r.provider !== 'claude');
      merged[host] = [...rows.map((r) => renewalProfile(r, now)), ...others];
    } else if (merged[host]) {
      merged[host] = merged[host].map((r) => {
        const match = rows.find((x) => x.profile === r.name);
        return match ? { ...r, ...renewalFields(match) } : r;
      });
    }
  }
  return merged;
}

const CACHE_MIN_MS = 30_000;
const CACHE_MAX_MS = 5 * 60 * 1000;
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
