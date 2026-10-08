/**
 * Saved-login renewal planner. `collectCodexRenewalSnapshot` reads local files
 * and running-process homes (no network, no writes); `planCodexProfileRenewal`
 * is pure. Snapshots hold credentials in memory only; plan entries carry safe
 * fields (names, codes, timestamps) and guard details carry paths and labels.
 *
 * Rules, in order: no login / unsafe file; no account binding; the live login's
 * account (every saved copy, not only the one the dashboard names) or an
 * unreadable live login; a recorded rejection of this file version; no family
 * markers; process scan failure; a Codex process using the profile folder; any
 * other login that shares or may share the family; recorded backoff; then due
 * (4 days or less of access left, or already expired) or fresh.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as yaml from 'js-yaml';
import { ConfigError } from '../errors/error-types';
import { getCcsDir } from '../utils/config-manager';
import { getActivationCodexHome } from './codex-activation-lock';
import type { CodexProcessSnapshot } from './codex-activation-runtime';
import { getCodexAuthRegistryPath, getCodexInstancesDir } from './codex-profile-paths';
import { validateCodexProfileRegistryData } from './codex-profile-registry';
import {
  compareLoginFamily,
  findHomeCodexAuthFiles,
  findProfileAuthFiles,
  fingerprintCodexLogin,
  parseAuthJson,
  readCliproxyCodexSources,
  readCodexLoginSource,
  sameAccountOrUnknown,
  scanCodexProcessHomes,
  type CodexFamilyMatch,
  type CodexLoginFingerprint,
  type CodexLoginSource,
  type CodexLoginSourceKind,
  type CodexProcessScan,
} from './codex-login-family';
import {
  CODEX_RENEWAL_DEAD_REASONS,
  CODEX_RENEWAL_MESSAGES,
  CODEX_RENEWAL_RETRY_REASONS,
  type CodexRenewalReason,
} from './codex-renewal-types';
import {
  codexAuthFileVersion,
  readCodexRenewalRecords,
  type CodexRenewalRecord,
  type CodexRenewalRecords,
} from './codex-renewal-status-store';

const DAY_MS = 24 * 60 * 60 * 1000;
/** Renew once 4 days or less of the 10-day access token remain. */
export const CODEX_RENEWAL_LEAD_MS = 4 * DAY_MS;
/** Without a decodable expiry: 6 days after the last refresh. */
export const CODEX_RENEWAL_FALLBACK_AGE_MS = 6 * DAY_MS;
/** Upper bound for a manual early renewal lead (the access token lasts 10 days). */
export const CODEX_RENEWAL_MAX_LEAD_MS = 10 * DAY_MS;
const MAX_PROFILE_AUTH_BYTES = 1024 * 1024;

/** Where to look. Tests inject every path; production uses the real homes. */
export interface CodexRenewalEnvironment {
  /** The shared native login (`~/.codex`). */
  codexHome?: string;
  /** Scanned for `.codex*` homes and their child homes. */
  homeDir?: string;
  extraCodexHomes?: string[];
  /** Null means the process scan is unavailable. */
  scanProcesses?: () => CodexProcessSnapshot[] | null;
  listProfiles?: () => string[];
  /**
   * Renew once this much access time or less remains (default 4 days). A manual
   * "renew now" passes a longer lead; it is clamped so it never exceeds the 10-day
   * token, and every guard still applies.
   */
  renewWithinMs?: number;
}

/** Private: holds the saved credentials in memory. */
export interface CodexRenewalProfileSnapshot {
  name: string;
  profileDir: string;
  authPath: string;
  realPath: string | null;
  realDir: string | null;
  state: 'ok' | 'missing' | 'unsafe';
  content: Buffer | null;
  parsed: Record<string, unknown> | null;
  fingerprint: CodexLoginFingerprint | null;
  version: string | null;
}

/** Private: holds credentials in memory. */
export interface CodexRenewalSnapshot {
  codexHome: string;
  homeDir: string;
  live: CodexLoginSource | null;
  profiles: CodexRenewalProfileSnapshot[];
  sources: CodexLoginSource[];
  processScan: CodexProcessScan;
  records: CodexRenewalRecords;
  renewWithinMs: number;
}

export type CodexRenewalDecision = 'fresh' | 'due' | 'skip' | 'dead' | 'backoff';

export interface CodexRenewalPlanEntry {
  name: string;
  decision: CodexRenewalDecision;
  reason: CodexRenewalReason;
  message: string;
  accessExpiresAt: string | null;
  dueAt: string | null;
  lastRenewedAt: string | null;
  lastAttemptAt: string | null;
  lastOutcome: CodexRenewalReason | null;
  nextAttemptAt: string | null;
}

export interface CodexRenewalGuards {
  live: boolean;
  inUse: boolean;
  sharedWith: { path: string; kind: CodexLoginSourceKind; matchedBy: CodexFamilyMatch[] }[];
  uncheckable: { path: string; kind: CodexLoginSourceKind }[];
}

/** Registered names, read without the registry class (whose constructor sweeps temp files). */
export function listRegisteredCodexProfiles(): string[] {
  let raw: string;
  try {
    raw = fs.readFileSync(getCodexAuthRegistryPath(), 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return [];
    throw new ConfigError('Codex profile registry could not be read safely.');
  }
  try {
    return Object.keys(validateCodexProfileRegistryData(yaml.load(raw)).profiles);
  } catch {
    throw new ConfigError('Codex profile registry could not be read safely.');
  }
}

function realPathOf(file: string): string | null {
  try {
    return fs.realpathSync(file);
  } catch {
    return null;
  }
}

/** The saved login, refusing links, non-regular and oversized files. */
export function readCodexRenewalProfile(name: string): CodexRenewalProfileSnapshot {
  const profileDir = path.join(path.resolve(getCodexInstancesDir()), name);
  const authPath = path.join(profileDir, 'auth.json');
  const base = {
    name,
    profileDir,
    authPath,
    realPath: realPathOf(authPath),
    realDir: realPathOf(profileDir),
    content: null,
    parsed: null,
    fingerprint: null,
    version: codexAuthFileVersion(authPath),
  };
  let content: Buffer;
  try {
    const stat = fs.lstatSync(authPath);
    if (!stat.isFile() || stat.size > MAX_PROFILE_AUTH_BYTES) return { ...base, state: 'unsafe' };
    content = fs.readFileSync(authPath);
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    return { ...base, state: code === 'ENOENT' || code === 'ENOTDIR' ? 'missing' : 'unsafe' };
  }
  const parsed = parseAuthJson(content);
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    return { ...base, state: 'unsafe' };
  }
  const fingerprint = fingerprintCodexLogin(parsed);
  const tokens = (parsed as { tokens?: unknown }).tokens;
  if (
    !fingerprint?.refreshDigest ||
    !tokens ||
    typeof tokens !== 'object' ||
    Array.isArray(tokens) ||
    typeof (tokens as { refresh_token?: unknown }).refresh_token !== 'string'
  ) {
    return { ...base, state: 'missing' };
  }
  return {
    ...base,
    state: 'ok',
    content,
    parsed: parsed as Record<string, unknown>,
    fingerprint,
  };
}

function collectSources(
  env: CodexRenewalEnvironment,
  codexHome: string,
  homeDir: string,
  processScan: CodexProcessScan
): { live: CodexLoginSource | null; sources: CodexLoginSource[] } {
  const seen = new Set<string>();
  const sources: CodexLoginSource[] = [];
  const add = (file: string, kind: CodexLoginSourceKind): CodexLoginSource | null => {
    const resolved = path.resolve(file);
    if (seen.has(resolved)) return null;
    seen.add(resolved);
    const source = readCodexLoginSource(resolved, kind);
    if (source) sources.push(source);
    return source;
  };
  const live = add(path.join(codexHome, 'auth.json'), 'live');
  for (const file of findProfileAuthFiles(path.resolve(getCodexInstancesDir()))) {
    add(file, 'profile');
  }
  for (const home of env.extraCodexHomes ?? []) add(path.join(home, 'auth.json'), 'extra');
  for (const file of findHomeCodexAuthFiles(homeDir)) add(file, 'codex_home');
  for (const { home } of processScan.homes) add(path.join(home, 'auth.json'), 'process_home');
  for (const source of readCliproxyCodexSources(getCcsDir())) {
    const resolved = path.resolve(source.path);
    if (seen.has(resolved)) continue;
    seen.add(resolved);
    sources.push(source);
  }
  return { live, sources };
}

/** Read-only. `names` limits the profile snapshots; every saved login is still a source. */
export function collectCodexRenewalSnapshot(
  env: CodexRenewalEnvironment = {},
  names?: string[]
): CodexRenewalSnapshot {
  const codexHome = path.resolve(env.codexHome ?? getActivationCodexHome());
  const homeDir = path.resolve(env.homeDir ?? os.homedir());
  const processScan = scanCodexProcessHomes(env.scanProcesses);
  const { live, sources } = collectSources(env, codexHome, homeDir, processScan);
  const profileNames = names ?? (env.listProfiles ?? listRegisteredCodexProfiles)();
  return {
    codexHome,
    homeDir,
    live,
    profiles: profileNames.map(readCodexRenewalProfile),
    sources,
    processScan,
    records: readCodexRenewalRecords(),
    renewWithinMs: renewalLead(env.renewWithinMs),
  };
}

function renewalLead(value: number | undefined): number {
  if (value === undefined || !Number.isFinite(value)) return CODEX_RENEWAL_LEAD_MS;
  return Math.min(Math.max(value, CODEX_RENEWAL_LEAD_MS), CODEX_RENEWAL_MAX_LEAD_MS);
}

export function evaluateCodexRenewalGuards(
  snapshot: CodexRenewalSnapshot,
  profile: CodexRenewalProfileSnapshot
): CodexRenewalGuards {
  const guards: CodexRenewalGuards = { live: false, inUse: false, sharedWith: [], uncheckable: [] };
  const fingerprint = profile.fingerprint;
  const live = snapshot.live?.state === 'ok' ? snapshot.live.fingerprint : null;
  if (fingerprint && live) guards.live = sameAccountOrUnknown(fingerprint, live);
  guards.inUse = snapshot.processScan.homes.some(
    ({ home, realHome }) =>
      home === profile.profileDir || (realHome !== null && realHome === profile.realDir)
  );
  if (!fingerprint) return guards;
  for (const source of snapshot.sources) {
    if (source.path === profile.authPath) continue;
    const result = compareLoginFamily({ fingerprint, realPath: profile.realPath }, source);
    if (result === 'unverifiable')
      guards.uncheckable.push({ path: source.path, kind: source.kind });
    else if (result) {
      guards.sharedWith.push({ path: source.path, kind: source.kind, matchedBy: result });
    }
  }
  return guards;
}

function iso(ms: number | null | undefined): string | null {
  return typeof ms === 'number' && Number.isFinite(ms) ? new Date(ms).toISOString() : null;
}

function currentRecord(
  snapshot: CodexRenewalSnapshot,
  profile: CodexRenewalProfileSnapshot
): CodexRenewalRecord | null {
  const record = snapshot.records[profile.name];
  return record && profile.version !== null && record.authVersion === profile.version
    ? record
    : null;
}

/** Pure decision for one profile from a snapshot. */
export function planCodexProfileRenewal(
  snapshot: CodexRenewalSnapshot,
  profile: CodexRenewalProfileSnapshot,
  now: number
): { entry: CodexRenewalPlanEntry; guards: CodexRenewalGuards } {
  const guards = evaluateCodexRenewalGuards(snapshot, profile);
  const history = snapshot.records[profile.name];
  const record = currentRecord(snapshot, profile);
  const fingerprint = profile.fingerprint;
  const accessExpiresAt = fingerprint?.accessExpiresAt ?? null;
  const lastRefreshAt = fingerprint?.lastRefreshAt ?? null;
  const dueAt =
    accessExpiresAt !== null
      ? accessExpiresAt - snapshot.renewWithinMs
      : lastRefreshAt !== null
        ? lastRefreshAt +
          CODEX_RENEWAL_FALLBACK_AGE_MS -
          (snapshot.renewWithinMs - CODEX_RENEWAL_LEAD_MS)
        : fingerprint
          ? now
          : null;
  const entry = (
    decision: CodexRenewalDecision,
    reason: CodexRenewalReason,
    nextAttemptAt: string | null = null
  ): { entry: CodexRenewalPlanEntry; guards: CodexRenewalGuards } => ({
    entry: {
      name: profile.name,
      decision,
      reason,
      message: CODEX_RENEWAL_MESSAGES[reason],
      accessExpiresAt: iso(accessExpiresAt),
      dueAt: iso(dueAt),
      lastRenewedAt: history?.lastSuccessAt ?? null,
      lastAttemptAt: history?.lastAttemptAt ?? null,
      lastOutcome: history?.outcome ?? null,
      nextAttemptAt,
    },
    guards,
  });

  if (profile.state === 'missing') return entry('skip', 'no_login');
  if (profile.state === 'unsafe' || !fingerprint) return entry('skip', 'unsafe_file');
  if (!fingerprint.binding) return entry('skip', 'unverifiable');
  if (snapshot.live?.state === 'unreadable') return entry('skip', 'live_unverifiable');
  if (snapshot.live?.state === 'ok' && !snapshot.live.fingerprint?.binding) {
    // A live token login without a readable account could be any saved profile.
    if (!snapshot.live.fingerprint?.email) return entry('skip', 'live_unverifiable');
  }
  if (guards.live) return entry('skip', 'live');
  if (record && CODEX_RENEWAL_DEAD_REASONS.has(record.outcome)) {
    return entry('dead', record.outcome);
  }
  if (fingerprint.sessionIds.size === 0 && fingerprint.authTimes.size === 0) {
    return entry('skip', 'unverifiable');
  }
  if (snapshot.processScan.state === 'failed') return entry('skip', 'process_scan_failed');
  if (guards.inUse) return entry('skip', 'in_use');
  if (guards.sharedWith.length > 0) return entry('skip', 'shared_family');
  if (guards.uncheckable.length > 0) return entry('skip', 'family_unverifiable');
  if (
    record &&
    CODEX_RENEWAL_RETRY_REASONS.has(record.outcome) &&
    record.nextAttemptAt &&
    Date.parse(record.nextAttemptAt) > now
  ) {
    return entry('backoff', record.outcome, record.nextAttemptAt);
  }
  if (dueAt !== null && now >= dueAt) return entry('due', 'due');
  return entry('fresh', 'fresh', iso(dueAt));
}

export function planCodexProfileRenewals(
  snapshot: CodexRenewalSnapshot,
  now: number
): CodexRenewalPlanEntry[] {
  return snapshot.profiles.map((profile) => planCodexProfileRenewal(snapshot, profile, now).entry);
}
