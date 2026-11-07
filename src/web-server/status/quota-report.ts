/**
 * quota-watch's share copy (`<reports>/v1/quota/report.json`, schema `quota-report@1`): per-stream
 * estimates of how much of each account meter a stream used, budgets, what could not be split, and
 * the machines a pass could not scan. One writer (quota-watch) rewrites it whole every 10 minutes.
 *
 * Read with the agent-reports rules: list the folder, then read; ENOENT and ESTALE keep the last
 * good copy and retry; NUL bytes, a file over 512 KB, a secret or a schema miss make it unreadable
 * by name (the last good copy stays, marked); a lower `seq` than one already accepted is ignored.
 * Only the sections the Status tab shows are projected; every value is an estimate unless the
 * report says otherwise, and an unknown stays null.
 */
import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';
import { isTransientShareError, MISSING_GRACE_MS } from './agent-reports-reader';
import {
  count,
  fail,
  findSecretIn,
  Invalid,
  isWriterName,
  iso,
  list,
  obj,
  oneOf,
  optIso,
  optText,
  text,
} from './schema-helpers';
import { readRegularFile } from './share-read';

export const QUOTA_REPORT_SCHEMA = 'quota-report@1';
export const QUOTA_REPORT_CAP = 512 * 1024;
export const QUOTA_PERIOD_IDS = ['day', 'week', 'window'] as const;
export type QuotaPeriodId = (typeof QUOTA_PERIOD_IDS)[number];
/** Percent means percentage points of a usage window; never added to credits. */
export type QuotaUnit = 'credits' | 'percent';

export interface QuotaRow {
  stream: string;
  account: string;
  meter: string;
  unit: QuotaUnit;
  value: number | null;
  estimate: boolean;
}

export interface QuotaPeriod {
  period: string | null;
  start: string | null;
  end: string | null;
  coverage: number | null;
  rows: QuotaRow[];
}

export interface QuotaBudget {
  stream: string;
  desc: string | null;
  accounts: string[];
  unit: QuotaUnit;
  meter: string;
  per: string | null;
  limit: number | null;
  use: number | null;
  /** false: part of the period has no reading, so `use` is a lower bound. */
  known: boolean;
  percent: number | null;
  period: string | null;
  coverage: number | null;
  estimate: boolean;
}

export interface QuotaUnattributed {
  account: string;
  meter: string;
  unit: QuotaUnit | null;
  amount: number | null;
  reason: string;
  why: string | null;
  estimate: boolean;
}

export interface QuotaMachineNotScanned {
  name: string;
  thisPass: boolean | null;
  error: string | null;
  runsMissed: number | null;
  runsInWindow: number | null;
}

export interface QuotaReportDoc {
  writer: string;
  seq: number;
  writtenAt: string;
  generatedAt: string | null;
  methodId: string | null;
  method: string | null;
  periods: Record<QuotaPeriodId, QuotaPeriod | null>;
  budgets: QuotaBudget[];
  unattributed: QuotaUnattributed[];
  machinesNotScanned: QuotaMachineNotScanned[];
}

function num(value: unknown, field: string): number | null {
  if (value === null || value === undefined) return null;
  if (typeof value !== 'number' || !Number.isFinite(value)) fail(field);
  return value;
}

function share(value: unknown, field: string): number | null {
  const n = num(value, field);
  if (n !== null && (n < 0 || n > 1)) fail(field);
  return n;
}

function unit(value: unknown, field: string): QuotaUnit {
  return oneOf(value, field, ['credits', 'percent'] as const);
}

/** `estimate: false` only when the report says so (a measured reading); otherwise an estimate. */
function estimate(value: unknown): boolean {
  return value !== false;
}

function period(value: unknown, field: string): QuotaPeriod | null {
  if (value === null || value === undefined) return null;
  const p = obj(value, field);
  return {
    period: optText(p.period, `${field}.period`, 80),
    start: optIso(p.start, `${field}.start`),
    end: optIso(p.end, `${field}.end`),
    coverage: share(p.coverage, `${field}.coverage`),
    rows: list(p.rows, `${field}.rows`, 2000, (row, at) => {
      const r = obj(row, at);
      return {
        stream: text(r.stream, `${at}.stream`, 120),
        account: text(r.account, `${at}.account`, 120),
        meter: text(r.meter, `${at}.meter`, 80),
        unit: unit(r.unit, `${at}.unit`),
        value: num(r.value, `${at}.value`),
        estimate: estimate(r.estimate),
      };
    }),
  };
}

function project(raw: unknown): QuotaReportDoc {
  const doc = obj(raw, 'document');
  if (doc.schema !== QUOTA_REPORT_SCHEMA) fail('schema');
  if (!isWriterName(doc.writer)) fail('writer');
  if (doc.estimate !== true) fail('estimate');
  const streamEstimates =
    doc.streamEstimates === null || doc.streamEstimates === undefined
      ? {}
      : obj(doc.streamEstimates, 'streamEstimates');
  return {
    writer: doc.writer,
    seq: count(doc.seq, 'seq'),
    writtenAt: iso(doc.writtenAt, 'writtenAt'),
    generatedAt: optIso(doc.generatedAt, 'generatedAt'),
    methodId: optText(doc.methodId, 'methodId', 80),
    method: optText(doc.method, 'method', 2000),
    periods: {
      day: period(streamEstimates.day, 'streamEstimates.day'),
      week: period(streamEstimates.week, 'streamEstimates.week'),
      window: period(streamEstimates.window, 'streamEstimates.window'),
    },
    budgets: list(doc.budgetRows, 'budgetRows', 500, (row, at) => {
      const r = obj(row, at);
      if (typeof r.known !== 'boolean') fail(`${at}.known`);
      return {
        stream: text(r.stream, `${at}.stream`, 120),
        desc: optText(r.desc, `${at}.desc`, 200),
        accounts: list(r.accounts, `${at}.accounts`, 50, (a, w) => text(a, w, 120)),
        unit: unit(r.unit, `${at}.unit`),
        meter: text(r.meter, `${at}.meter`, 80),
        per: optText(r.per, `${at}.per`, 40),
        limit: num(r.limit, `${at}.limit`),
        use: num(r.use, `${at}.use`),
        known: r.known,
        percent: num(r.percent, `${at}.percent`),
        period: optText(r.period, `${at}.period`, 80),
        coverage: share(r.coverage, `${at}.coverage`),
        estimate: estimate(r.estimate),
      };
    }),
    unattributed: list(doc.unattributedRows, 'unattributedRows', 500, (row, at) => {
      const r = obj(row, at);
      return {
        account: text(r.account, `${at}.account`, 120),
        meter: text(r.meter, `${at}.meter`, 80),
        unit: r.unit === null || r.unit === undefined ? null : unit(r.unit, `${at}.unit`),
        amount: num(r.amount, `${at}.amount`),
        reason: text(r.reason, `${at}.reason`, 120),
        why: optText(r.why, `${at}.why`, 500),
        estimate: estimate(r.estimate),
      };
    }),
    machinesNotScanned: list(doc.machinesNotScanned, 'machinesNotScanned', 20, (row, at) => {
      const r = obj(row, at);
      return {
        name: text(r.name, `${at}.name`, 80),
        thisPass: typeof r.thisPass === 'boolean' ? r.thisPass : null,
        error: optText(r.error, `${at}.error`, 500),
        runsMissed:
          r.runsMissed === null || r.runsMissed === undefined
            ? null
            : count(r.runsMissed, `${at}.runsMissed`),
        runsInWindow:
          r.runsInWindow === null || r.runsInWindow === undefined
            ? null
            : count(r.runsInWindow, `${at}.runsInWindow`),
      };
    }),
  };
}

export type QuotaParse =
  | { ok: true; doc: QuotaReportDoc; sha256: string }
  | { ok: false; reason: string };

/** Bytes of report.json -> the projected report, or a fixed reason. */
export function parseQuotaReport(bytes: Buffer): QuotaParse {
  if (bytes.length > QUOTA_REPORT_CAP) return { ok: false, reason: 'larger than the 512 KB cap' };
  if (bytes.length === 0) return { ok: false, reason: 'empty' };
  if (bytes.includes(0)) return { ok: false, reason: 'contains NUL bytes from a damaged write' };
  let source: string;
  try {
    source = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    return { ok: false, reason: 'not UTF-8 text' };
  }
  let raw: unknown;
  try {
    raw = JSON.parse(source);
  } catch {
    return { ok: false, reason: 'not valid JSON' };
  }
  const secret = findSecretIn(source, raw);
  if (secret) return { ok: false, reason: `refused: it contains what looks like ${secret}` };
  try {
    return {
      ok: true,
      doc: project(raw),
      sha256: createHash('sha256').update(bytes).digest('hex'),
    };
  } catch (error) {
    if (error instanceof Invalid) return { ok: false, reason: error.message };
    return { ok: false, reason: 'could not be read' };
  }
}

export interface QuotaReportState {
  /** missing: no report (or folder); error: the folder could not be listed. */
  state: 'ok' | 'missing' | 'error';
  scannedAt: number | null;
  /** The last good copy and when AAC first saw that version. */
  good: { doc: QuotaReportDoc; sha256: string; firstSeenAt: number; writtenAtMs: number } | null;
  /** The file's current problem: the copy above is the last good one. */
  problem: { reason: string; since: number } | null;
}

export interface QuotaReaderFs {
  readdir(dir: string): Promise<string[]>;
  stat(file: string): Promise<fs.Stats>;
  readFile(file: string): Promise<Buffer>;
}

const nodeFs: QuotaReaderFs = {
  readdir: (dir) => fs.promises.readdir(dir),
  stat: (file) => fs.promises.stat(file),
  readFile: (file) => readRegularFile(file, QUOTA_REPORT_CAP),
};

export class QuotaReportReader {
  private good: QuotaReportState['good'] = null;
  private problem: QuotaReportState['problem'] = null;
  private state: QuotaReportState['state'] = 'missing';
  private scannedAt: number | null = null;
  private statKey = '';
  private missingSince: number | null = null;

  constructor(
    private readonly options: { root: () => string; now?: () => number; fs?: QuotaReaderFs }
  ) {}

  private now(): number {
    return (this.options.now ?? Date.now)();
  }

  private get fs(): QuotaReaderFs {
    return this.options.fs ?? nodeFs;
  }

  /** One rescan; resolves whether anything the view shows changed. */
  async scan(): Promise<boolean> {
    const folder = path.join(this.options.root(), 'v1', 'quota');
    const file = path.join(folder, 'report.json');
    const before = JSON.stringify([this.state, this.good?.sha256, this.problem?.reason]);
    const done = () => {
      this.scannedAt = this.now();
      return JSON.stringify([this.state, this.good?.sha256, this.problem?.reason]) !== before;
    };
    let names: string[];
    try {
      names = await this.fs.readdir(folder);
    } catch (error) {
      if ((error as NodeJS.ErrnoException)?.code === 'ENOENT') return this.absent(done);
      // A stale handle: keep everything and try again next scan.
      if (isTransientShareError(error)) return done();
      this.state = 'error';
      return done();
    }
    if (!names.includes('report.json')) return this.absent(done);
    this.missingSince = null;
    this.state = 'ok';
    let stat: fs.Stats;
    try {
      stat = await this.fs.stat(file);
    } catch (error) {
      if (!isTransientShareError(error)) this.setProblem('could not be read');
      return done();
    }
    if (!stat.isFile()) {
      this.statKey = '';
      this.setProblem('is not a regular file');
      return done();
    }
    const statKey = `${stat.size}:${stat.mtimeMs}:${stat.ino}:${stat.ctimeMs}`;
    if (statKey === this.statKey) return done();
    if (stat.size > QUOTA_REPORT_CAP) {
      this.statKey = statKey;
      this.setProblem('larger than the 512 KB cap');
      return done();
    }
    let bytes: Buffer;
    try {
      bytes = await this.fs.readFile(file);
    } catch (error) {
      if (!isTransientShareError(error)) this.setProblem('could not be read');
      return done();
    }
    this.statKey = statKey;
    const parsed = parseQuotaReport(bytes);
    if (!parsed.ok) {
      this.setProblem(parsed.reason);
      return done();
    }
    this.problem = null;
    // An older write landing late: ignored.
    if (this.good && parsed.doc.seq < this.good.doc.seq) return done();
    if (this.good?.sha256 !== parsed.sha256)
      this.good = {
        doc: parsed.doc,
        sha256: parsed.sha256,
        firstSeenAt: this.now(),
        writtenAtMs: Date.parse(parsed.doc.writtenAt),
      };
    return done();
  }

  /** No report listed: keep the last good copy through the rename gap, then report it missing. */
  private absent(done: () => boolean): boolean {
    const now = this.now();
    this.missingSince ??= now;
    if (!this.good || now - this.missingSince >= MISSING_GRACE_MS) {
      this.good = null;
      this.problem = null;
      this.statKey = '';
      this.state = 'missing';
    }
    return done();
  }

  private setProblem(reason: string): void {
    if (this.problem?.reason !== reason) this.problem = { reason, since: this.now() };
  }

  snapshot(): QuotaReportState {
    return { state: this.state, scannedAt: this.scannedAt, good: this.good, problem: this.problem };
  }
}
