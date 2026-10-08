/**
 * Keep idle saved Codex logins alive: renew each inactive saved profile before
 * its access token expires, with the Codex CLI's refresh grant, and write the
 * rotated tokens back into that same profile only.
 *
 * Refresh tokens rotate and a replayed one can revoke its whole family, so every
 * doubt skips. Per profile, under the activation lock taken without waiting
 * (activation, sign-in and removal write saved logins under it): re-read the
 * profile, re-plan it with every guard (live account, running Codex homes, other
 * logins of the same family), refresh with a 20 s timeout, check the new tokens'
 * account, and replace the file only if it still holds the bytes read under the
 * lock. Tokens never reach logs, the status file, errors or return values.
 */
import * as fs from 'fs';
import * as path from 'path';
import { createLogger } from '../services/logging';
import { replaceFileAtomically } from './codex-atomic-file';
import { tryAcquireCodexActivationLock, getActivationCodexHome } from './codex-activation-lock';
import { invalidateCodexAuthProfilesCache } from './codex-auth-dashboard-service';
import { getCodexInstancesDir } from './codex-profile-paths';
import { readAuthBytes, type CodexLoginFingerprint } from './codex-login-family';
import { checkRefreshedIdentity, intersects, mergeRefreshedTokens } from './codex-renewal-tokens';
import {
  collectCodexRenewalSnapshot,
  listRegisteredCodexProfiles,
  planCodexProfileRenewal,
  type CodexRenewalEnvironment,
  type CodexRenewalPlanEntry,
} from './codex-renewal-planner';
import {
  codexAuthFileVersion,
  nextCodexRenewalRecord,
  updateCodexRenewalRecords,
  type CodexRenewalAttemptOutcome as LockedOutcome,
  type CodexRenewalRecord,
} from './codex-renewal-status-store';
import { refreshCodexTokens, type CodexTokenFetch } from './codex-token-refresh';
import {
  CODEX_RENEWAL_MESSAGES,
  codexRenewalState,
  isCodexProfileRenewalEnabled,
  type CodexRenewalReason,
  type CodexRenewalState,
} from './codex-renewal-types';
import { getCodexProfileNameError } from './types';

export { dryRunCodexProfileRenewal } from './codex-renewal-dry-run';
export { isCodexProfileRenewalEnabled } from './codex-renewal-types';

const logger = createLogger('codex-renewal');
const DEFAULT_GAP_MS = { min: 3_000, max: 12_000 };

export type CodexRenewalLogSink = (
  level: 'info' | 'warn',
  event: string,
  message: string,
  context: Record<string, unknown>
) => void;

export interface CodexProfileRenewalOptions extends CodexRenewalEnvironment {
  /** Test seam only; the refresh URL is fixed. */
  fetch?: CodexTokenFetch;
  timeoutMs?: number;
  env?: NodeJS.ProcessEnv;
  now?: () => number;
  random?: () => number;
  sleep?: (ms: number) => Promise<void>;
  /** Pause between two profiles of one cycle. */
  gapMs?: { min: number; max: number };
  log?: CodexRenewalLogSink;
  /** Checked before each profile; a refresh in flight is never interrupted. */
  shouldContinue?: () => boolean;
  onRenewing?: (name: string | null) => void;
}

export interface CodexRenewalAttempt {
  name: string;
  state: CodexRenewalState;
  reason: CodexRenewalReason;
  message: string;
  /** True only when a refresh request was sent. */
  attempted: boolean;
  at: string;
  nextAttemptAt: string | null;
  accessExpiresAt: string | null;
}

export interface CodexRenewalCycle {
  startedAt: string;
  finishedAt: string;
  enabled: boolean;
  plans: CodexRenewalPlanEntry[];
  attempts: CodexRenewalAttempt[];
  /** Earliest backoff or busy retry, for the scheduler. */
  retryAt: string | null;
}

export interface CodexProfileRenewalProfileStatus {
  name: string;
  state: CodexRenewalState;
  reason: CodexRenewalReason;
  message: string;
  lastRenewedAt: string | null;
  lastAttemptAt: string | null;
  lastOutcome: CodexRenewalReason | null;
  nextAttemptAt: string | null;
  accessExpiresAt: string | null;
}

export interface CodexProfileRenewalStatus {
  enabled: boolean;
  checkedAt: string;
  profiles: CodexProfileRenewalProfileStatus[];
}

function iso(ms: number | null): string | null {
  return ms === null ? null : new Date(ms).toISOString();
}

async function renewUnderLock(
  name: string,
  options: CodexProfileRenewalOptions,
  codexHome: string,
  now: () => number
): Promise<LockedOutcome> {
  const names = (options.listProfiles ?? listRegisteredCodexProfiles)();
  if (!names.includes(name)) {
    return { reason: 'no_login', attempted: false, version: null, accessExpiresAt: null };
  }
  const snapshot = collectCodexRenewalSnapshot({ ...options, codexHome }, [name]);
  const profile = snapshot.profiles[0];
  const { entry } = planCodexProfileRenewal(snapshot, profile, now());
  const accessBefore = profile.fingerprint?.accessExpiresAt ?? null;
  if (entry.decision !== 'due') {
    return {
      reason: entry.reason,
      attempted: false,
      version: profile.version,
      accessExpiresAt: accessBefore,
    };
  }
  const saved = profile.parsed?.tokens as Record<string, unknown>;
  const refreshToken = saved.refresh_token as string;
  const result = await refreshCodexTokens(refreshToken, {
    fetch: options.fetch,
    timeoutMs: options.timeoutMs,
  });
  const failed = (reason: CodexRenewalReason): LockedOutcome => ({
    reason,
    attempted: true,
    version: profile.version,
    accessExpiresAt: accessBefore,
  });
  if (result.kind === 'dead') return failed('dead');
  if (result.kind === 'transient') return failed('transient');
  if (result.kind === 'invalid_response') return failed('invalid_response');
  const identity = checkRefreshedIdentity(profile, result.tokens, now());
  if (identity.reason || !identity.fingerprint)
    return failed(identity.reason ?? 'invalid_response');
  // Compare-and-swap: only the bytes read under this lock may be replaced.
  const current = readAuthBytes(profile.authPath);
  let regular = false;
  try {
    regular = fs.lstatSync(profile.authPath).isFile();
  } catch {
    regular = false;
  }
  if (
    !regular ||
    !Buffer.isBuffer(current) ||
    !profile.content ||
    !current.equals(profile.content)
  ) {
    return failed('changed');
  }
  try {
    replaceFileAtomically(
      profile.authPath,
      mergeRefreshedTokens(profile.parsed as Record<string, unknown>, result.tokens, now())
    );
  } catch {
    return failed('write_failed');
  }
  invalidateCodexAuthProfilesCache();
  const before = profile.fingerprint as CodexLoginFingerprint;
  return {
    reason: 'renewed',
    attempted: true,
    version: codexAuthFileVersion(profile.authPath),
    accessExpiresAt: identity.fingerprint.accessExpiresAt,
    familyMarkersKept:
      intersects(before.sessionIds, identity.fingerprint.sessionIds) ||
      intersects(before.authTimes, identity.fingerprint.authTimes),
  };
}

function writeLog(options: CodexProfileRenewalOptions, attempt: CodexRenewalAttempt, extra = {}) {
  const level = attempt.state === 'failed' || attempt.state === 'retrying' ? 'warn' : 'info';
  const context = {
    profile: attempt.name,
    outcome: attempt.reason,
    attempted: attempt.attempted,
    at: attempt.at,
    ...(attempt.nextAttemptAt ? { nextAttemptAt: attempt.nextAttemptAt } : {}),
    ...(attempt.accessExpiresAt ? { accessExpiresAt: attempt.accessExpiresAt } : {}),
    ...extra,
  };
  try {
    if (options.log) options.log(level, 'codex.renewal', attempt.message, context);
    else if (level === 'warn') logger.warn('codex.renewal', attempt.message, context);
    else logger.info('codex.renewal', attempt.message, context);
  } catch {
    // The outcome trail must never break renewal.
  }
}

/**
 * Renew one saved profile if it is due and every guard passes. Never waits for
 * the activation lock; never throws for provider or file outcomes.
 */
export async function renewCodexProfile(
  name: string,
  options: CodexProfileRenewalOptions = {}
): Promise<CodexRenewalAttempt> {
  const now = options.now ?? Date.now;
  const random = options.random ?? Math.random;
  const startedAt = now();
  const result = (outcome: LockedOutcome, record: CodexRenewalRecord | null) => {
    const attempt: CodexRenewalAttempt = {
      name,
      state: codexRenewalState(outcome.reason),
      reason: outcome.reason,
      message: CODEX_RENEWAL_MESSAGES[outcome.reason],
      attempted: outcome.attempted,
      at: new Date(startedAt).toISOString(),
      nextAttemptAt: record?.nextAttemptAt ?? null,
      accessExpiresAt: iso(outcome.accessExpiresAt),
    };
    writeLog(
      options,
      attempt,
      outcome.familyMarkersKept === undefined
        ? {}
        : { familyMarkersKept: outcome.familyMarkersKept }
    );
    return attempt;
  };
  const skipped = (reason: CodexRenewalReason): LockedOutcome => ({
    reason,
    attempted: false,
    version: null,
    accessExpiresAt: null,
  });
  if (!isCodexProfileRenewalEnabled(options.env)) return result(skipped('disabled'), null);
  if (getCodexProfileNameError(name)) return result(skipped('no_login'), null);

  const codexHome = path.resolve(options.codexHome ?? getActivationCodexHome());
  let release: (() => Promise<void>) | undefined;
  try {
    fs.mkdirSync(codexHome, { recursive: true, mode: 0o700 });
    release = await tryAcquireCodexActivationLock(codexHome);
  } catch {
    release = undefined;
  }
  let outcome: LockedOutcome;
  if (!release) {
    const version = codexAuthFileVersion(
      path.join(path.resolve(getCodexInstancesDir()), name, 'auth.json')
    );
    outcome = { ...skipped('activation_busy'), version };
  } else {
    try {
      outcome = await renewUnderLock(name, options, codexHome, now);
    } catch {
      // A local read failed before any request (registry, profile files): next cycle retries.
      outcome = skipped('transient');
    } finally {
      await release().catch(() => undefined);
    }
  }
  if (!outcome.attempted && outcome.reason !== 'activation_busy') return result(outcome, null);
  const written: { record: CodexRenewalRecord | null } = { record: null };
  try {
    const keep = new Set((options.listProfiles ?? listRegisteredCodexProfiles)());
    await updateCodexRenewalRecords((records) => {
      written.record = nextCodexRenewalRecord(records[name], outcome, startedAt, random);
      records[name] = written.record;
    }, keep);
  } catch {
    // History is advisory; the outcome is still logged and returned.
  }
  return result(outcome, written.record);
}

function earliestRetry(plans: CodexRenewalPlanEntry[], attempts: CodexRenewalAttempt[]) {
  const times = [
    ...plans.filter((plan) => plan.decision === 'backoff').map((plan) => plan.nextAttemptAt),
    ...attempts.map((attempt) => attempt.nextAttemptAt),
  ]
    .map((value) => (value ? Date.parse(value) : NaN))
    .filter((value) => Number.isFinite(value));
  return times.length > 0 ? new Date(Math.min(...times)).toISOString() : null;
}

/** One cycle: plan every profile, then renew the due ones sequentially. */
export async function renewDueCodexProfiles(
  options: CodexProfileRenewalOptions = {}
): Promise<CodexRenewalCycle> {
  const now = options.now ?? Date.now;
  const random = options.random ?? Math.random;
  const sleep = options.sleep ?? ((ms: number) => new Promise((done) => setTimeout(done, ms)));
  const startedAt = new Date(now()).toISOString();
  const enabled = isCodexProfileRenewalEnabled(options.env);
  if (!enabled) {
    return { startedAt, finishedAt: startedAt, enabled, plans: [], attempts: [], retryAt: null };
  }
  const snapshot = collectCodexRenewalSnapshot(options);
  const plans = snapshot.profiles.map(
    (profile) => planCodexProfileRenewal(snapshot, profile, now()).entry
  );
  const attempts: CodexRenewalAttempt[] = [];
  const gap = options.gapMs ?? DEFAULT_GAP_MS;
  // One line per profile per cycle: renewCodexProfile logs the due ones.
  for (const plan of plans) {
    if (plan.decision === 'due') continue;
    writeLog(options, {
      name: plan.name,
      state: codexRenewalState(plan.reason),
      reason: plan.reason,
      message: plan.message,
      attempted: false,
      at: startedAt,
      nextAttemptAt: plan.nextAttemptAt,
      accessExpiresAt: plan.accessExpiresAt,
    });
  }
  for (const plan of plans) {
    if (plan.decision !== 'due') continue;
    if (options.shouldContinue && !options.shouldContinue()) break;
    if (attempts.length > 0) {
      await sleep(Math.round(gap.min + (gap.max - gap.min) * random()));
      if (options.shouldContinue && !options.shouldContinue()) break;
    }
    options.onRenewing?.(plan.name);
    try {
      attempts.push(await renewCodexProfile(plan.name, options));
    } finally {
      options.onRenewing?.(null);
    }
  }
  return {
    startedAt,
    finishedAt: new Date(now()).toISOString(),
    enabled,
    plans,
    attempts,
    retryAt: earliestRetry(plans, attempts),
  };
}

/** Dashboard status per saved profile, from local files only. */
export function getCodexProfileRenewalStatus(
  options: CodexProfileRenewalOptions = {}
): CodexProfileRenewalStatus {
  const now = (options.now ?? Date.now)();
  const enabled = isCodexProfileRenewalEnabled(options.env);
  const snapshot = collectCodexRenewalSnapshot(options);
  return {
    enabled,
    checkedAt: new Date(now).toISOString(),
    profiles: snapshot.profiles.map((profile) => {
      const plan = planCodexProfileRenewal(snapshot, profile, now).entry;
      const reason: CodexRenewalReason =
        !enabled && plan.decision !== 'dead' ? 'disabled' : plan.reason;
      return {
        name: plan.name,
        state: codexRenewalState(reason),
        reason,
        message: CODEX_RENEWAL_MESSAGES[reason],
        lastRenewedAt: plan.lastRenewedAt,
        lastAttemptAt: plan.lastAttemptAt,
        lastOutcome: plan.lastOutcome,
        nextAttemptAt: enabled ? plan.nextAttemptAt : null,
        accessExpiresAt: plan.accessExpiresAt,
      };
    }),
  };
}
