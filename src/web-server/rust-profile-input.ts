/**
 * M3 part 4 inputs and reply gates for the profile read routes: Claude desktop profiles and
 * their cached usage, Codex profiles and quotas, Antigravity profiles and quotas. Inputs carry
 * only what the guarded TS handler already read from its owners (the remote history text, the
 * Codex registry and display-safe login fields, collector rows and renewal notes, the
 * Antigravity runtime's inventory and accounts); nothing here reads a file, a credential or a
 * provider. An input JSON cannot carry exactly returns null, so the bridge logs a TS fallback.
 */
import type { ClaudeDesktopUsageFacts } from './services/claude-desktop-usage-service';
import type { CodexAuthProfilesFacts } from '../codex-auth/codex-auth-dashboard-service';
/** The quota composition input the retired service read (the Rust route reads its own now). */
interface CodexProfileQuotaFacts {
  profiles: Array<{ name: string; authValid: boolean }>;
  rows: unknown[];
  renewals: ReadonlyMap<string, unknown>;
  now: number;
}
import { rustReadInput } from './rust-read-input';

function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

/**
 * Whether `JSON.stringify` carries `value` exactly: null, booleans, strings, finite numbers,
 * arrays without holes and plain objects. An own member whose value is undefined is dropped,
 * which the composers here read the same as an absent one; a caller whose composer tells the
 * two apart checks those members itself.
 */
export function jsonExact(value: unknown, depth = 0): boolean {
  if (depth > 64) return false;
  if (value === null || typeof value === 'boolean' || typeof value === 'string') return true;
  if (typeof value === 'number') return Number.isFinite(value);
  if (Array.isArray(value)) {
    for (let index = 0; index < value.length; index++)
      if (!(index in value) || !jsonExact(value[index], depth + 1)) return false;
    return true;
  }
  if (!value || typeof value !== 'object') return false;
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) return false;
  for (const key of Object.keys(value)) {
    if (key === '__proto__') return false;
    const member = (value as Record<string, unknown>)[key];
    if (member !== undefined && !jsonExact(member, depth + 1)) return false;
  }
  return true;
}

function keys(value: unknown, expected: string[]): value is Record<string, unknown> {
  return object(value) && Object.keys(value).join('\n') === expected.join('\n');
}

/** ISO facts only: the composers read `Date.parse` epochs and `toISOString` words. */
function envelope(data: unknown): unknown {
  return jsonExact(data)
    ? rustReadInput(data, 'UTC', [], { rankAll: false, clockAndDay: false })
    : null;
}

/** Rust's manifest rows; TS adds this process's Open progress, as the original handler does. */
export function acceptRustDesktopProfiles(
  withOperations: (profiles: Record<string, unknown>[]) => unknown[]
): (body: unknown) => { value: unknown } | null {
  return (body) =>
    keys(body, ['profiles']) && Array.isArray(body.profiles) && body.profiles.every(object)
      ? { value: { profiles: withOperations(body.profiles) } }
      : null;
}

const OPEN_OPERATION_STATES = [
  'checking',
  'copying',
  'opening',
  'opened',
  'blocked_uncertain',
  'failed',
];

function isOpenOperation(value: unknown): boolean {
  if (value === null) return true;
  const count = (count: unknown) =>
    count === null || (typeof count === 'number' && Number.isSafeInteger(count) && count >= 0);
  return (
    keys(value, ['id', 'platform', 'state', 'confirmedCount', 'totalCount', 'message']) &&
    typeof value.id === 'string' &&
    (value.platform === 'mac' || value.platform === 'windows') &&
    typeof value.state === 'string' &&
    OPEN_OPERATION_STATES.includes(value.state) &&
    count(value.confirmedCount) &&
    count(value.totalCount) &&
    (value.message === null || typeof value.message === 'string')
  );
}

/**
 * G13: when the Rust Open owner is bound, the service's manifest rows already carry each
 * Open's `openOperation`; the composition validates that shape and serves the rows as they
 * are instead of adding this process's (empty) store.
 */
export function acceptRustDesktopProfilesWithOperations(body: unknown): { value: unknown } | null {
  if (!keys(body, ['profiles']) || !Array.isArray(body.profiles) || !body.profiles.every(object))
    return null;
  for (const profile of body.profiles) {
    if (typeof profile.id === 'string' && profile.id && !isOpenOperation(profile.openOperation))
      return null;
  }
  return { value: body };
}

/** The usage composition input: each profile's identity and what its history read found. */
export function rustDesktopUsageInput(facts: ClaudeDesktopUsageFacts): unknown {
  const input = {
    platform: facts.platform,
    fetchedAt: facts.fetchedAt,
    profiles: facts.profiles.map(({ profile, read }) => ({
      ...(profile.id === undefined ? {} : { id: profile.id }),
      email: profile.email,
      read: read.state,
      ...(read.state === 'read' ? { history: read.history } : {}),
    })),
  };
  return jsonExact(input) ? input : null;
}

export function isRustDesktopUsage(value: unknown): boolean {
  return keys(value, ['platform', 'fetchedAt', 'profiles']) && Array.isArray(value.profiles);
}

/** The summary input: registry names in key order, display-safe login fields, env overrides. */
export function rustCodexProfilesInput(facts: CodexAuthProfilesFacts): unknown {
  return jsonExact(facts) ? facts : null;
}

export function isRustCodexProfiles(value: unknown): boolean {
  return (
    keys(value, ['active', 'activated', 'default', 'profiles']) && Array.isArray(value.profiles)
  );
}

/**
 * The quota input: profile names and login validity, the collector-row fields the composer
 * reads, renewal notes in Map order, and the response clock, in the ISO-only envelope.
 */
export function rustCodexQuotasInput(facts: CodexProfileQuotaFacts): unknown {
  const rows: unknown[] = [];
  for (const row of facts.rows as unknown[]) {
    if (!object(row)) return null;
    const windows = row.quotaWindows;
    if (windows !== undefined && windows !== null && !Array.isArray(windows)) return null;
    const picked: unknown[] | null | undefined = Array.isArray(windows) ? [] : windows;
    for (const window of Array.isArray(windows) ? (windows as unknown[]) : []) {
      if (!object(window)) return null;
      (picked as unknown[]).push({
        key: window.key,
        label: window.label,
        usedPercent: window.usedPercent,
        resetAt: window.resetAt,
      });
    }
    rows.push({
      profile: row.profile,
      quotaStatus: row.quotaStatus,
      needsReauth: row.needsReauth,
      fetchedAt: row.fetchedAt,
      staleAsOf: row.staleAsOf,
      quotaWindows: picked,
    });
  }
  const renewals: unknown[] = [];
  for (const [name, entry] of facts.renewals as ReadonlyMap<unknown, unknown>)
    renewals.push([
      name,
      object(entry) ? { state: entry.state, message: entry.message } : (entry ?? null),
    ]);
  return envelope({
    profiles: facts.profiles.map((profile) => ({
      name: profile.name,
      authValid: profile.authValid,
    })),
    rows,
    renewals,
    now: facts.now,
  });
}

export function isRustCodexQuotas(value: unknown): boolean {
  return keys(value, ['profiles']) && Array.isArray(value.profiles);
}

/** The runtime's inventory as read, in the ISO-only envelope. */
export function rustAntigravityInventoryInput(inventory: unknown): unknown {
  return envelope(inventory);
}

export function isRustAntigravityInventory(value: unknown): boolean {
  if (!object(value)) return false;
  const names = Object.keys(value).join('\n');
  return (
    [
      'schemaVersion\nhostId\nprofiles',
      'schemaVersion\nhostId\nruntimeServiceProblem\nprofiles',
    ].includes(names) && Array.isArray(value.profiles)
  );
}

/**
 * The runtime's accounts as read, the response instant and this engine's clock (the plan's
 * model list falls back to it), in the ISO-only envelope. A window composes `remaining` and
 * `expiresAt` from their own-member presence, which JSON cannot carry for an undefined value.
 */
export function rustAntigravityQuotasInput(accounts: unknown, at: number): unknown {
  if (!Array.isArray(accounts)) return null;
  for (const account of accounts as unknown[])
    if (object(account) && Array.isArray(account.windows))
      for (const window of account.windows as unknown[])
        if (
          object(window) &&
          ['remaining', 'expiresAt'].some(
            (key) => Object.prototype.hasOwnProperty.call(window, key) && window[key] === undefined
          )
        )
          return null;
  return envelope({ accounts, now: at, clock: Date.now() });
}

export function isRustAntigravityQuotas(value: unknown): boolean {
  return keys(value, ['schemaVersion', 'hostId', 'accounts']) && Array.isArray(value.accounts);
}
