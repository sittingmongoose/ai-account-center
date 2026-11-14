/**
 * The Cursor account through AAC's own API key (DESIGN 17 in the share folder
 * 2026-11-13-analytics-discovery). The work is the Rust reader `aac-cursor-account`
 * (native/aac-cursor-account), packaged per target in dist/native/aac-cursor-account and run on
 * this computer only, digest-checked before every run. This module is the transport: it runs the
 * reader, keeps its last answer (`cache/cursor-account-v1.json`: usage numbers and an account hash,
 * never the key), reads the account at most every 10 minutes (a manual refresh reads at once), and
 * asks the reader which of the account's usage no local run accounts for.
 *
 * The key itself is a `credential` in AAC's key store (`account-usage/keys/cursor-<id>.json`, 0600;
 * DPAPI on Windows), put there by `ai-account-center cursor-key set` from stdin. Nothing here sees
 * it: the reader loads it, and prints only its last four characters and fingerprint.
 */
import { execFile } from 'child_process';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import { ProviderError } from '../../errors/error-types';
import { getCcsDir } from '../../utils/config-manager';
import type { DashboardAccount } from './account-dashboard-types';
import type { AnalyticsRemoteRow } from './analytics-remote-transport';
import { localUsageScanTarget, type UsageScanTarget } from './usage-scan-binary';

export const CURSOR_ACCOUNT_NAME = 'aac-cursor-account';
/** The account is read at most this often; a manual refresh reads at once. */
export const CURSOR_ACCOUNT_TTL_MS = 10 * 60_000;
const READ_TIMEOUT_MS = 120_000;
const MAX_OUTPUT = 32 * 1024 * 1024;

export type CursorAccountState = 'no_key' | 'key_rejected' | 'read' | 'unavailable' | 'no_reader';

/** One hour and model of the account's usage, as the reader prints it. */
export interface CursorAccountRow {
  h: string;
  m: string;
  i: number;
  o: number;
  cr: number;
  cw: number;
  n: number;
  /** Cursor's own cost, in cents, of the events that had one. */
  cents: number;
  /** Events Cursor gave no cost for. */
  uncosted: number;
}

/** The last answer, kept across restarts. */
export interface CursorAccountRead {
  state: CursorAccountState;
  /** When this answer was read (any state). */
  checkedAt: string;
  /** When the account's usage was last read in full; the rows are from then. */
  readAt: string | null;
  cause?: string;
  /** 16 hex of the account (never its id). */
  account?: string | null;
  key?: { last4: string; fingerprint: string } | null;
  rows: CursorAccountRow[];
  events: number;
  /** In-range events whose `tokenUsage` had no usable counts: not logged, never measured zeros. */
  unmeasured: number;
}

/** What Analytics shows for the account (DESIGN 17.5) beside the totals it never joins. */
export interface CursorAccountAnswer {
  state: CursorAccountState;
  checkedAt: string | null;
  readAt: string | null;
  cause?: string;
  last4?: string;
  accountTokens: number;
  matchedTokens: number;
  outsideTokens: number;
  localUnmatchedTokens: number;
  /**
   * Present when some of the account's events carried no Cursor cost: any cost figure for the
   * account is partial, and no list price fills the gap.
   */
  costPartial?: boolean;
  /** In-range events whose token counts were not logged: the tokens are measured ones only. */
  unmeasured: number;
  /** The account's own hourly rows, in the remote rows' shape; never added to any total. */
  rows: AnalyticsRemoteRow[];
}

export interface CursorAccountReaderDeps {
  /** Runs the reader with arguments and stdin; tests pass their own. */
  run?: (args: string[], input: string) => Promise<unknown>;
  cacheDir?: string;
  ccsDir?: string;
  now?: () => number;
}

/** The packaged reader for this computer, digest-checked; null when none was built for it. */
export function cursorAccountBinary(
  dir = path.resolve(__dirname, '../../native/aac-cursor-account'),
  target: UsageScanTarget | null = localUsageScanTarget()
): string | null {
  try {
    const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8')) as {
      name?: unknown;
      targets?: Record<string, { file?: unknown; sha256?: unknown; size?: unknown }>;
    };
    const entry = target ? manifest.targets?.[target] : undefined;
    if (
      manifest.name !== CURSOR_ACCOUNT_NAME ||
      !entry ||
      typeof entry.file !== 'string' ||
      entry.file.includes('..') ||
      typeof entry.sha256 !== 'string' ||
      !/^[0-9a-f]{64}$/.test(entry.sha256)
    )
      return null;
    const file = path.join(dir, entry.file);
    const digest = createHash('sha256').update(fs.readFileSync(file)).digest('hex');
    return digest === entry.sha256 ? file : null;
  } catch {
    return null;
  }
}

/** Run the packaged reader; its one JSON line, parsed. Throws when there is no usable reader. */
export function runCursorAccount(args: string[], input: string): Promise<unknown> {
  const file = cursorAccountBinary();
  if (!file) return Promise.reject(new Error('no_reader'));
  return new Promise((resolve, reject) => {
    const child = execFile(
      file,
      args,
      { encoding: 'utf8', timeout: READ_TIMEOUT_MS, maxBuffer: MAX_OUTPUT, windowsHide: true },
      (error, stdout) => {
        try {
          const line = String(stdout).trim().split('\n').pop() ?? '';
          const parsed = JSON.parse(line) as unknown;
          // `key put` exits 1 with a fixed error object: still an answer.
          if (error && !(parsed && typeof parsed === 'object')) reject(new Error('reader_failed'));
          else resolve(parsed);
        } catch {
          reject(new Error(error ? 'reader_failed' : 'bad_output'));
        }
      }
    );
    child.stdin?.on('error', () => {
      /* The exit is handled above. */
    });
    child.stdin?.end(input);
  });
}

const STATES: ReadonlySet<string> = new Set([
  'no_key',
  'key_rejected',
  'read',
  'unavailable',
  'no_reader',
]);
const HOUR = /^\d{4}-\d{2}-\d{2} \d{2}:00$/;
const count = (value: unknown) =>
  typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;

/** The reader's rows, each checked; a malformed one is dropped. */
export function parseCursorAccountRows(value: unknown): CursorAccountRow[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter(
      (row): row is CursorAccountRow =>
        !!row &&
        typeof row === 'object' &&
        typeof row.h === 'string' &&
        HOUR.test(row.h) &&
        typeof row.m === 'string' &&
        row.m.length > 0 &&
        row.m.length <= 200 &&
        !/[\u0000-\u001f\u007f]/.test(row.m) &&
        ['i', 'o', 'cr', 'cw', 'n', 'uncosted'].every((key) => count(row[key])) &&
        typeof row.cents === 'number' &&
        Number.isFinite(row.cents) &&
        row.cents >= 0
    )
    .slice(0, 100_000);
}

function cacheFile(cacheDir: string): string {
  return path.join(cacheDir, 'cursor-account-v1.json');
}

/** The last saved read, or null (what the page serves while a new read is still running). */
export function loadCursorAccountRead(
  cacheDir: string = path.join(getCcsDir(), 'cache')
): CursorAccountRead | null {
  return loadRead(cacheDir);
}

function loadRead(cacheDir: string): CursorAccountRead | null {
  try {
    const file = cacheFile(cacheDir);
    if (fs.statSync(file).size > MAX_OUTPUT) return null;
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as Partial<CursorAccountRead>;
    if (!value || !STATES.has(String(value.state)) || typeof value.checkedAt !== 'string')
      return null;
    return {
      state: value.state as CursorAccountState,
      checkedAt: value.checkedAt,
      readAt: typeof value.readAt === 'string' ? value.readAt : null,
      ...(typeof value.cause === 'string' ? { cause: value.cause } : {}),
      account: typeof value.account === 'string' ? value.account : null,
      key:
        value.key &&
        typeof value.key.last4 === 'string' &&
        typeof value.key.fingerprint === 'string'
          ? { last4: value.key.last4, fingerprint: value.key.fingerprint }
          : null,
      rows: parseCursorAccountRows(value.rows),
      events: count(value.events) ? (value.events as number) : 0,
      unmeasured: count(value.unmeasured) ? (value.unmeasured as number) : 0,
    };
  } catch {
    return null;
  }
}

function saveRead(cacheDir: string, value: CursorAccountRead): void {
  try {
    fs.mkdirSync(cacheDir, { recursive: true, mode: 0o700 });
    const file = cacheFile(cacheDir);
    const temporary = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(temporary, JSON.stringify(value), { mode: 0o600 });
    fs.renameSync(temporary, file);
  } catch {
    /* The read is still served; the next one saves again. */
  }
}

/**
 * The account's usage over [since, until], read at most every 10 minutes (`force`: at once). A
 * failed read keeps the last good rows, named with the failure and the time they are from.
 */
export async function readCursorAccount(
  sinceMs: number,
  untilMs: number,
  options: { force?: boolean } & CursorAccountReaderDeps = {}
): Promise<CursorAccountRead> {
  const now = options.now ?? Date.now;
  const cacheDir = options.cacheDir ?? path.join(getCcsDir(), 'cache');
  const ccsDir = options.ccsDir ?? getCcsDir();
  const run = options.run ?? runCursorAccount;
  const previous = loadRead(cacheDir);
  if (
    previous &&
    !options.force &&
    now() - Date.parse(previous.checkedAt) < CURSOR_ACCOUNT_TTL_MS &&
    now() >= Date.parse(previous.checkedAt)
  )
    return previous;
  const checkedAt = new Date(now()).toISOString();
  let answer: Record<string, unknown>;
  try {
    answer = (await run(
      ['read', '--ccs-dir', ccsDir],
      JSON.stringify({ sinceMs: Math.floor(sinceMs), untilMs: Math.floor(untilMs) })
    )) as Record<string, unknown>;
  } catch (error) {
    answer = {
      state: error instanceof Error && error.message === 'no_reader' ? 'no_reader' : 'unavailable',
      cause: 'reader_failed',
    };
  }
  const state = STATES.has(String(answer?.state))
    ? (answer.state as CursorAccountState)
    : 'unavailable';
  const key = answer?.key as { last4?: unknown; fingerprint?: unknown } | undefined;
  const keyInfo =
    key && typeof key.last4 === 'string' && typeof key.fingerprint === 'string'
      ? { last4: key.last4.slice(-4), fingerprint: key.fingerprint }
      : (previous?.key ?? null);
  let read: CursorAccountRead;
  if (state === 'read') {
    read = {
      state,
      checkedAt,
      readAt: typeof answer.readAt === 'number' ? new Date(answer.readAt).toISOString() : checkedAt,
      account: typeof answer.account === 'string' ? answer.account : null,
      key: keyInfo,
      rows: parseCursorAccountRows(answer.rows),
      events: count(answer.events) ? (answer.events as number) : 0,
      unmeasured: count(answer.unmeasured) ? (answer.unmeasured as number) : 0,
    };
  } else {
    // No key, a rejected key or no reader: nothing of the account counts. A failed read keeps the
    // last good rows, with their own time, never presented as current.
    const keep = state === 'unavailable' && previous?.readAt ? previous : null;
    read = {
      state,
      checkedAt,
      readAt: keep?.readAt ?? null,
      ...(state === 'unavailable'
        ? {
            cause:
              typeof answer.cause === 'string' && /^[a-z_]{1,32}$/.test(answer.cause)
                ? answer.cause
                : 'reader_failed',
          }
        : {}),
      account: keep?.account ?? null,
      key: state === 'no_key' ? null : keyInfo,
      rows: keep?.rows ?? [],
      events: keep?.events ?? 0,
      unmeasured: keep?.unmeasured ?? 0,
    };
  }
  saveRead(cacheDir, read);
  return read;
}

/**
 * The account's usage beside the local runs (DESIGN 17.4). The account feed is the authoritative
 * account total; the local runs are an overlapping breakdown of it, never added to it, so no
 * part of the answer is added to Analytics' totals. `local` are the scanned computers' Cursor
 * hourly rows; the reader's summary reports the account total and the overlap estimate.
 */
export async function cursorAccountOutside(
  read: CursorAccountRead,
  local: Array<{ h: string; m: string; i: number; o: number; cr: number; cw: number }>,
  deps: Pick<CursorAccountReaderDeps, 'run'> = {}
): Promise<CursorAccountAnswer> {
  const base = {
    state: read.state,
    checkedAt: read.checkedAt,
    readAt: read.readAt,
    ...(read.cause ? { cause: read.cause } : {}),
    ...(read.key?.last4 ? { last4: read.key.last4 } : {}),
    unmeasured: read.unmeasured,
  };
  const empty = {
    ...base,
    accountTokens: 0,
    matchedTokens: 0,
    outsideTokens: 0,
    localUnmatchedTokens: 0,
    rows: [],
  };
  if (!read.rows.length) return empty;
  let answer: { rows?: unknown; summary?: Record<string, unknown> };
  try {
    answer = (await (deps.run ?? runCursorAccount)(
      ['dedupe'],
      JSON.stringify({ account: read.rows, local })
    )) as typeof answer;
  } catch {
    // Without the dedupe the account's numbers cannot be trusted either: only the states remain.
    return { ...empty, state: 'unavailable', cause: 'dedupe_failed' };
  }
  const summary = answer?.summary ?? {};
  const total = (key: string) => (count(summary[key]) ? (summary[key] as number) : 0);
  const rows: AnalyticsRemoteRow[] = parseCursorAccountRows(answer?.rows).map((row) => ({
    k: 'cursor',
    f: 'cursor-account',
    m: row.m,
    h: row.h,
    i: row.i,
    o: row.o,
    cr: row.cr,
    cw: row.cw,
    // Cursor's own cost where every event in the hour had one, kept whole even at 0. An hour
    // where any event had none is a row of unknown cost: never priced, marked partial.
    c: row.uncosted === 0 ? row.cents / 100 : 0,
    n: row.n,
    costBasis: row.uncosted === 0 ? ('reported' as const) : ('unknown' as const),
  }));
  return {
    ...base,
    accountTokens: total('accountTokens'),
    matchedTokens: total('matchedTokens'),
    outsideTokens: total('outsideTokens'),
    localUnmatchedTokens: total('localUnmatchedTokens'),
    ...(rows.some((row) => row.costBasis === 'unknown') ? { costPartial: true } : {}),
    rows,
  };
}

/** Quota transport only: the Rust reader owns credentials and Cursor's protocol. */
export class CursorQuotaReader {
  private pending: Promise<DashboardAccount | null> | null = null;
  private current: DashboardAccount | null = null;

  constructor(private readonly deps: CursorAccountReaderDeps = {}) {}

  peek(base: DashboardAccount) {
    return this.current ? { ...base, ...this.current, id: base.id, label: base.label } : null;
  }

  read(base: DashboardAccount, force = false) {
    this.pending ??= this.collect(base, force).finally(() => {
      this.pending = null;
    });
    return this.pending.then((row) => (row ? { ...row, id: base.id, label: base.label } : null));
  }

  private async collect(base: DashboardAccount, force: boolean) {
    const ccsDir = this.deps.ccsDir ?? getCcsDir();
    const file = path.join(
      this.deps.cacheDir ?? path.join(ccsDir, 'cache'),
      'cursor-quota-v1.json'
    );
    const run = this.deps.run ?? runCursorAccount;
    const now = (this.deps.now ?? Date.now)();
    const at = new Date(now).toISOString();
    let fingerprint: string | null = null;
    let previous: {
      version: number;
      fingerprint: string;
      checkedAt: string;
      account: DashboardAccount;
    } | null = null;
    let answer: Record<string, unknown> = {};
    try {
      // Only the reader opens the store. A missing/unsafe reader is not proof of no key.
      const status = (await run(['key', 'status', '--ccs-dir', ccsDir], '')) as Record<
        string,
        unknown
      >;
      if (status.present === false && !status.error) {
        this.current = null;
        try {
          fs.unlinkSync(file);
        } catch {
          /* No saved quota. */
        }
        return null;
      }
      if (status.present !== true || typeof status.fingerprint !== 'string')
        throw new ProviderError('Cursor key status is unavailable.', 'cursor');
      fingerprint = status.fingerprint;
      try {
        if (fs.statSync(file).size <= 64 * 1024)
          previous = JSON.parse(fs.readFileSync(file, 'utf8'));
      } catch {
        /* No saved quota. */
      }
      if (
        previous?.version !== 1 ||
        previous.fingerprint !== fingerprint ||
        !previous.account ||
        !Array.isArray(previous.account.windows) ||
        !['ok', 'cached', 'needs_sign_in', 'unavailable'].includes(previous.account.status) ||
        !Number.isFinite(Date.parse(previous.checkedAt)) ||
        previous.account.windows.some(
          (window) =>
            !window ||
            typeof window.key !== 'string' ||
            typeof window.label !== 'string' ||
            typeof window.usedPercent !== 'number' ||
            !Number.isFinite(window.usedPercent) ||
            window.usedPercent < 0 ||
            (window.resetAt !== null && !Number.isFinite(Date.parse(window.resetAt)))
        ) ||
        (previous.account.windows.length > 0 &&
          !Number.isFinite(Date.parse(previous.account.sampledAt ?? '')))
      )
        previous = null;
      if (
        previous &&
        !force &&
        now >= Date.parse(previous.checkedAt) &&
        now - Date.parse(previous.checkedAt) < CURSOR_ACCOUNT_TTL_MS
      ) {
        this.current = previous.account;
        return this.peek(base);
      }
      answer = (await run(['quota', '--ccs-dir', ccsDir], '')) as Record<string, unknown>;
    } catch {
      answer = { state: 'unavailable' };
    }
    const quota = answer?.quota as Record<string, unknown> | undefined;
    const plan = quota?.planUsage as Record<string, unknown> | undefined;
    const percentage = (value: unknown): value is number =>
      typeof value === 'number' && Number.isFinite(value) && value >= 0;
    const resetMs = quota?.billingCycleEndMs;
    const resetAt =
      typeof resetMs === 'number' && Number.isFinite(resetMs) && Math.abs(resetMs) <= 8.64e15
        ? new Date(resetMs).toISOString()
        : null;
    const valid =
      answer?.state === 'read' &&
      percentage(plan?.totalPercentUsed) &&
      percentage(plan?.autoPercentUsed);
    const sampledAt =
      typeof answer?.readAt === 'number' &&
      Number.isFinite(answer.readAt) &&
      Math.abs(answer.readAt) <= 8.64e15
        ? new Date(answer.readAt).toISOString()
        : at;
    const windows = valid
      ? [
          ['plan-reported', 'Included usage', plan?.totalPercentUsed],
          ['autoPercentUsed', 'Cursor models', plan?.autoPercentUsed],
        ].map(([key, label, value]) => ({
          key: key as string,
          label: label as string,
          usedPercent: value as number,
          remainingPercent: Math.max(0, 100 - (value as number)),
          resetAt,
          windowMinutes: null,
          used: null,
          limit: null,
          unit: null,
        }))
      : [];
    const keep =
      !valid &&
      answer?.state !== 'key_rejected' &&
      previous?.account.sampledAt &&
      previous.account.windows.length
        ? previous.account
        : null;
    this.current = {
      ...base,
      email: null,
      plan: null,
      host: undefined,
      platform:
        process.platform === 'darwin' ? 'mac' : process.platform === 'win32' ? 'windows' : 'ubuntu',
      source: 'Cursor API key',
      fetchedAt: at,
      sampledAt: valid ? sampledAt : (keep?.sampledAt ?? null),
      status: valid
        ? 'ok'
        : answer?.state === 'key_rejected'
          ? 'needs_sign_in'
          : keep
            ? 'cached'
            : 'unavailable',
      message: valid
        ? null
        : answer?.state === 'key_rejected'
          ? 'The key was rejected.'
          : keep
            ? `Cursor read failed; showing the cached reading from ${keep.sampledAt}.`
            : 'Cursor quota could not be read.',
      windows: valid ? windows : (keep?.windows ?? []),
    };
    if (fingerprint) {
      try {
        fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
        const temporary = `${file}.${process.pid}.tmp`;
        fs.writeFileSync(
          temporary,
          JSON.stringify({ version: 1, fingerprint, checkedAt: at, account: this.current }),
          { mode: 0o600 }
        );
        fs.renameSync(temporary, file);
      } catch {
        /* Serve the answer even if persistence is unavailable. */
      }
    }
    return this.current;
  }
}
