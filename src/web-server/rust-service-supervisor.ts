/**
 * L1 service lifecycle under the bridge: start or attach, restart a crashed service with backoff
 * (at most five an hour), per-route counters and paired timings with their logs, and the
 * diagnostics block. The read methods live in rust-service.ts on top of this.
 */
import fs from 'fs';
import type http from 'http';
import path from 'path';
import { loadUnifiedConfig } from '../config/unified-config-loader';
import { addEventLoopMonitorFields } from '../utils/event-loop-monitor';
import { createLogger } from '../services/logging';
import { getCcsDir } from '../utils/config-manager';
import {
  acquireRustServiceDetailed,
  PUBLIC_HEALTH_PRINCIPAL,
  type RustLease,
} from './rust-service-lifecycle';
import { RestartBudget, RouteBreakers, RouteStats, type RouteOutcome } from './rust-service-health';
import {
  notifyRustStoreGaveUp,
  registerRustStoreConnection,
  requestRust,
  registerRustAgentConnection,
  setRustStoreHosted,
  type RustStoreBootInput,
} from './rust-service-transport';
import { settlePrivateFileOwners } from '../shared/private-files';
import { rustGroupOn, rustStoreSwitchOn } from './rust-service-groups';
import { loadNativeStore } from '../store/native-loader';
import { shareBackupEnv, storeOpenEnv, STORE_RAW_RETENTION_DAYS } from '../store/store-service';
import { readSnapshotSettings } from './agent-usage/agent-usage-settings';

export type RustServiceMode = 'off' | 'shadow' | 'on';

/**
 * `AAC_RUST_SERVICE`, else `rustService.mode` in config.yaml, else off. Invalid values stay off.
 */
export function rustServiceMode(explicit?: string): RustServiceMode {
  let value: unknown = explicit ?? process.env.AAC_RUST_SERVICE;
  if (value === undefined) {
    try {
      const section = loadUnifiedConfig()?.rustService;
      value =
        section && typeof section === 'object' ? (section as { mode?: unknown }).mode : undefined;
    } catch {
      value = undefined;
    }
  }
  return value === 'on' || value === 'shadow' ? value : 'off';
}

export const logger = createLogger('rust-service');

/** Transport timeouts in a row before the process's health is probed (review L3). */
export const HUNG_TIMEOUTS = 3;

export class RustSupervisor {
  readonly mode: RustServiceMode;
  protected readonly writesOn: boolean;
  /** Counts actual replies; contracts assert this to prevent an accidental all-TS pass. */
  readonly stats = { served: 0, shadowed: 0, differences: 0, fallbacks: 0 };
  /** L1: per (route, outcome, reason) counters and paired timings; a log line per state change. */
  readonly routes = new RouteStats((route, state, previous, count) => {
    if (!this.admitComparisonLog(route, state)) return;
    const context = { route, state, previous, count };
    if (state === 'serving') logger.info('rust.service.route_state', 'Rust route serving', context);
    else
      logger.warn(
        'rust.service.route_state',
        'Rust route state changed; TypeScript answers',
        context
      );
  });
  /** The same outcomes for the current minute only (the `rust.service.minute` line). */
  protected minute = new RouteStats(() => {});
  private readonly comparisonLogs = new Map<string, { at: number; states: Set<string> }>();
  private suppressedComparisons = 0;

  /** Bound even alternating equal/different states to two entries per route per minute. */
  private admitComparisonLog(route: string, state: string): boolean {
    const previous = this.comparisonLogs.get(route);
    if (previous && Date.now() - previous.at < 60_000) {
      if (!previous.states.has(state) && previous.states.size < 2) {
        previous.states.add(state);
        return true;
      }
      this.suppressedComparisons++;
      return false;
    }
    this.comparisonLogs.set(route, { at: Date.now(), states: new Set([state]) });
    return true;
  }
  protected readonly breakers: RouteBreakers;
  protected readonly restarts: RestartBudget;
  protected lease: RustLease | null = null;
  private removeAgentConnection: (() => void) | null = null;
  private removeStoreConnection: (() => void) | null = null;
  /** R3: the store host's boot input, resolved once in `start`; null keeps the store in TS. */
  protected storeBoot: RustStoreBootInput | null = null;
  protected active = new Set<http.ClientRequest>();
  protected inFlight = 0;
  protected readonly shadowInFlight = new Map<string, number>();
  protected stopped = false;
  private readonly acquisition = new AbortController();
  protected binary: string | null = null;
  protected restartTimer: NodeJS.Timeout | null = null;
  protected minuteTimer: NodeJS.Timeout | null = null;
  protected removeMonitorFields: (() => void) | null = null;
  protected gaveUp = false;
  protected checking = false;
  /** Transport timeouts in a row on the current lease, any route (review L3). */
  protected timeouts = 0;

  /** `options` exists for tests: a clock for the breakers and the restart budget, and delays. */
  constructor(
    mode?: string,
    options: { now?: () => number; restartDelaysMs?: readonly number[] } = {}
  ) {
    this.mode = rustServiceMode(mode);
    // L2: the writer assignment sent in the private boot. Writes have their own switch and
    // need the Unix service. Shadow keeps the TS writer.
    this.writesOn = this.mode === 'on' && process.platform !== 'win32' && rustGroupOn('writes');
    settlePrivateFileOwners((group) => group === 'writes' && this.writesOn);
    this.breakers = new RouteBreakers(options.now);
    this.restarts = new RestartBudget(options.now, options.restartDelaysMs);
  }

  async start(
    binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(__dirname, '../native/aac-http')
  ): Promise<void> {
    if (this.stopped) return;
    if (this.mode === 'off') {
      let transport: unknown = process.env.AAC_AGENT_TRANSPORT;
      if (transport === undefined) {
        try {
          transport = loadUnifiedConfig()?.agentTransport;
        } catch {
          transport = undefined;
        }
      }
      if (transport !== 'on') return;
    }
    this.binary = binary;
    // R3: the store host's boot input, resolved once: its own switch, mode on (no shadow
    // for the one writer) and the store not disabled. Everything else keeps the store here.
    if (
      this.mode === 'on' &&
      rustStoreSwitchOn() &&
      (process.env.AAC_STORE ?? '').trim().toLowerCase() !== 'off'
    ) {
      let bindingIdentity: unknown = null;
      try {
        const loaded = loadNativeStore();
        const text = loaded.module?.buildIdentity?.();
        if (text) bindingIdentity = JSON.parse(text);
      } catch {
        /* The service rejects an unproved binding before opening the store. */
      }
      this.storeBoot = {
        bindingIdentity,
        retentionDays: STORE_RAW_RETENTION_DAYS,
        ...storeOpenEnv(process.env),
        snapshotDirectory: null,
        ...shareBackupEnv(process.env),
      };
      // L1 starts in the background. Reserve the hosted writer before the first await,
      // so an early producer cannot open NAPI while the boot config is still loading.
      setRustStoreHosted(getCcsDir(), true);
      const snapshotDirectory =
        (await readSnapshotSettings(getCcsDir()).catch(() => null))?.directory ?? null;
      if (this.stopped) return;
      this.storeBoot.snapshotDirectory = snapshotDirectory;
    }
    this.minuteTimer = setInterval(() => this.logMinute(), 60_000);
    this.minuteTimer.unref();
    // The Rust child's RSS joins the event-loop monitor's minute line (design 4.2).
    this.removeMonitorFields = addEventLoopMonitorFields(() => this.monitorFields());
    let failure: string | null = 'unhealthy';
    try {
      const result = await acquireRustServiceDetailed(
        getCcsDir(),
        binary,
        this.active,
        this.acquisition.signal,
        this.storeBoot
      );
      failure = result.failure;
      if (this.stopped) {
        result.lease?.close();
        return;
      }
      if (result.lease) {
        this.adopt(result.lease);
        logger.info('rust.service.ready', 'Private Rust service ready', {
          mode: this.mode,
          owned: result.lease.owned,
          attachRefused: result.refusedAttach,
          swept: result.swept,
        });
        return;
      }
    } catch {
      /* Safe TS fallback. */
    }
    this.stats.fallbacks++;
    logger.warn('rust.service.fallback', 'Rust service absent or unhealthy; TypeScript answers', {
      mode: this.mode,
      reason: failure ?? 'unhealthy',
    });
    if (failure === 'unhealthy' || failure === 'spawn') this.scheduleRestart();
    // R3: no restart is coming for the other failures, so the store falls back at once.
    else if (this.storeBoot) notifyRustStoreGaveUp(getCcsDir());
  }

  stop(): void {
    this.stopped = true;
    this.acquisition.abort();
    if (this.restartTimer) clearTimeout(this.restartTimer);
    if (this.minuteTimer) clearInterval(this.minuteTimer);
    this.restartTimer = null;
    this.minuteTimer = null;
    this.removeMonitorFields?.();
    this.removeMonitorFields = null;
    for (const request of this.active) request.destroy();
    this.removeAgentConnection?.();
    this.removeAgentConnection = null;
    this.removeStoreConnection?.();
    this.removeStoreConnection = null;
    if (this.storeBoot) setRustStoreHosted(getCcsDir(), false);
    this.lease?.close();
    this.lease = null;
  }

  protected adopt(lease: RustLease): void {
    if (this.stopped) {
      lease.close();
      return;
    }
    this.timeouts = 0;
    // The breakers counted the previous process's failures; a new boot starts closed.
    this.breakers.reset();
    this.removeAgentConnection?.();
    this.removeAgentConnection = registerRustAgentConnection(
      lease.connection.ccsDir,
      lease.connection,
      this.active,
      { replied: () => this.replied(lease), failed: (kind) => this.transportFailed(lease, kind) }
    );
    // R3: the store's socket handle resolves this boot per call. An attached boot is
    // registered as such, never driven: its store belongs to the owning front door.
    this.removeStoreConnection?.();
    this.removeStoreConnection = this.storeBoot
      ? registerRustStoreConnection(
          lease.connection.ccsDir,
          lease.connection,
          this.active,
          {
            replied: () => this.replied(lease),
            failed: (kind) => this.transportFailed(lease, kind),
            unresponsive: () => {
              if (lease === this.lease) this.checkLease('unresponsive');
            },
          },
          lease.owned
        )
      : null;
    this.lease = lease;
    lease.onExit((code, signal) => this.lost(lease, 'exit', { code, signal }));
  }

  /** The service died or stopped answering: TS answers until a restart succeeds (L1). */
  protected lost(lease: RustLease, cause: string, context: Record<string, unknown> = {}): void {
    if (this.stopped || this.lease !== lease) return;
    this.removeAgentConnection?.();
    this.removeAgentConnection = null;
    this.removeStoreConnection?.();
    this.removeStoreConnection = null;
    this.lease = null;
    lease.close();
    logger.warn('rust.service.lost', 'Rust service stopped; TypeScript answers', {
      cause,
      ...context,
    });
    this.scheduleRestart();
  }

  protected scheduleRestart(): void {
    if (this.stopped || this.restartTimer || this.gaveUp || !this.binary) return;
    const delay = this.restarts.next();
    // R3's fifth crash consumes the last restart slot as a handoff to NAPI. Do not
    // start another writer only to crash it a sixth time before automatic rollback.
    if (delay === null || (this.storeBoot !== null && this.restarts.used >= 5)) {
      this.gaveUp = true;
      logger.error(
        'rust.service.gave_up',
        'Rust service restarted too often; off until the next dashboard start',
        { restarts: this.restarts.used }
      );
      // R3: the dead service released the store lock; TS opens it in process (design 1.3).
      if (this.storeBoot) notifyRustStoreGaveUp(getCcsDir());
      return;
    }
    const binary = this.binary;
    this.restartTimer = setTimeout(() => {
      void (async () => {
        let failure: string | null = 'unhealthy';
        try {
          const result = await acquireRustServiceDetailed(
            getCcsDir(),
            binary,
            this.active,
            this.acquisition.signal,
            this.storeBoot
          );
          failure = result.failure;
          if (result.lease) {
            if (this.stopped) result.lease.close();
            else {
              this.adopt(result.lease);
              logger.info('rust.service.restarted', 'Private Rust service restarted', {
                attempt: this.restarts.used,
                owned: result.lease.owned,
                swept: result.swept,
              });
            }
          }
        } catch {
          /* Counted below. */
        }
        this.restartTimer = null;
        if (!this.lease && !this.stopped) {
          logger.warn('rust.service.restart_failed', 'Rust service restart failed', {
            attempt: this.restarts.used,
            reason: failure ?? 'unhealthy',
          });
          this.scheduleRestart();
        }
      })();
    }, delay);
    this.restartTimer.unref();
  }

  /** A connect failure: is the boot still there? An attached boot has no exit event to watch. */
  protected checkLease(cause = 'unreachable'): void {
    const lease = this.lease;
    if (!lease || this.checking || this.stopped) return;
    this.checking = true;
    // Health uses no blocking permit, so a busy but live service answers it; requestRust's
    // own 500 ms deadline bounds the probe.
    requestRust(lease.connection, '/api/health', PUBLIC_HEALTH_PRINCIPAL, this.active)
      .then(
        (reply) => reply.status === 200,
        () => false
      )
      .then((healthy) => {
        this.checking = false;
        if (!healthy) this.lost(lease, cause);
      });
  }

  /** Any reply from the service, a named decline included: the process answers. */
  protected replied(lease: RustLease): void {
    if (lease === this.lease) this.timeouts = 0;
  }

  /**
   * A transport failure on `lease`. Connect and boot failures check the boot at once. Transport
   * timeouts (not Rust's own `deadline` declines, which are replies) count across routes: after
   * three in a row, a bounded health probe decides whether the process is hung (SIGSTOP, a
   * deadlock, accepting but never answering). A hung one is retired into the restart budget.
   */
  protected transportFailed(lease: RustLease, kind: string): void {
    if (lease !== this.lease) return;
    if (kind === 'connect' || kind === 'boot') {
      this.checkLease();
      return;
    }
    if (kind !== 'deadline') return;
    this.timeouts++;
    if (this.timeouts >= HUNG_TIMEOUTS) {
      this.timeouts = 0;
      this.checkLease('unresponsive');
    }
  }

  protected record(route: string, outcome: RouteOutcome): void {
    if (
      (outcome.kind === 'equal' || outcome.kind === 'different') &&
      this.routes.routes.get(route)?.state === (outcome.kind === 'equal' ? 'serving' : 'different')
    )
      this.suppressedComparisons++;
    this.routes.record(route, outcome);
    this.minute.record(route, outcome);
  }

  protected pair(route: string, ts: number | null, rust: number | null): void {
    this.routes.pair(route, ts, rust);
    this.minute.pair(route, ts, rust);
  }

  /** A TS answer instead of Rust's, with its route and reason (Fable L4). */
  protected fallback(route: string, outcome: RouteOutcome): void {
    this.stats.fallbacks++;
    this.record(route, outcome);
  }

  protected logMinute(): void {
    const minute = this.minute;
    this.minute = new RouteStats(() => {});
    if (minute.routes.size === 0) return;
    try {
      logger.info('rust.service.minute', 'Rust bridge minute', {
        mode: this.mode,
        rustRssBytes: this.childRssBytes(),
        routes: minute.snapshot(),
        suppressedComparisons: this.suppressedComparisons,
      });
      this.suppressedComparisons = 0;
    } catch {
      /* A failed log disk must not stop the bridge. */
    }
  }

  /** The owned service child's resident memory (Linux), for the event-loop monitor's line. */
  childRssBytes(): number | null {
    const pid = this.lease?.pid;
    if (!pid) return null;
    try {
      const match = /^VmRSS:\s+(\d+)\s+kB$/m.exec(fs.readFileSync(`/proc/${pid}/status`, 'utf8'));
      return match ? Number(match[1]) * 1024 : null;
    } catch {
      return null;
    }
  }

  /** Extra fields for the event-loop monitor's minute line; none while off. */
  monitorFields(): Record<string, unknown> | undefined {
    if (this.mode === 'off') return undefined;
    return { rustMode: this.mode, rustRssBytes: this.childRssBytes() };
  }

  /** `/api/diag?bridge=1`: the service state, counters and paired timings. No request data. */
  diagnostics(): Record<string, unknown> {
    const lease = this.lease;
    return {
      mode: this.mode,
      service: {
        state: this.mode === 'off' ? 'off' : lease ? 'ready' : this.gaveUp ? 'gave-up' : 'down',
        owned: lease?.owned ?? null,
        build: lease?.connection.build ?? null,
        restarts: this.restarts.used,
        rssBytes: this.childRssBytes(),
      },
      stats: { ...this.stats },
      routes: this.routes.snapshot(),
    };
  }
}
