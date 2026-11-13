/**
 * Analytics usage history (docs/store.md section 12), the TypeScript side: it only forwards.
 * Today's TypeScript collectors' rows become contract reports (`buildActivityReports`) that the
 * Rust store checks and decides on; the page's activity is read back from the store
 * (`storeActivityAnswer`) and shaped by the existing projection. Ingest decisions, retention,
 * day close, bucketing and coverage are all in `native/aac-store/src/activity.rs`.
 */
import { createHash } from 'crypto';
import type { AacStore } from './store-service';
import type {
  ActivityRange,
  ActivitySources,
  UsageCell,
  UsageReport,
  UsageReportState,
  UsageSession,
} from './usage-report';
import { USAGE_REPORT_SCHEMA } from './usage-report';
import type { UsageWorkerResult } from '../web-server/usage/worker-client';
import type { HourlyUsage, ModelBreakdown, SessionUsage } from '../web-server/usage/types';
import type {
  AccountAnalyticsActivity,
  AccountAnalyticsActivityProvider,
  AccountAnalyticsHistoryCoverage,
  AccountAnalyticsQuery,
  AccountAnalyticsSource,
  AccountAnalyticsUsageProvider,
} from '../web-server/services/account-analytics-types';
import {
  projectAccountAnalyticsActivity,
  type SourceData,
} from '../web-server/services/account-analytics-projection';
import { usageProviderFor } from '../web-server/services/account-analytics-attribution';
import type { AccountAnalyticsPricingLookup } from '../web-server/services/account-analytics-pricing';
import { getModelPricingWithSource } from '../web-server/model-pricing';
import {
  addCalendarDays,
  localDate,
  localMidnight,
} from '../web-server/services/account-analytics-range';
import type { DashboardProvider } from '../web-server/services/account-dashboard-types';

const HOUR = 3_600_000;
const DAY = 86_400_000;
/** Ranges up to this long are answered hourly; longer ones in local days. */
export const HOURLY_RANGE_MS = 92 * DAY;
const SESSION_KEY = /^[0-9a-f]{16}$/;

const TOOLS: readonly AccountAnalyticsActivityProvider[] = [
  'claude',
  'codex',
  'omp',
  'muse',
  'zcode',
  'jsonl',
  'antigravity',
];

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

// ----------------------------------------------------------------------------- reading back

/** The local midnights from the day of `from` through the one after `to`, as UTC instants. */
export function localDayStarts(from: number, to: number, tz: string): number[] {
  const starts: number[] = [];
  let date = localDate(from, tz);
  const last = localDate(to, tz);
  for (let guard = 0; guard < 20_000; guard++) {
    starts.push(localMidnight(date, tz));
    if (date > last) break;
    date = addCalendarDays(date, 1);
  }
  return starts;
}

function hourText(ms: number): string {
  return new Date(ms).toISOString().slice(0, 13).replace('T', ' ') + ':00';
}

/** A cost the store has no estimate for (a reader that logs only its own costs): priced here. */
function listed(
  route: string,
  model: string,
  tokens: [number, number, number, number]
): { cost: number; fallback: number } {
  const resolved = getModelPricingWithSource(model, { provider: route || undefined });
  const r = resolved.pricing;
  const cost =
    (tokens[0] / 1e6) * r.inputPerMillion +
    (tokens[1] / 1e6) * r.outputPerMillion +
    (tokens[2] / 1e6) * r.cacheCreationPerMillion +
    (tokens[3] / 1e6) * r.cacheReadPerMillion;
  return { cost, fallback: resolved.source === 'fallback' ? cost : 0 };
}

/** The store's bucketed rows as the per-tool sources the projection reads (one row per bucket). */
export function rangeToSources(
  range: ActivityRange,
  stamp: (bucket: number) => number,
  fetchedAt: string
): SourceData[] {
  const byTool = new Map<string, Map<number, HourlyUsage>>();
  const knownCounts = new Map<string, boolean>();
  for (const row of range.rows) {
    const [bucket, , ti, ri, mi, i, o, cw, cr, loggedNano, costNano, fallbackNano, events] = row;
    const tool = range.tools[ti];
    const route = range.routes[ri] ?? '';
    const model = range.models[mi] ?? 'unknown';
    let cost: number;
    let fallback: number;
    if (costNano === null) {
      // the reader logged only its own cost: the rest at list price, priced now
      const priced = listed(route, model, [i, o, cw, cr]);
      const logged = (loggedNano ?? 0) / 1e9;
      cost = logged > 0 ? logged : priced.cost;
      fallback = logged > 0 ? 0 : priced.fallback;
    } else {
      cost = costNano / 1e9;
      fallback = (fallbackNano ?? 0) / 1e9;
    }
    const at = stamp(bucket);
    const hours = byTool.get(tool) ?? new Map<number, HourlyUsage>();
    byTool.set(tool, hours);
    const hour = hours.get(at) ?? {
      hour: hourText(at),
      source: `${tool}-store`,
      inputTokens: 0,
      outputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      cost: 0,
      totalCost: 0,
      modelsUsed: [],
      modelBreakdowns: [],
      requestCount: 0,
    };
    hours.set(at, hour);
    const breakdown: ModelBreakdown = {
      modelName: model,
      ...(route ? { provider: route } : {}),
      inputTokens: i,
      outputTokens: o,
      cacheCreationTokens: cw,
      cacheReadTokens: cr,
      cost,
      ...(fallback > 0 ? { fallbackCost: fallback } : {}),
      ...(events !== null ? { requestCount: events } : {}),
    };
    // one model may come in several rows (one per computer): fold it
    const prior = hour.modelBreakdowns.find(
      (b) => b.modelName === model && (b.provider ?? '') === route
    );
    if (prior) {
      prior.inputTokens += i;
      prior.outputTokens += o;
      prior.cacheCreationTokens += cw;
      prior.cacheReadTokens += cr;
      prior.cost += cost;
      if (fallback > 0) prior.fallbackCost = (prior.fallbackCost ?? 0) + fallback;
      if (events === null || prior.requestCount === undefined) delete prior.requestCount;
      else prior.requestCount += events;
    } else hour.modelBreakdowns.push(breakdown);
    if (!hour.modelsUsed.includes(model)) hour.modelsUsed.push(model);
    hour.inputTokens += i;
    hour.outputTokens += o;
    hour.cacheCreationTokens += cw;
    hour.cacheReadTokens += cr;
    hour.cost += cost;
    hour.totalCost += cost;
    if (fallback > 0) hour.fallbackCost = (hour.fallbackCost ?? 0) + fallback;
    const countKey = `${tool}\0${at}`;
    if (events === null) knownCounts.set(countKey, false);
    else if (knownCounts.get(countKey) !== false) {
      knownCounts.set(countKey, true);
      hour.requestCount = (hour.requestCount ?? 0) + events;
    }
  }
  const sessionsByTool = new Map<string, SessionUsage[]>();
  for (const s of range.sessions.sample) {
    if (!s.sessionKey) continue;
    const parts = s.parts.map((p) => ({
      modelName: p.modelRaw,
      ...(p.route ? { provider: p.route } : {}),
      inputTokens: p.inputTokens,
      outputTokens: p.outputTokens,
      cacheCreationTokens: p.cacheCreationTokens,
      cacheReadTokens: p.cacheReadTokens,
      cost: p.costUsd ?? p.loggedCostUsd ?? 0,
      ...(p.fallbackCostUsd ? { fallbackCost: p.fallbackCostUsd } : {}),
    }));
    const sum = (
      f: 'inputTokens' | 'outputTokens' | 'cacheCreationTokens' | 'cacheReadTokens' | 'cost'
    ) => parts.reduce((n, p) => n + p[f], 0);
    const list = sessionsByTool.get(s.tool) ?? [];
    list.push({
      sessionId: s.sessionKey,
      projectPath: '',
      source: `${s.tool}-store`,
      inputTokens: sum('inputTokens'),
      outputTokens: sum('outputTokens'),
      cacheCreationTokens: sum('cacheCreationTokens'),
      cacheReadTokens: sum('cacheReadTokens'),
      cost: sum('cost'),
      totalCost: sum('cost'),
      firstActivity: iso(s.firstMs),
      lastActivity: iso(s.lastMs),
      versions: [],
      modelsUsed: [...new Set(parts.map((p) => p.modelName))],
      modelBreakdowns: parts,
      ...(s.target ? { target: s.target } : {}),
    });
    sessionsByTool.set(s.tool, list);
  }
  const sources: SourceData[] = [];
  for (const tool of TOOLS) {
    const hours = byTool.get(tool);
    const sessions = sessionsByTool.get(tool) ?? [];
    if (!hours && !sessions.length) continue;
    const hourly = [...(hours?.values() ?? [])];
    for (const hour of hourly)
      if (knownCounts.get(`${tool}\0${hourOf(hour.hour)}`) === false) delete hour.requestCount;
    sources.push({
      provider: tool,
      data: [
        {
          daily: [],
          monthly: [],
          hourly,
          session: sessions,
          eventCount: hourly.reduce((n, h) => n + (h.requestCount ?? 0), 0),
        },
      ],
      fetchedAt,
    });
  }
  return sources;
}

/** Per-provider session counts from the store's signatures (a session counts under each provider that served it). */
export function sessionCounts(range: ActivityRange): Map<AccountAnalyticsUsageProvider, number> {
  const counts = new Map<AccountAnalyticsUsageProvider, number>();
  for (const sig of range.sessions.signatures) {
    const tool = sig.tool as AccountAnalyticsActivityProvider;
    const providers = new Set(
      sig.pairs.map(([route, model]) => usageProviderFor(tool, route, model))
    );
    if (!providers.size) providers.add(usageProviderFor(tool, null, null));
    for (const p of providers) counts.set(p, (counts.get(p) ?? 0) + sig.count);
  }
  return counts;
}

export type StoreActivityCoverage = AccountAnalyticsHistoryCoverage;

/** The "Included usage" grid from the store's last reads, for an answer before this run's first pass. */
export function gridFromStore(history: StoreActivityCoverage): AccountAnalyticsSource[] {
  const tools = ['claude', 'codex', 'omp', 'muse', 'zcode', 'jsonl', 'antigravity', 'cursor'];
  const grid: AccountAnalyticsSource[] = [];
  for (const s of history.sources) {
    if (!tools.includes(s.tool)) continue;
    const state: AccountAnalyticsSource['state'] =
      s.state === 'ok'
        ? 'ok'
        : s.state === 'no_usage'
          ? 'no_usage'
          : s.state === 'partial' || s.state === 'cached'
            ? 'cached'
            : s.state === 'scanning'
              ? 'scanning'
              : s.state === 'not_installed' || s.state === 'installed_no_logs'
                ? 'not_installed'
                : 'unavailable';
    grid.push({
      tool: s.tool as AccountAnalyticsSource['tool'],
      host: s.host as AccountAnalyticsSource['host'],
      state,
      lastScanAt: s.lastReadAt,
      rowCount: 0,
      detail:
        state === 'unavailable'
          ? s.state === 'no_reader'
            ? 'no local usage log'
            : (s.cause ?? 'the last read failed')
          : null,
    });
  }
  return grid;
}

export function coverageOf(sources: ActivitySources): StoreActivityCoverage {
  const at = (ms: number | null | undefined) => (typeof ms === 'number' ? iso(ms) : null);
  const backfill = sources.sources
    .filter((s) => s.backfill)
    .map((s) => ({
      host: s.computer,
      tool: s.tool,
      state: s.backfill?.state ?? 'pending',
      filesRead: s.backfill?.files_read ?? null,
      filesTotal: s.backfill?.files_total ?? null,
    }));
  return {
    computers: sources.computers.map((c) => ({
      host: c.computer,
      oldestAt: at(c.oldestMs),
      newestAt: at(c.newestMs),
      lastReadAt: at(c.lastReadMs),
      gaps: c.gaps.map(([a, b]) => [iso(a), iso(b)] as [string, string]),
    })),
    sources: sources.sources.map((s) => ({
      host: s.computer,
      tool: s.tool,
      state: s.state,
      cause: s.cause,
      lastReadAt: at(s.lastReadMs),
      badSince: at(s.badSinceMs),
      oldestAt: at(s.oldestMs),
    })),
    backfill: {
      running: backfill.some((b) => b.state === 'pending' || b.state === 'running'),
      sources: backfill,
    },
  };
}

/**
 * The page's activity, from the store only: never waits on a collection. `live` carries what the
 * collector knows right now (whether a pass runs, the source grid, pricing).
 */
export async function storeActivityAnswer(
  store: AacStore,
  query: AccountAnalyticsQuery,
  from: number,
  to: number,
  live: {
    tz: string;
    pricing?: AccountAnalyticsPricingLookup;
    sources: AccountAnalyticsSource[];
    refreshing: boolean;
    refreshingRemote: AccountAnalyticsActivity['refreshingRemote'];
  }
): Promise<{
  activity: AccountAnalyticsActivity & {
    history: StoreActivityCoverage;
    resolution: 'hour' | 'day';
  };
  oldestHourAt: number | null;
  providersWithActivity: DashboardProvider[];
}> {
  const daily = to - from > HOURLY_RANGE_MS;
  const dayStarts = daily ? localDayStarts(from, to, live.tz) : [];
  const [range, sourcesState] = await Promise.all([
    store.activityRange({
      fromMs: from,
      toMs: to,
      resolution: daily ? 'day' : 'hour',
      ...(daily ? { dayStarts } : {}),
      sessionSample: 200,
    }),
    store.activitySources({ fromMs: from, toMs: to }),
  ]);
  const coverage = coverageOf(sourcesState);
  const lastRead = sourcesState.computers.reduce((max, c) => Math.max(max, c.lastReadMs ?? 0), 0);
  const fetchedAt = iso(lastRead || Date.now());
  // A day bucket is labelled by the first whole hour of its local day, so the projection files it
  // under that day in the page's zone.
  const stamp = (bucket: number) => (daily ? Math.ceil(bucket / HOUR) * HOUR : bucket);
  const sources = rangeToSources(range, stamp, fetchedAt);
  const stored = sourcesState.computers.some((c) => c.oldestMs !== null || c.lastReadMs !== null);
  const status: AccountAnalyticsActivity['status'] = sources.length
    ? live.refreshing
      ? 'cached'
      : 'ok'
    : stored
      ? 'ok'
      : live.refreshing
        ? 'loading'
        : 'unavailable';
  const projectFrom = daily ? Math.min(from, dayStarts[0] ?? from) : from;
  const projectTo = daily ? Math.max(to, stamp(dayStarts[dayStarts.length - 2] ?? to)) : to;
  const message =
    status === 'loading'
      ? 'Usage history is being read for the first time. Quota history is available immediately.'
      : `CLI activity kept in AAC's history (${daily ? 'daily buckets in ' + live.tz : 'UTC hourly buckets; days are grouped in ' + live.tz}). Cost is an API-equivalent estimate, not a subscription charge.`;
  const projected = projectAccountAnalyticsActivity(
    sources,
    query,
    projectFrom,
    projectTo,
    status,
    message,
    {
      tz: live.tz,
      ...(live.pricing ? { pricing: live.pricing } : {}),
      sources: live.sources,
      refreshing: live.refreshing,
      refreshingRemote: live.refreshingRemote,
    }
  );
  // Session counts cover every session in the range, not only the sample the projection saw.
  const counts = sessionCounts(range);
  for (const p of projected.providers) p.sessionCount = counts.get(p.provider) ?? 0;
  if (projected.sessions)
    projected.sessions = {
      ...projected.sessions,
      total:
        query.provider === 'all'
          ? range.sessions.total
          : (counts.get(query.provider as AccountAnalyticsUsageProvider) ?? 0),
      truncated: range.sessions.truncated || projected.sessions.truncated,
    };
  const oldest = sourcesState.computers.reduce<number | null>(
    (min, c) => (c.oldestMs === null ? min : min === null ? c.oldestMs : Math.min(min, c.oldestMs)),
    null
  );
  return {
    activity: { ...projected, history: coverage, resolution: daily ? 'day' : 'hour' },
    oldestHourAt: oldest,
    providersWithActivity: projected.providers
      .map((p) => p.provider)
      .filter((p): p is DashboardProvider => p !== 'other'),
  };
}
