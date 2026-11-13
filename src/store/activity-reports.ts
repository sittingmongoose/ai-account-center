/**
 * Analytics usage history (docs/store.md section 12): today's TypeScript collectors' rows as usage
 * reports, and the ingest queue. The TypeScript side only forwards.
 * Today's TypeScript collectors' rows become contract reports (`buildActivityReports`) that the
 * Rust store checks and decides on; the page's activity is read back from the store
 * (`storeActivityAnswer`) and shaped by the existing projection. Ingest decisions, retention,
 * day close, bucketing and coverage are all in `native/aac-store/src/activity.rs`.
 */
import { createHash } from 'crypto';
import type { AacStore } from './store-service';
import type { UsageCell, UsageReport, UsageReportState, UsageSession } from './usage-report';
import { USAGE_REPORT_SCHEMA } from './usage-report';
import type { UsageWorkerResult } from '../web-server/usage/worker-client';
import type { HourlyUsage, SessionUsage } from '../web-server/usage/types';
import type {
  AccountAnalyticsActivityProvider,
  AccountAnalyticsSource,
} from '../web-server/services/account-analytics-types';

const HOUR = 3_600_000;
const DAY = 86_400_000;
const SESSION_KEY = /^[0-9a-f]{16}$/;

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

function hourOf(text: string): number {
  return Date.parse(`${text.replace(' ', 'T')}:00Z`);
}

/** Cells for a saved result that predates them: one per hourly breakdown, tied to no session. */
function cellsFromHourly(hourly: readonly HourlyUsage[]): UsageCell[] {
  const cells: UsageCell[] = [];
  for (const hour of hourly) {
    const at = hourOf(hour.hour);
    if (!Number.isFinite(at)) continue;
    const breakdowns = Array.isArray(hour.modelBreakdowns) ? hour.modelBreakdowns : [];
    for (const b of breakdowns) {
      const tokens = b.inputTokens + b.outputTokens + b.cacheCreationTokens + b.cacheReadTokens;
      if (!(tokens > 0) && !(b.cost > 0)) continue;
      cells.push({
        hour: iso(at),
        sessionKey: null,
        route: (b.provider ?? '').toLowerCase().slice(0, 64),
        modelRaw: (b.modelName || 'unknown').slice(0, 200),
        inputTokens: Math.max(0, Math.floor(b.inputTokens)),
        outputTokens: Math.max(0, Math.floor(b.outputTokens)),
        cacheCreationTokens: Math.max(0, Math.floor(b.cacheCreationTokens)),
        cacheReadTokens: Math.max(0, Math.floor(b.cacheReadTokens)),
        costUsd: Math.max(0, b.cost),
        fallbackCostUsd: Math.max(0, b.fallbackCost ?? 0),
        events:
          typeof b.requestCount === 'number'
            ? b.requestCount
            : breakdowns.length === 1 && typeof hour.requestCount === 'number'
              ? hour.requestCount
              : null,
      });
    }
  }
  return cells;
}

function sessionsOf(rows: readonly SessionUsage[]): UsageSession[] {
  const out = new Map<string, UsageSession>();
  for (const row of rows) {
    if (!SESSION_KEY.test(row.sessionId)) continue;
    const last = Date.parse(row.lastActivity);
    const first = Date.parse(row.firstActivity ?? row.lastActivity);
    if (!Number.isFinite(last) || !Number.isFinite(first) || first > last) continue;
    const parts = (Array.isArray(row.modelBreakdowns) ? row.modelBreakdowns : [])
      .slice(0, 32)
      .map((b) => ({
        route: (b.provider ?? '').toLowerCase().slice(0, 64),
        modelRaw: (b.modelName || 'unknown').slice(0, 200),
        inputTokens: Math.max(0, Math.floor(b.inputTokens)),
        outputTokens: Math.max(0, Math.floor(b.outputTokens)),
        cacheCreationTokens: Math.max(0, Math.floor(b.cacheCreationTokens)),
        cacheReadTokens: Math.max(0, Math.floor(b.cacheReadTokens)),
        costUsd: Math.max(0, b.cost),
        fallbackCostUsd: Math.max(0, b.fallbackCost ?? 0),
      }));
    const session: UsageSession = {
      sessionKey: row.sessionId,
      first: iso(first),
      last: iso(last),
      parts,
      target:
        typeof row.target === 'string' && /^[a-z0-9-]{1,32}$/.test(row.target) ? row.target : null,
    };
    const tokens = (s: UsageSession) =>
      s.parts.reduce(
        (n, p) => n + p.inputTokens + p.outputTokens + p.cacheCreationTokens + p.cacheReadTokens,
        0
      );
    const prior = out.get(row.sessionId);
    if (!prior || tokens(session) > tokens(prior)) out.set(row.sessionId, session);
  }
  return [...out.values()];
}

/** The contract state for one source cell of the "Included usage" grid. */
function reportState(
  source: AccountAnalyticsSource,
  hasCells: boolean
): {
  state: UsageReportState;
  cause: string | null;
} {
  const detail = source.detail ?? '';
  switch (source.state) {
    case 'ok':
      return { state: hasCells ? 'ok' : 'no_usage', cause: null };
    case 'no_usage':
      return { state: hasCells ? 'ok' : 'no_usage', cause: null };
    case 'cached':
      return { state: hasCells ? 'partial' : 'cached', cause: null };
    case 'scanning':
      return { state: hasCells ? 'partial' : 'scanning', cause: null };
    case 'not_installed':
      return { state: 'not_installed', cause: null };
    case 'unavailable':
      if (/no local usage log/i.test(detail))
        return { state: 'no_reader', cause: 'no_local_usage_records' };
      if (hasCells) return { state: 'partial', cause: null };
      return {
        state: source.host === 'ubuntu' ? 'scan_failed' : 'unreadable',
        cause: /timed out/i.test(detail) ? 'timeout' : null,
      };
    default:
      return { state: 'scan_failed', cause: null };
  }
}

/**
 * One report per computer and tool of a published pass: the "Included usage" grid's states with the
 * rows each source contributed. `localComplete(tool)` says whether the local read of a tool finished
 * this pass; a remote state of `ok` is a finished read of that host.
 */
export function buildActivityReports(args: {
  local: ReadonlyMap<AccountAnalyticsActivityProvider, readonly UsageWorkerResult[]>;
  remote: ReadonlyArray<{ tool: string; data: UsageWorkerResult }>;
  states: readonly AccountAnalyticsSource[];
  /** The start of the window the readers read (31 days before the pass). */
  windowFromMs: number;
  now: number;
  localComplete: (tool: AccountAnalyticsActivityProvider) => boolean;
}): UsageReport[] {
  const reports: UsageReport[] = [];
  const coverFrom = Math.ceil(args.windowFromMs / HOUR) * HOUR;
  for (const source of args.states) {
    const tool = source.tool;
    const data =
      source.host === 'ubuntu'
        ? tool === 'cursor'
          ? []
          : [...(args.local.get(tool) ?? [])]
        : args.remote
            .filter((entry) => entry.tool === tool && entry.data.host === source.host)
            .map((entry) => entry.data);
    let derived = false;
    const cells: UsageCell[] = [];
    const sessionRows: SessionUsage[] = [];
    for (const entry of data) {
      if (Array.isArray(entry.cells)) cells.push(...entry.cells);
      else if (entry.hourly.length) {
        derived = true;
        cells.push(...cellsFromHourly(entry.hourly));
      }
      sessionRows.push(...entry.session);
    }
    const { state, cause } = reportState(source, cells.length > 0);
    const newestCell = cells.reduce((max, c) => Math.max(max, Date.parse(c.hour)), 0);
    const scanned = Date.parse(source.lastScanAt ?? '');
    const readAt = Math.max(Number.isFinite(scanned) ? scanned : args.now, newestCell);
    const complete =
      (state === 'ok' || state === 'no_usage') &&
      !derived &&
      (source.host !== 'ubuntu' ||
        (tool !== 'cursor' && args.localComplete(tool as AccountAnalyticsActivityProvider)));
    reports.push({
      schema: USAGE_REPORT_SCHEMA,
      computer: source.host,
      tool,
      readAt: iso(readAt),
      state: complete ? state : state === 'ok' ? 'partial' : state,
      complete,
      coverFrom: complete ? iso(Math.min(coverFrom, Math.floor(readAt / HOUR) * HOUR)) : null,
      cause,
      cells: state === 'ok' || state === 'partial' ? cells : [],
      sessions: sessionsOf(sessionRows),
    });
  }
  return reports;
}

/**
 * One report of rows read outside a live pass: an old cache file (`import`) or a deep read of the
 * logs (`backfill`). The store only adds and raises from these, never lowers or removes.
 */
export function rowsReport(args: {
  computer: string;
  tool: string;
  data: readonly UsageWorkerResult[];
  kind: 'import' | 'backfill';
  readAt: number;
  complete: boolean;
  cause?: string | null;
}): UsageReport {
  const cells: UsageCell[] = [];
  const sessions: SessionUsage[] = [];
  for (const entry of args.data) {
    if (Array.isArray(entry.cells)) cells.push(...entry.cells);
    else cells.push(...cellsFromHourly(entry.hourly));
    sessions.push(...entry.session);
  }
  const newest = cells.reduce((max, c) => Math.max(max, Date.parse(c.hour)), 0);
  const readAt = Math.max(args.readAt, newest);
  const oldest = cells.reduce((min, c) => Math.min(min, Date.parse(c.hour)), readAt);
  const complete = args.complete && args.kind === 'backfill';
  return {
    schema: USAGE_REPORT_SCHEMA,
    computer: args.computer,
    tool: args.tool,
    readAt: iso(readAt),
    state: cells.length ? (complete ? 'ok' : 'partial') : complete ? 'no_usage' : 'scanning',
    complete,
    coverFrom: complete ? iso(Math.floor(oldest / HOUR) * HOUR) : null,
    cause: args.cause ?? null,
    cells,
    sessions: sessionsOf(sessions),
    kind: args.kind,
  };
}

function digest(value: unknown): string {
  return createHash('sha1').update(JSON.stringify(value)).digest('hex');
}

/**
 * Hands reports to the store, one call at a time, leaving out each source's days and sessions it
 * already sent unchanged (the store treats those days as untouched). A failed call forgets what was
 * sent, so the next pass sends everything again.
 */
export class ActivityIngestor {
  private readonly days = new Map<string, Map<number, string>>();
  private readonly sessions = new Map<string, Map<string, string>>();
  private running: Promise<void> | null = null;
  private queued: UsageReport[] | null = null;

  constructor(
    private readonly store: () => AacStore | null | Promise<AacStore | null>,
    private readonly onError: (error: unknown) => void = () => undefined
  ) {}

  /** Queues a pass's reports; a pass that arrives while one is being written replaces any waiting one. */
  submit(reports: UsageReport[]): Promise<void> {
    this.queued = reports;
    if (!this.running)
      this.running = (async () => {
        while (this.queued) {
          const next = this.queued;
          this.queued = null;
          try {
            await this.write(next);
          } catch (error) {
            this.onError(error);
          }
        }
      })().finally(() => {
        this.running = null;
      });
    return this.running;
  }

  /** Waits for the queue to drain (tests, shutdown). */
  async idle(): Promise<void> {
    while (this.running) await this.running;
  }

  async write(reports: UsageReport[]): Promise<void> {
    const store = await this.store();
    if (!store || reports.length === 0) return;
    const sent: Array<[string, Map<number, string>, Map<string, string>]> = [];
    const trimmed = reports.map((report) => {
      const key = `${report.computer}\0${report.tool}\0${report.kind ?? 'live'}`;
      const byDay = new Map<number, UsageCell[]>();
      for (const cell of report.cells) {
        const day = Math.floor(Date.parse(cell.hour) / DAY);
        const list = byDay.get(day) ?? [];
        list.push(cell);
        byDay.set(day, list);
      }
      const dayDigests = new Map<number, string>();
      for (const [day, list] of byDay) dayDigests.set(day, digest(list));
      const before = this.days.get(key);
      const same = [...dayDigests].filter(([day, d]) => before?.get(day) === d).map(([day]) => day);
      const sameSet = new Set(same);
      const sessionDigests = new Map(report.sessions.map((s) => [s.sessionKey, digest(s)]));
      const beforeSessions = this.sessions.get(key);
      sent.push([key, dayDigests, sessionDigests]);
      return {
        ...report,
        cells: report.cells.filter((cell) => !sameSet.has(Math.floor(Date.parse(cell.hour) / DAY))),
        sessions: report.sessions.filter(
          (s) => beforeSessions?.get(s.sessionKey) !== sessionDigests.get(s.sessionKey)
        ),
        ...(same.length ? { sameDays: same } : {}),
      };
    });
    try {
      const result = await store.activityIngest(trimmed);
      const refused = new Set(result.refused.map((r) => r.index));
      sent.forEach(([key, days, sessions], index) => {
        if (refused.has(index)) {
          this.days.delete(key);
          this.sessions.delete(key);
        } else {
          this.days.set(key, days);
          this.sessions.set(key, sessions);
        }
      });
      if (refused.size)
        this.onError(
          new Error(`store refused ${refused.size} usage report(s): ${result.refused[0]?.code}`)
        );
    } catch (error) {
      for (const [key] of sent) {
        this.days.delete(key);
        this.sessions.delete(key);
      }
      throw error;
    }
  }
}
