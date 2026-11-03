import * as fs from 'fs';
import * as path from 'path';
import { readSnapshotSettings } from '../agent-usage/agent-usage-settings';

/**
 * Claude Code sign-in renewal, as published by the renewal runner (scripts/claude-signin).
 *
 * The runner lives on the computer with the browser and the person (the Mac): Claude's Keychain items
 * there are readable only from the desktop session, and claude.ai's CAPTCHA needs a person at a
 * window. After every run it writes `claude-signins.json` into the usage snapshot folder. This module
 * reads that file, keeps only allowlisted fields and marks it stale when the runner stopped writing.
 * The file holds times, account emails, profile names and fixed state words; anything shaped like a
 * link or a token is refused, so the dashboard can never echo one.
 */

export const RENEWAL_FILE = 'claude-signins.json';
const MAX_BYTES = 512 * 1024;
const CACHE_MS = 30 * 1000;
const NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;
const EMAIL = /^[^\s@/\\]{1,64}@[^\s@/\\]{1,190}$/;
const WORD = /^[a-z][a-z0-9_]{0,39}$/;
const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z$/;
const SECRETISH = /https?:|[A-Za-z0-9_-]{20,}#|sk-ant|[A-Za-z0-9_-]{40,}/;

export const RENEWAL_STATES = [
  'ok',
  'logged_out',
  'no_refresh_token',
  'no_end_date',
  'unreadable',
  'not_signed_in',
] as const;
export type RenewalState = (typeof RENEWAL_STATES)[number];

export interface ClaudeSignInRenewalRow {
  host: string;
  profile: string;
  email: string | null;
  state: RenewalState;
  signInEndsAt: string | null;
  logoutAt: string | null;
  /** When the runner starts renewing this sign-in by itself. */
  renewFrom: string | null;
  autoRenew: boolean;
  lastRenewAt: string | null;
  /** A fixed word: renewed, waiting_for_you, end_not_moved, login_failed, ... */
  lastResult: string | null;
  nextVerifyAt: string | null;
  retryAfter: string | null;
  /** How often an older Claude process wrote its old sign-in back over a renewal. */
  reverts: number;
  readAt: string | null;
}

export interface ClaudeSignInNeed {
  email: string;
  /** captcha, mail_link_missing, wrong_account, browser_signin_failed, renewal_failing */
  reason: string;
  /** One plain sentence for the person: what to do and where. */
  how: string;
  since: string | null;
}

export interface ClaudeSignInRenewal {
  runner: string;
  runnerVersion: string;
  generatedAt: string;
  /** The runner has not written for longer than it promised: renewals may not be happening. */
  stale: boolean;
  renewWithinHours: number;
  needsYou: ClaudeSignInNeed[];
  hosts: Array<{ host: string; readAt: string | null; error: string | null }>;
  signIns: ClaudeSignInRenewalRow[];
}

const str = (value: unknown, pattern: RegExp): string | null =>
  typeof value === 'string' && pattern.test(value) ? value : null;
const time = (value: unknown): string | null => str(value, ISO);
const plain = (value: unknown, max: number): string | null =>
  typeof value === 'string' &&
  value.length > 0 &&
  value.length <= max &&
  !SECRETISH.test(value) &&
  !/[\u0000-\u001f\u007f]/.test(value)
    ? value
    : null;

function row(raw: unknown): ClaudeSignInRenewalRow | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const host = str(r.host, NAME);
  const profile = str(r.profile, NAME);
  const state = RENEWAL_STATES.find((s) => s === r.state);
  if (!host || !profile || !state) return null;
  return {
    host,
    profile,
    email: typeof r.email === 'string' ? str(r.email.toLowerCase(), EMAIL) : null,
    state,
    signInEndsAt: time(r.signInEndsAt),
    logoutAt: time(r.logoutAt),
    renewFrom: time(r.renewFrom),
    autoRenew: r.autoRenew !== false,
    lastRenewAt: time(r.lastRenewAt),
    lastResult: str(r.lastResult, WORD),
    nextVerifyAt: time(r.nextVerifyAt),
    retryAfter: time(r.retryAfter),
    reverts:
      typeof r.reverts === 'number' && Number.isInteger(r.reverts) && r.reverts >= 0
        ? Math.min(r.reverts, 999)
        : 0,
    readAt: time(r.readAt),
  };
}

function need(raw: unknown): ClaudeSignInNeed | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const email = typeof r.email === 'string' ? str(r.email.toLowerCase(), EMAIL) : null;
  const reason = str(r.reason, WORD);
  if (!email || !reason) return null;
  return { email, reason, how: plain(r.how, 400) ?? '', since: time(r.since) };
}

/** Parses one published file; null when it is not a renewal file this version understands. */
export function parseClaudeSignInRenewal(
  text: string,
  now = Date.now()
): ClaudeSignInRenewal | null {
  let doc: Record<string, unknown>;
  try {
    const value = JSON.parse(text);
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
    doc = value as Record<string, unknown>;
  } catch {
    return null;
  }
  const generatedAt = time(doc.generatedAt);
  if (doc.version !== 1 || !generatedAt) return null;
  const staleAfter =
    typeof doc.staleAfterSeconds === 'number' && doc.staleAfterSeconds >= 600
      ? Math.min(doc.staleAfterSeconds, 86400)
      : 3600;
  const list = (value: unknown, limit: number): unknown[] =>
    Array.isArray(value) ? value.slice(0, limit) : [];
  return {
    runner: str(doc.runner, NAME) ?? 'unknown',
    runnerVersion: str(doc.runnerVersion, /^\d{1,3}\.\d{1,3}\.\d{1,3}$/) ?? 'unknown',
    generatedAt,
    stale: now - Date.parse(generatedAt) > staleAfter * 1000,
    renewWithinHours:
      typeof doc.renewWithinHours === 'number' &&
      doc.renewWithinHours > 0 &&
      doc.renewWithinHours <= 720
        ? doc.renewWithinHours
        : 96,
    needsYou: list(doc.needsYou, 32)
      .map(need)
      .filter((n): n is ClaudeSignInNeed => n !== null),
    hosts: list(doc.hosts, 16)
      .map((h) => {
        const r = (typeof h === 'object' && h !== null ? h : {}) as Record<string, unknown>;
        const host = str(r.host, NAME);
        return host ? { host, readAt: time(r.readAt), error: plain(r.error, 160) } : null;
      })
      .filter((h): h is { host: string; readAt: string | null; error: string | null } => !!h),
    signIns: list(doc.signIns, 128)
      .map(row)
      .filter((r): r is ClaudeSignInRenewalRow => r !== null),
  };
}

export interface RenewalReaderDeps {
  folder?: () => Promise<string | null>;
  readFile?: (file: string) => Promise<string>;
  now?: () => number;
}

let cache: { at: number; value: Promise<ClaudeSignInRenewal | null> } | null = null;

/** The runner's latest published state, or null when no runner publishes into the snapshot folder. */
export function readClaudeSignInRenewal(
  deps: RenewalReaderDeps = {}
): Promise<ClaudeSignInRenewal | null> {
  const now = (deps.now ?? Date.now)();
  const injected = deps.folder || deps.readFile;
  if (!injected && cache && now - cache.at < CACHE_MS) return cache.value;
  const value = (async () => {
    const folder = deps.folder
      ? await deps.folder()
      : ((await readSnapshotSettings().catch(() => null))?.directory ?? null);
    if (!folder) return null;
    const file = path.join(folder, RENEWAL_FILE);
    try {
      const text = deps.readFile
        ? await deps.readFile(file)
        : await (async () => {
            const stat = await fs.promises.stat(file);
            if (!stat.isFile() || stat.size > MAX_BYTES) return '';
            return fs.promises.readFile(file, 'utf8');
          })();
      return parseClaudeSignInRenewal(text, now);
    } catch {
      return null;
    }
  })();
  if (!injected) cache = { at: now, value };
  return value;
}

export function invalidateClaudeSignInRenewal(): void {
  cache = null;
}
