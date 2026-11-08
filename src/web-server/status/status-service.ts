/**
 * Runs the Status tab's readers and collectors and keeps their latest results.
 *
 * - Agent reports: rescanned every 10 s (stat first, read only changed files).
 * - pm-mail registry: every 30 s, read-only.
 * - Machines, browser-check and Simlock: every minute while the Status page has been read in the
 *   last 15 minutes (they start processes and ssh sessions; nobody watching, nothing runs). A read
 *   after a quiet spell starts a collection at once and the page hears `status-changed` when it
 *   lands.
 * - Usage: AAC's own analytics samples (no provider refresh).
 * - Answers: failed share writes and doorbells are retried every minute.
 * A change reaches `/ws` clients as `{ type: 'status-changed' }`, at most one every 5 seconds.
 */
import { broadcastDashboardEvent } from '../dashboard-events';
import { buildSnapshotFiles } from '../agent-usage/agent-usage-snapshot';
import { RecentReadings } from '../agent-usage/agent-usage-history';
import {
  onAccountAnalyticsSample,
  type AccountAnalyticsSampleListener,
} from '../services/account-analytics-service';
import { getAccountRefreshIntervalSeconds } from '../services/account-refresh-settings';
import {
  DEFAULT_DASHBOARD_TIME_ZONE,
  readDashboardPreferences,
} from '../services/dashboard-preferences';
import { ValidationError } from '../../errors/error-types';
import { createLogger } from '../../services/logging';
import { AgentReportsReader, ensureReportsLayout, publishSchemas } from './agent-reports-reader';
import { DOORBELL_SENDER, StatusAnswers } from './answers';
import { QuotaReportReader } from './quota-report';
import { MACHINES, type MachineId } from './report-schema';
import {
  busyBetween,
  commandFailure,
  findExecutable,
  localMachineId,
  mergeBrowserRows,
  nextProblem,
  parseBrowserSlots,
  parseSimlockStatus,
  PmMailRegistryReader,
  readLocalMachine,
  readRemoteMachine,
  runCommand,
  type BrowserMachine,
  type CommandRunner,
  type MachineSample,
  type ReadProblem,
  type RegistryThread,
  type SimlockStatus,
} from './status-collectors';
import { buildStatusView, type Collected, type StatusView, type UsageInput } from './status-view';

const logger = createLogger('status-service');

const REPORTS_EVERY_MS = 10_000;
const REGISTRY_EVERY_MS = 30_000;
const COLLECT_EVERY_MS = 60_000;
const DEMAND_WINDOW_MS = 15 * 60_000;
const HINT_GAP_MS = 5_000;

function empty<T>(): Collected<T> {
  return { data: null, asOf: null, error: null };
}

function succeeded<T>(data: T, now: number): Collected<T> {
  return { data, asOf: now, error: null };
}

function failed<T>(
  before: Collected<T>,
  reason: string,
  now: number,
  missing = false
): Collected<T> {
  return {
    data: before.data,
    asOf: before.asOf,
    error: { reason, since: before.error?.reason === reason ? before.error.since : now },
    missing,
    problems: before.problems,
  };
}

function errorReason(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

export interface StatusServiceDeps {
  reader?: AgentReportsReader;
  quotaReader?: QuotaReportReader;
  registryReader?: PmMailRegistryReader;
  answers?: StatusAnswers;
  run?: CommandRunner;
  /** Which machine row this computer is; by default from its host name. */
  localMachine?: () => MachineId;
  readLocalMachine?: () => Promise<MachineSample>;
  readRemoteMachine?: (machine: Exclude<MachineId, 'vm'>) => Promise<MachineSample>;
  timeZone?: () => string;
  now?: () => number;
  /** Sends the `/ws` hint; defaults to browser-session clients. */
  notify?: () => void;
  subscribeUsage?: (listener: AccountAnalyticsSampleListener) => () => void;
  refreshSeconds?: () => number;
}

export class StatusService {
  readonly reader: AgentReportsReader;
  readonly quotaReader: QuotaReportReader;
  readonly answers: StatusAnswers;
  private readonly registryReader: PmMailRegistryReader;
  private registry: Collected<RegistryThread[]> = empty();
  private machines: Record<MachineId, Collected<MachineSample>> = {
    vm: empty(),
    mac: empty(),
    windows: empty(),
    nas1: empty(),
  };
  private browsers: Collected<BrowserMachine[]> = empty();
  private simlock: Collected<SimlockStatus> = empty();
  private usage: UsageInput | null = null;
  private lastDemand = 0;
  private lastCollect = 0;
  private collecting: Promise<void> | null = null;
  private hintTimer: ReturnType<typeof setTimeout> | null = null;
  private lastHint = 0;
  private readonly recent = new RecentReadings();

  constructor(private readonly deps: StatusServiceDeps = {}) {
    this.reader = deps.reader ?? new AgentReportsReader();
    this.quotaReader = deps.quotaReader ?? new QuotaReportReader({ root: () => this.reader.root });
    this.registryReader = deps.registryReader ?? new PmMailRegistryReader();
    this.answers =
      deps.answers ??
      new StatusAnswers({
        readCard: (id) => this.reader.readCardNow(id),
        onChange: () => this.changed(),
      });
  }

  private now(): number {
    return (this.deps.now ?? Date.now)();
  }

  private timeZone(): string {
    if (this.deps.timeZone) return this.deps.timeZone();
    try {
      return readDashboardPreferences().timeZone;
    } catch {
      return DEFAULT_DASHBOARD_TIME_ZONE;
    }
  }

  /** A hint to `/ws` clients, at most one per HINT_GAP_MS (the last change always gets one). */
  changed(): void {
    if (this.hintTimer) return;
    const wait = Math.max(0, this.lastHint + HINT_GAP_MS - Date.now());
    this.hintTimer = setTimeout(() => {
      this.hintTimer = null;
      this.lastHint = Date.now();
      try {
        (
          this.deps.notify ??
          (() =>
            broadcastDashboardEvent((client) =>
              client.authKind === 'session' ? { type: 'status-changed' } : null
            ))
        )();
      } catch {
        /* A hint never fails the collectors. */
      }
    }, wait);
    this.hintTimer.unref?.();
  }

  async scanReports(): Promise<void> {
    try {
      if (await this.reader.scan()) this.changed();
    } catch {
      /* The reader keeps its own state; the source shows stale. */
    }
    try {
      if (await this.quotaReader.scan()) this.changed();
    } catch {
      /* The reader keeps its own state; the source shows stale. */
    }
  }

  async scanRegistry(): Promise<void> {
    try {
      const now = this.now();
      const { threads, problems, changed } = await this.registryReader.scan(now);
      // A failed file is a named problem: the scan is partial, never a clean success.
      this.registry = { ...succeeded(threads, now), problems: problems ?? [] };
      if (changed) this.changed();
    } catch (error) {
      const missing = /missing/.test(errorReason(error, ''));
      this.registry = failed(
        this.registry,
        errorReason(error, 'The pm-mail registry could not be read.'),
        this.now(),
        missing
      );
      this.changed();
    }
  }

  private run(): CommandRunner {
    return this.deps.run ?? runCommand;
  }

  async collectBrowsers(): Promise<void> {
    const result = await this.run()(findExecutable('browser-check'), ['slots', '--json'], {
      timeoutMs: 45_000,
      maxBytes: 4 * 1024 * 1024,
    });
    const now = this.now();
    if (result.code !== 0) {
      this.browsers = failed(
        this.browsers,
        commandFailure('browser-check', result),
        now,
        result.missing
      );
    } else {
      try {
        const rows = mergeBrowserRows(
          this.browsers.data,
          parseBrowserSlots(JSON.parse(result.stdout)),
          now
        );
        this.browsers = {
          ...succeeded(rows, now),
          problems: rows.flatMap((row) => (row.problem ? [row.problem] : [])),
        };
      } catch (error) {
        this.browsers = failed(
          this.browsers,
          errorReason(error, 'browser-check answered in an unknown shape.').replace(
            /^Unexpected.*$/,
            'browser-check answered in an unknown shape.'
          ),
          now
        );
      }
    }
    this.changed();
  }

  async collectSimlock(): Promise<void> {
    const result = await this.run()(findExecutable('simlock'), ['status', '--json'], {
      timeoutMs: 20_000,
      maxBytes: 4 * 1024 * 1024,
    });
    const now = this.now();
    if (result.code !== 0) {
      this.simlock = failed(this.simlock, commandFailure('simlock', result), now, result.missing);
    } else {
      try {
        const status = parseSimlockStatus(JSON.parse(result.stdout));
        const before = this.simlock.problems ?? [];
        const problems: ReadProblem[] = status.problems.map((p) => {
          const seen = p.lastSeenAt ? Date.parse(p.lastSeenAt) : NaN;
          const last = before.find((b) => b.item === p.item);
          return Number.isFinite(seen)
            ? { item: p.item, reason: p.reason, since: seen }
            : nextProblem(last, p.item, p.reason, now);
        });
        this.simlock = { ...succeeded(status, now), problems };
      } catch (error) {
        this.simlock = failed(
          this.simlock,
          error instanceof SyntaxError
            ? 'simlock answered in an unknown shape.'
            : errorReason(error, 'simlock answered in an unknown shape.'),
          now
        );
      }
    }
    this.changed();
  }

  async collectMachines(): Promise<void> {
    const local = (this.deps.localMachine ?? localMachineId)();
    const read = async (id: MachineId) => {
      try {
        if (id !== local && id === 'vm')
          throw new ValidationError('No ssh host is configured for this machine.');
        const sample =
          id === local
            ? await (this.deps.readLocalMachine ?? readLocalMachine)()
            : await (this.deps.readRemoteMachine ?? ((m) => readRemoteMachine(m, this.run())))(
                id as Exclude<MachineId, 'vm'>
              );
        const held = this.machines[id];
        const before = held.data;
        const busy = busyBetween(before?.cpuTimes ?? null, sample.cpuTimes);
        const now = this.now();
        const busyPct = busy ?? sample.busyPct;
        const data: MachineSample = {
          ...sample,
          busyPct,
          cpuReadAt: busyPct === null ? (before?.cpuReadAt ?? null) : now,
          diskReadAt: sample.disk ? now : null,
          memoryReadAt: sample.memoryError ? null : now,
        };
        // A part (disk, memory) that failed while the rest was read: keep its earlier reading with
        // its own time, and name the failure on the machine until that part reads again.
        const problems: ReadProblem[] = [];
        const earlier = (item: string) => held.problems?.find((problem) => problem.item === item);
        if (!sample.disk && sample.diskError) {
          data.disk = before?.disk ?? null;
          data.diskReadAt = before?.disk ? (before.diskReadAt ?? held.asOf) : null;
          problems.push(nextProblem(earlier('disk'), 'disk', sample.diskError, now));
        }
        if (sample.cpuError) {
          // Keep the earlier busy figure and counters: the next good read measures across the gap.
          data.busyPct = before?.busyPct ?? null;
          data.cpuTimes = before?.cpuTimes ?? null;
          data.cpuReadAt = before?.cpuReadAt ?? null;
          problems.push(nextProblem(earlier('cpu'), 'cpu', sample.cpuError, now));
        }
        if (sample.memoryError) {
          // The held reading already carries the last good memory forward, with its time.
          data.memAvailableBytes = before?.memAvailableBytes ?? null;
          data.memTotalBytes = before?.memTotalBytes ?? null;
          data.memoryReadAt = before?.memoryReadAt ?? null;
          problems.push(nextProblem(earlier('memory'), 'memory', sample.memoryError, now));
        }
        this.machines[id] =
          problems.length === 0 ? succeeded(data, now) : { data, asOf: now, error: null, problems };
      } catch (error) {
        this.machines[id] = failed(
          this.machines[id],
          errorReason(error, 'The machine did not answer.'),
          this.now()
        );
      }
    };
    await Promise.all(MACHINES.map(read));
    this.changed();
  }

  /** Machines, browsers and Simlock together; overlapping calls share one run. */
  collect(): Promise<void> {
    if (!this.collecting) {
      this.lastCollect = this.now();
      this.collecting = Promise.all([
        this.collectMachines(),
        this.collectBrowsers(),
        this.collectSimlock(),
      ])
        .then(() => undefined)
        .catch(() => undefined)
        .finally(() => {
          this.collecting = null;
        });
    }
    return this.collecting;
  }

  private wanted(): boolean {
    return this.now() - this.lastDemand < DEMAND_WINDOW_MS;
  }

  /** Records an analytics sample (the dashboard's own readings). */
  recordUsage: AccountAnalyticsSampleListener = (event) => {
    try {
      this.recent.add(event.accounts, event.sampledAt);
      const refresh = (this.deps.refreshSeconds ?? getAccountRefreshIntervalSeconds)();
      const { snapshot } = buildSnapshotFiles(event, refresh, this.recent);
      if (snapshot.accounts.length === 0) return;
      this.usage = {
        accounts: snapshot.accounts,
        sampledAt: event.sampledAt,
        staleAfterSeconds: snapshot.staleAfterSeconds,
      };
      this.changed();
    } catch {
      /* The usage panel keeps its last sample. */
    }
  };

  /** The view now. A read counts as demand for the process-starting collectors. */
  view(): StatusView {
    const now = this.now();
    const idle = !this.wanted();
    this.lastDemand = now;
    if (idle || now - this.lastCollect >= COLLECT_EVERY_MS) void this.collect();
    // A failed private source must not hide the independently collected panels.
    let answers: ReturnType<StatusAnswers['list']> = [];
    let sends: ReturnType<StatusAnswers['sendList']> = [];
    let answerHistoryProblem: string | null = null;
    try {
      answers = this.answers.list();
      sends = this.answers.sendList();
    } catch {
      answers = [];
      answerHistoryProblem = "The answer history can't be read right now.";
    }
    return buildStatusView({
      now,
      timeZone: this.timeZone(),
      reports: this.reader.snapshot(),
      registry: this.registry,
      machines: this.machines,
      browsers: this.browsers,
      simlock: this.simlock,
      usage: this.usage,
      answers,
      sends,
      answerHistoryProblem,
      hiddenThreadNames: [DOORBELL_SENDER],
      quota: this.quotaReader.snapshot(),
    });
  }

  /** Starts the timers; returns the stop call. */
  start(): () => void {
    const timers: Array<ReturnType<typeof setInterval>> = [];
    const every = (ms: number, work: () => Promise<void>) => {
      let running = false;
      const tick = () => {
        if (running) return;
        running = true;
        void work()
          .catch(() => undefined)
          .finally(() => {
            running = false;
          });
      };
      const timer = setInterval(tick, ms);
      timer.unref?.();
      timers.push(timer);
      tick();
    };
    // AAC owns v1/ and its folders; the reporter never creates them.
    void ensureReportsLayout(this.reader.root)
      .then(() => publishSchemas(this.reader.root))
      .catch(() => undefined);
    every(REPORTS_EVERY_MS, () => this.scanReports());
    every(REGISTRY_EVERY_MS, () => this.scanRegistry());
    every(COLLECT_EVERY_MS, async () => {
      if (this.wanted()) await this.collect();
    });
    every(COLLECT_EVERY_MS, () => this.answers.retryPending());
    const unsubscribe = (this.deps.subscribeUsage ?? onAccountAnalyticsSample)(this.recordUsage);
    return () => {
      this.answers.flushCompletions();
      for (const timer of timers) clearInterval(timer);
      unsubscribe();
      if (this.hintTimer) clearTimeout(this.hintTimer);
      this.hintTimer = null;
    };
  }
}

let service: StatusService | null = null;

export function getStatusService(): StatusService {
  service ??= new StatusService();
  return service;
}

/** For tests: replace (or drop) the process-wide service. */
export function setStatusServiceForTests(next: StatusService | null): void {
  service = next;
}

/** Starts the process-wide service; returns the stop call. */
export function startStatusService(): () => void {
  try {
    return getStatusService().start();
  } catch {
    try {
      logger.warn('status.start_failed', 'The Status tab collectors could not start');
    } catch {
      /* Logging never stops the dashboard. */
    }
    return () => undefined;
  }
}
