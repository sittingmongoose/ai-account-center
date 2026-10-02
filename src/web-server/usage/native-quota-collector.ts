/**
 * Saved native Codex profile quota collector: the server-side fetch surface for
 * the subscription quota of the user's saved Codex logins.
 *
 * The dashboard, the trays and the Codex auto-switch read this through the
 * account services and never call ChatGPT themselves. The upstream endpoint is
 * undocumented and hostile to polling (persistent 429s, no Retry-After), so the
 * controls below exist to protect the user's accounts:
 *
 *   - long TTL (10 min) on-demand cache, never a tight timer loop
 *   - in-flight coalescing so concurrent callers share one fetch
 *   - Retry-After honored; exponential backoff + jitter on 429/5xx
 *   - circuit breaker stops calling after repeated 429s for a cooldown
 *   - serve-stale-on-failure; a profile with no data is parked, never invented
 *
 * Each saved profile reads its own auth.json (codex-instances/<name>/auth.json,
 * or ~/.codex/auth.json for the bare default) and polls
 * chatgpt.com/backend-api/wham/usage with that token and workspace
 * (fetchCodexQuotaWithToken). Only the bare default may fall back to the local
 * session logs (getCodexLocalQuota), because those logs belong to ~/.codex.
 *
 * Each profile has its own ProviderState, so a 429 on one profile never trips
 * the breaker of another. State is keyed by the CCS home as well as the profile
 * name, and a changed auth file discards the cached quota and cooldowns.
 */

import { fetchCodexQuotaWithToken } from '../../cliproxy/quota/quota-fetcher-codex';
import { getProviderAccounts } from '../../cliproxy/accounts/query';
import { getCodexLocalQuota, type CodexLocalQuota } from './codex-local-quota-collector';
import type { CodexQuotaResult } from '../../cliproxy/quota/quota-types';
import type { BarSummaryRow, QuotaWindowDetail } from '../routes/bar-routes';
import { getCodexAdditionalUsageWindows } from './codex-network-extra-windows';

import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { createHash } from 'node:crypto';
import { getCcsDir } from '../../utils/config-manager';
import { resolveCodexProfileDir } from '../../codex-auth/codex-profile-paths';

// ============================================================================
// Safety constants (concrete, named, module-level)
// ============================================================================

/** On-demand cache TTL. Floor is 5 min; we use 10 min because the dashboard,
 *  the trays and the auto-switch ask far more often than a hook fires. */
const NATIVE_QUOTA_TTL_MS = 600_000; // 10 minutes
// After a 401/expired result, cache the reauth row and cool the profile down so
// an expired account is shown dimmed and re-checked at most this often instead
// of being re-polled (and re-401'd) on every dashboard or tray refresh.
const REAUTH_COOLDOWN_MS = NATIVE_QUOTA_TTL_MS; // 10 minutes
// Parked rows (no on-disk creds, quotaStatus 'unsupported') re-check on a short
// TTL — re-statting credentials is cheap, and this lets a profile that the user
// just logged into appear within seconds instead of staying dimmed for the full
// quota TTL. (Reauth/error rows keep the full TTL + cooldown to avoid re-401s.)
const PARKED_TTL_MS = 30_000; // 30 seconds

/** Exponential backoff base; delay = min(base * 2^n, MAX) + jitter. */
const RETRY_BASE_MS = 1_000;

/** Ceiling for any single backoff / Retry-After cooldown derived from one call. */
const MAX_BACKOFF_MS = 60_000; // 1 minute

/** Jitter added to backoff to avoid synchronized retries. */
const JITTER_MAX_MS = 500;

/** Consecutive 429s that trip the breaker open. */
const CB_TRIP_THRESHOLD = 3;

/** How long the breaker stays open (zero network) once tripped. */
const CB_COOLDOWN_MS = 900_000; // 15 minutes

// Surface identifier
const SURFACE_CODEX = 'ccsx';

// The "default way of running" a surface: the bare ~/.codex login, as opposed to
// a named saved profile.
const DEFAULT_PROFILE = 'default';

// Provider value on the wire
const CODEX_NATIVE_PROVIDER = 'codex';

// ============================================================================
// Injectable dependencies (tests inject mocks; never live endpoints in CI)
// ============================================================================

export interface NativeQuotaDeps {
  /** Fetch a saved native profile's quota with its own token and workspace. */
  fetchCodexQuotaWithToken?: (accessToken: string, accountId: string) => Promise<CodexQuotaResult>;
  /** Read Codex quota from local session logs (zero network, fallback). */
  getCodexQuota?: () => Promise<CodexLocalQuota | null>;
  /**
   * Read the native Codex auth for a profile (file-only, no keychain).
   * DEFAULT_PROFILE ('default') reads ~/.codex/auth.json; other names read codex-instances/<name>/auth.json.
   * Returns null when absent/unparseable.
   */
  readCodexNativeAuth?: (profile: string) => NativeCodexAuth | null;
  /** Resolve the default Codex profile name. */
  defaultCodexProfile?: () => string | null;
  /** Clock seam for deterministic backoff/TTL/breaker tests. */
  now?: () => number;
  /** Sleep seam (no real delay in tests). */
  sleep?: (ms: number) => Promise<void>;
}

interface NativeCodexAuth {
  accessToken: string;
  accountId: string;
  /** Internal digest of the saved file; never returned on a dashboard DTO. */
  cacheSignature?: string;
}

// ============================================================================
// Per-provider mutable state (module-scoped; reset() for tests)
// ============================================================================

interface ProviderState {
  /** Last successfully-built row, kept for stale-on-fail and TTL serving. */
  cachedRow: BarSummaryRow | null;
  /** Epoch ms when cachedRow was produced. */
  cachedAt: number;
  /** Shared in-flight promise; concurrent callers await this, not a new fetch. */
  pending: Promise<BarSummaryRow | null> | null;
  /** Consecutive 429 count toward the breaker threshold. */
  consecutive429: number;
  /** Epoch ms until which the breaker is open (no network). */
  breakerOpenUntil: number;
  /** Epoch ms until which a Retry-After / backoff cooldown holds. */
  cooldownUntil: number;
  /** Attempt counter feeding exponential backoff. */
  backoffAttempt: number;
  authSignature?: string;
  authReader?: (profile: string) => NativeCodexAuth | null;
}

function freshProviderState(): ProviderState {
  return {
    cachedRow: null,
    cachedAt: 0,
    pending: null,
    consecutive429: 0,
    breakerOpenUntil: 0,
    cooldownUntil: 0,
    backoffAttempt: 0,
  };
}

// Per-profile state, keyed by CCS home and profile name
const codexProfileStates = new Map<string, ProviderState>();

function stateKey(profile: string): string {
  return `${path.resolve(getCcsDir())}\0${profile}`;
}

function getState(profile: string): ProviderState {
  const key = stateKey(profile);
  let s = codexProfileStates.get(key);
  if (!s) {
    s = freshProviderState();
    codexProfileStates.set(key, s);
  }
  return s;
}

function getProfileState(profile: string): ProviderState | undefined {
  const state = codexProfileStates.get(stateKey(profile));
  return state?.authReader ? validateCodexProfileState(profile, state.authReader).state : state;
}

/** Changed credentials discard quota, pending work, and old-account cooldowns before serving cache. */
function validateCodexProfileState(
  profile: string,
  reader: (profile: string) => NativeCodexAuth | null
): { state: ProviderState; auth: NativeCodexAuth | null } {
  let auth: NativeCodexAuth | null;
  try {
    auth = reader(profile);
  } catch {
    auth = null;
  }
  const signature = auth
    ? (auth.cacheSignature ??
      createHash('sha256')
        .update(JSON.stringify([auth.accessToken, auth.accountId]))
        .digest('hex'))
    : 'missing';
  let state = getState(profile);
  if (state.authSignature !== signature) {
    state = freshProviderState();
    state.authSignature = signature;
    codexProfileStates.set(stateKey(profile), state);
  }
  state.authReader = reader;
  return { state, auth };
}

/** Reset all module state. Tests call this to avoid cross-test pollution. */
export function resetNativeQuotaState(): void {
  codexProfileStates.clear();
}

// ============================================================================
// Helpers
// ============================================================================

function defaultSleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Parse Retry-After (seconds or HTTP-date) into ms, capped at MAX_BACKOFF_MS. */
function parseRetryAfterMs(detail: string | undefined, now: number): number | null {
  if (!detail) return null;
  const match = /retry-after:(.+)$/.exec(detail);
  if (!match) return null;
  const raw = match[1].trim();

  const asSeconds = Number(raw);
  if (Number.isFinite(asSeconds)) {
    return Math.min(Math.max(0, asSeconds) * 1000, MAX_BACKOFF_MS);
  }

  const asDate = Date.parse(raw);
  if (Number.isFinite(asDate)) {
    return Math.min(Math.max(0, asDate - now), MAX_BACKOFF_MS);
  }
  return null;
}

function computeBackoffMs(attempt: number): number {
  const exp = Math.min(RETRY_BASE_MS * 2 ** attempt, MAX_BACKOFF_MS);
  const jitter = Math.floor(Math.random() * JITTER_MAX_MS);
  return exp + jitter;
}

/** Window length in minutes for the Codex core windows. */
const FIVE_HOUR_MINUTES = 300;
const SEVEN_DAY_MINUTES = 10080;

/** Map the Codex local windows into the row's per-window detail shape. */
function buildCodexQuotaWindows(quota: CodexLocalQuota): QuotaWindowDetail[] {
  return quota.windows.map((w) => ({
    key: w.key,
    label: w.label,
    usedPercent: w.usedPercent,
    remainingPercent: w.remainingPercent,
    resetAt: w.resetAt,
    windowMinutes: w.windowMinutes,
  }));
}

function buildCodexRow(
  quota: CodexLocalQuota,
  now: number,
  surface: string,
  profile: string
): BarSummaryRow {
  const quotaWindows = buildCodexQuotaWindows(quota);
  return {
    account_id: `${surface}:${profile}`,
    provider: CODEX_NATIVE_PROVIDER,
    surface,
    profile,
    is_subscription: true,
    displayName: profile,
    tier: quota.tier,
    paused: false,
    quota_percentage: quota.quotaPercentage,
    quotaStatus: 'ok',
    quotaSource: 'local',
    next_reset: quota.nextReset,
    is_default: false,
    last_activity_at: null,
    today_cost: null,
    // Codex is a local read; a stale source still reflects real usage so we keep
    // quotaStatus 'ok' but flag health 'warning' to hint freshness.
    health: quota.stale ? 'warning' : 'ok',
    cached: false,
    fetchedAt: new Date(now).toISOString(),
    needsReauth: false,
    ...(quotaWindows.length > 0 ? { quotaWindows } : {}),
    // staleAsOf is only present (and serialized) when the source session is old.
    ...(quota.staleAsOf ? { staleAsOf: quota.staleAsOf } : {}),
  };
}

/**
 * Build the Codex row from a LIVE network quota result.
 *
 * Uses coreUsage (5h/weekly) to produce QuotaWindowDetail entries with stable
 * keys. quota_percentage = min remaining across present core windows.
 * next_reset = soonest core resetAt. No staleAsOf on a live result.
 */
function buildCodexNetworkRow(
  quota: CodexQuotaResult,
  now: number,
  surface: string,
  profile: string
): BarSummaryRow {
  const windows: QuotaWindowDetail[] = [];

  const fiveHour = quota.coreUsage?.fiveHour;
  if (fiveHour) {
    windows.push({
      key: 'five_hour',
      label: '5h',
      usedPercent: 100 - fiveHour.remainingPercent,
      remainingPercent: fiveHour.remainingPercent,
      resetAt: fiveHour.resetAt,
      windowMinutes: FIVE_HOUR_MINUTES,
    });
  }

  const weekly = quota.coreUsage?.weekly;
  if (weekly) {
    windows.push({
      key: 'seven_day',
      label: 'week',
      usedPercent: 100 - weekly.remainingPercent,
      remainingPercent: weekly.remainingPercent,
      resetAt: weekly.resetAt,
      windowMinutes: SEVEN_DAY_MINUTES,
    });
  }

  // quota_percentage = min remaining across the windows present
  const coreWindows = [fiveHour, weekly].filter((w): w is NonNullable<typeof w> => !!w);
  const quotaPercentage =
    coreWindows.length > 0 ? Math.min(...coreWindows.map((w) => w.remainingPercent)) : null;

  // next_reset = soonest resetAt across present core windows
  const resets = coreWindows
    .map((w) => w.resetAt)
    .filter((r): r is string => typeof r === 'string')
    .map((r) => ({ iso: r, ms: new Date(r).getTime() }))
    .filter((r) => Number.isFinite(r.ms))
    .sort((a, b) => a.ms - b.ms);
  const nextReset = resets.length > 0 ? resets[0].iso : null;

  const extraUsage = getCodexAdditionalUsageWindows(quota);
  windows.push(...extraUsage.quotaWindows);

  return {
    account_id: `${surface}:${profile}`,
    provider: CODEX_NATIVE_PROVIDER,
    surface,
    profile,
    is_subscription: true,
    displayName: profile,
    tier: quota.planType ?? null,
    paused: false,
    quota_percentage: quotaPercentage,
    quotaStatus: 'ok',
    quotaSource: 'network',
    next_reset: nextReset,
    is_default: false,
    last_activity_at: null,
    today_cost: null,
    health: 'ok',
    cached: false,
    fetchedAt: new Date(now).toISOString(),
    needsReauth: false,
    // No staleAsOf — live data is always fresh.
    ...(windows.length > 0 ? { quotaWindows: windows } : {}),
    ...(extraUsage.balanceWindows.length > 0 ? { balanceWindows: extraUsage.balanceWindows } : {}),
  };
}

/** Return the cached row marked cached=true (used for TTL + stale serving). */
function serveCached(state: ProviderState): BarSummaryRow | null {
  if (!state.cachedRow) return null;
  return { ...state.cachedRow, cached: true };
}

// ============================================================================
// Saved profile helpers (production implementations, DI-overridable)
// ============================================================================

/**
 * Read Codex native auth from the profile's on-disk auth.json.
 * 'default' reads ~/.codex/auth.json; other names read codex-instances/<name>/auth.json.
 * Returns null when absent or unparseable.
 */
function readCodexNativeAuthFromDisk(profile: string): NativeCodexAuth | null {
  try {
    let authPath: string;
    if (profile === DEFAULT_PROFILE) {
      authPath = path.join(os.homedir(), '.codex', 'auth.json');
    } else {
      // Validate the saved profile name before reading under codex-instances.
      authPath = path.join(resolveCodexProfileDir(profile), 'auth.json');
    }

    if (!fs.existsSync(authPath)) return null;
    const raw = fs.readFileSync(authPath, 'utf8');
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const tokens = parsed.tokens as Record<string, unknown> | undefined;
    if (!tokens) return null;
    const accessToken = tokens.access_token;
    const accountId = tokens.account_id;
    if (typeof accessToken !== 'string' || !accessToken) return null;
    return {
      accessToken,
      accountId: typeof accountId === 'string' ? accountId : '',
      cacheSignature: createHash('sha256').update(raw).digest('hex'),
    };
  } catch {
    return null;
  }
}

/**
 * Resolve the default Codex profile. Falls back to DEFAULT_PROFILE when the bare
 * ~/.codex/auth.json exists and no registry default is set.
 */
function getDefaultCodexProfileFromDisk(): string | null {
  try {
    const { CodexProfileRegistry } = require('../../codex-auth/codex-profile-registry') as {
      CodexProfileRegistry: new () => {
        getDefault: () => string | null;
        getProfile: (name: string) => { email?: string; account_id?: string };
        listProfiles: () => string[];
      };
    };
    const registry = new CodexProfileRegistry();
    const pausedIdentities = getPausedCodexAccountIdentities();
    const def = registry.getDefault();
    if (def && !codexProfileMatchesPausedAccount(def, registry.getProfile(def), pausedIdentities)) {
      return def;
    }
    const fallback = registry
      .listProfiles()
      .find(
        (name) =>
          !codexProfileMatchesPausedAccount(name, registry.getProfile(name), pausedIdentities)
      );
    if (fallback) return fallback;
    // Fall back to the bare ~/.codex account (DEFAULT_PROFILE) when no registry
    // default is set — it is the default `ccsx` invocation.
    if (fs.existsSync(path.join(os.homedir(), '.codex', 'auth.json'))) return DEFAULT_PROFILE;
    return null;
  } catch {
    return null;
  }
}

function getPausedCodexAccountIdentities(): Set<string> | null {
  try {
    const accounts = getProviderAccounts('codex');
    if (accounts.length === 0) return null;
    const identities = new Set<string>();
    for (const account of accounts) {
      if (!account.paused) continue;
      identities.add(account.id);
      if (account.email) identities.add(account.email);
    }
    return identities;
  } catch {
    return null;
  }
}

function codexProfileMatchesPausedAccount(
  profileName: string,
  profile: { email?: string; account_id?: string },
  pausedIdentities: Set<string> | null
): boolean {
  if (!pausedIdentities || pausedIdentities.size === 0) return false;
  return [profileName, profile.email, profile.account_id].some(
    (identity): identity is string => typeof identity === 'string' && pausedIdentities.has(identity)
  );
}

// ============================================================================
// Per-profile collector with full safety controls
// ============================================================================

function buildParkedCodexProfileRow(profile: string, now: number): BarSummaryRow {
  return {
    account_id: `${SURFACE_CODEX}:${profile}`,
    provider: CODEX_NATIVE_PROVIDER,
    surface: SURFACE_CODEX,
    profile,
    is_subscription: true,
    displayName: profile,
    tier: null,
    paused: true,
    quota_percentage: null,
    quotaStatus: 'unsupported',
    next_reset: null,
    is_default: false,
    last_activity_at: null,
    today_cost: null,
    health: 'ok',
    cached: false,
    fetchedAt: new Date(now).toISOString(),
    needsReauth: true,
  };
}

/**
 * Tag a row with whether it is the surface's default profile (drives the
 * "active" badge only). The row's own `paused` flag stays authoritative for
 * dimming.
 */
function markDefault(row: BarSummaryRow, isDefault: boolean): BarSummaryRow {
  return { ...row, is_default: isDefault };
}

/**
 * True when the cached row was fetched BEFORE its own next_reset boundary and
 * that boundary has now passed — the row describes the previous quota window,
 * so its values (and the reset time itself) are visibly wrong in the bar.
 * The cachedAt guard means a post-reset payload that still reports a past
 * reset cannot cause a refetch loop: once re-fetched, normal TTL applies.
 */
function isCachedRowStaleByReset(state: ProviderState, now: number): boolean {
  const nextReset = state.cachedRow?.next_reset;
  if (!nextReset) return false;
  const resetMs = Date.parse(nextReset);
  if (!Number.isFinite(resetMs)) return false;
  return resetMs <= now && state.cachedAt < resetMs;
}

/**
 * Tag the row with is_default AND write the flag back onto the cached copy. The
 * collector caches a row before the default profile is known (the default is
 * resolved by the caller), so without this the cache-fallback path
 * (getCachedCodexProfileQuotaRows) would serve the default account with
 * is_default:false and the UI would stop ordering/tagging it as the default.
 */
function markDefaultAndSyncCache(
  profile: string,
  row: BarSummaryRow,
  isDefault: boolean
): BarSummaryRow {
  const state = getProfileState(profile);
  const marked = markDefault(row, isDefault);
  if (state?.cachedRow) {
    state.cachedRow = { ...state.cachedRow, is_default: isDefault };
  }
  return marked;
}

async function collectCodexRowForProfile(
  profile: string,
  deps: NativeQuotaDeps,
  force = false
): Promise<BarSummaryRow | null> {
  const now = (deps.now ?? Date.now)();
  const readNativeAuth = deps.readCodexNativeAuth ?? readCodexNativeAuthFromDisk;
  const { state, auth: nativeAuth } = validateCodexProfileState(profile, readNativeAuth);

  // Serve from cache while within TTL — force bypasses the short-circuit. Parked
  // rows (no auth -> quotaStatus 'unsupported') use a short TTL so a fresh login
  // is picked up within seconds instead of staying dimmed for the full quota TTL.
  // A row whose own next_reset has passed is stale regardless of TTL.
  if (!force && state.cachedRow) {
    const ttl = state.cachedRow.quotaStatus === 'unsupported' ? PARKED_TTL_MS : NATIVE_QUOTA_TTL_MS;
    if (now - state.cachedAt < ttl && !isCachedRowStaleByReset(state, now)) {
      return serveCached(state);
    }
  }

  // Breaker open or cooldown active -> skip network, go to LOCAL fallback.
  // Force does NOT bypass the breaker — it protects the account.
  const breakerOrCooldownActive = now < state.breakerOpenUntil || now < state.cooldownUntil;

  // Coalesce: concurrent callers past TTL share one in-flight resolution.
  if (state.pending) {
    return state.pending;
  }

  // Native profiles supply their own saved token and workspace.
  const fetchNetwork = deps.fetchCodexQuotaWithToken ?? fetchCodexQuotaWithToken;
  const getCodex = deps.getCodexQuota ?? getCodexLocalQuota;
  const sleep = deps.sleep ?? defaultSleep;

  state.pending = (async (): Promise<BarSummaryRow | null> => {
    try {
      // Yield once so the `state.pending = (...)()` assignment completes before
      // any synchronous return below runs the finally that nulls it (otherwise a
      // sync return leaves a stale resolved promise the next call would reuse).
      await Promise.resolve();
      // ----------------------------------------------------------------
      // PRIMARY: live network fetch (skipped when breaker/cooldown active)
      // ----------------------------------------------------------------
      if (!breakerOrCooldownActive) {
        // Use the on-disk accountId for the network call; fall through to local
        // when the auth file is absent (parked profile).
        if (nativeAuth) {
          const quota = await fetchNetwork(nativeAuth.accessToken, nativeAuth.accountId);
          const currentState = getProfileState(profile);
          if (currentState !== state) {
            return currentState ? serveCached(currentState) : null;
          }

          if (quota.success) {
            // A healthy response closes the breaker and clears backoff,
            // regardless of content.
            state.consecutive429 = 0;
            state.breakerOpenUntil = 0;
            state.cooldownUntil = 0;
            state.backoffAttempt = 0;
            // At least one core window (5h/weekly) resolved -> full quota row.
            if (quota.coreUsage?.fiveHour || quota.coreUsage?.weekly) {
              const row = buildCodexNetworkRow(quota, now, SURFACE_CODEX, profile);
              state.cachedRow = row;
              state.cachedAt = now;
              return { ...row, cached: false };
            }
            // Success but no core window. The token authenticated, so this is a
            // VALID active subscription with a sparse payload — emit an active
            // (quota-less) row instead of parking it. Only the bare default keeps
            // falling through to the global local session data below.
            if (profile !== DEFAULT_PROFILE) {
              const row = buildCodexNetworkRow(quota, now, SURFACE_CODEX, profile);
              state.cachedRow = row;
              state.cachedAt = now;
              return { ...row, cached: false };
            }
            // else (default): fall through to LOCAL fallback below.
          } else if (quota.needsReauth) {
            // Token expired -> dimmed reauth row. Cache it and cool down so the
            // expired account is NOT re-polled (and re-401'd) every refresh; it
            // re-checks after REAUTH_COOLDOWN_MS to pick up a re-auth.
            const reauthRow: BarSummaryRow = {
              account_id: `${SURFACE_CODEX}:${profile}`,
              provider: CODEX_NATIVE_PROVIDER,
              surface: SURFACE_CODEX,
              profile,
              is_subscription: true,
              displayName: profile,
              tier: null,
              paused: true,
              quota_percentage: null,
              quotaStatus: 'error',
              next_reset: null,
              is_default: false,
              last_activity_at: null,
              today_cost: null,
              health: 'error',
              cached: false,
              fetchedAt: new Date(now).toISOString(),
              needsReauth: true,
            };
            state.cachedRow = reauthRow;
            state.cachedAt = now;
            state.cooldownUntil = now + REAUTH_COOLDOWN_MS;
            return { ...reauthRow, cached: false };
          } else if (quota.httpStatus === 429) {
            // 429: apply breaker + backoff, then fall through to local.
            state.consecutive429 += 1;
            if (state.consecutive429 >= CB_TRIP_THRESHOLD) {
              state.breakerOpenUntil = now + CB_COOLDOWN_MS;
            }
            const retryAfter = parseRetryAfterMs(quota.errorDetail, now);
            const backoff = retryAfter ?? computeBackoffMs(state.backoffAttempt);
            state.cooldownUntil = now + backoff;
            state.backoffAttempt += 1;
            void sleep; // retained as an injectable seam for future inline retry
          } else if (quota.retryable) {
            // Other transient failure: set cooldown, fall through to local.
            const backoff = computeBackoffMs(state.backoffAttempt);
            state.cooldownUntil = now + backoff;
            state.backoffAttempt += 1;
          } else {
            // Terminal non-retryable failure (e.g. 403/404): back off so we
            // don't re-hit a dead endpoint every poll when no local data caches
            // a row to engage the TTL short-circuit. Then fall through to local.
            const backoff = computeBackoffMs(state.backoffAttempt);
            state.cooldownUntil = now + backoff;
            state.backoffAttempt += 1;
          }
          // Fall through to LOCAL fallback below.
        }
        // No on-disk auth for this profile -> fall through to the fallback below.
      }

      // ----------------------------------------------------------------
      // LOCAL FALLBACK: session-log read (zero network). The session logs live
      // in the GLOBAL ~/.codex, so they represent ONLY the bare default account,
      // never a named profile. Using them for a named profile would misattribute
      // the default's usage to the profile, so only the default falls back to
      // local; named profiles park instead.
      // ----------------------------------------------------------------
      if (profile === DEFAULT_PROFILE) {
        const localQuota = await getCodex();
        const currentState = getProfileState(profile);
        if (currentState !== state) {
          return currentState ? serveCached(currentState) : null;
        }
        if (localQuota) {
          const row = buildCodexRow(localQuota, now, SURFACE_CODEX, profile);
          state.cachedRow = row;
          state.cachedAt = now;
          return { ...row, cached: false };
        }
      }

      // Named profile (or default with no local data): serve the last-known row
      // if any, else a dimmed parked row -- never global local data for a named
      // profile.
      const stale = serveCached(state);
      if (stale) return stale;
      const parkedRow = buildParkedCodexProfileRow(profile, now);
      state.cachedRow = parkedRow;
      state.cachedAt = now;
      return parkedRow;
    } catch {
      const currentState = getProfileState(profile);
      if (currentState !== state) {
        return currentState ? serveCached(currentState) : null;
      }
      // Network/parse rejection -> treat as transient, serve stale.
      const backoff = computeBackoffMs(state.backoffAttempt);
      state.cooldownUntil = now + backoff;
      state.backoffAttempt += 1;
      return serveCached(state);
    } finally {
      state.pending = null;
    }
  })();

  return state.pending;
}

// ============================================================================
// Public entry point
// ============================================================================

/**
 * Retrieve saved Codex profiles for the dashboard without polling unrelated
 * providers or the bare ~/.codex login. Two workers bound concurrent upstream
 * requests; each profile owns its TTL, in-flight coalescing, and cooldown.
 * A caller may serve cache while this completes.
 */
export async function getCodexProfileQuotaRows(
  profiles: string[],
  deps: NativeQuotaDeps = {},
  opts?: { force?: boolean }
): Promise<BarSummaryRow[]> {
  const names = [...new Set(profiles)];
  const rows: BarSummaryRow[] = [];
  let nextIndex = 0;
  const defaultProfile = (deps.defaultCodexProfile ?? getDefaultCodexProfileFromDisk)();
  const worker = async (): Promise<void> => {
    while (nextIndex < names.length) {
      const profile = names[nextIndex++];
      const row = await collectCodexRowForProfile(profile, deps, opts?.force ?? false).catch(
        () => null
      );
      if (row) {
        rows.push(markDefaultAndSyncCache(profile, row, profile === defaultProfile));
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(2, names.length) }, () => worker()));
  return rows.sort((a, b) => (a.profile ?? '').localeCompare(b.profile ?? ''));
}

/** Only known saved-profile cache entries; never invent or persist an unread login row. */
export function getCachedCodexProfileQuotaRows(profiles: string[]): BarSummaryRow[] {
  return profiles.flatMap((profile) => {
    const state = getProfileState(profile);
    const row = state ? serveCached(state) : null;
    return row ? [row] : [];
  });
}
