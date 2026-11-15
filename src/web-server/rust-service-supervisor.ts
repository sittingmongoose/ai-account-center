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
import { requestRust, registerRustAgentConnection } from './rust-service-transport';

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
  /** Counts actual replies; contracts assert this to prevent an accidental all-TS pass. */
  readonly stats = { served: 0, shadowed: 0, differences: 0, fallbacks: 0 };
  /** L1: per (route, outcome, reason) counters and paired timings; a log line per state change. */
  readonly routes = new RouteStats((route, state, previous, count) => {
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
  protected readonly breakers: RouteBreakers;
  protected readonly restarts: RestartBudget;
  protected lease: RustLease | null = null;
  private removeAgentConnection: (() => void) | null = null;
  protected active = new Set<http.ClientRequest>();
  protected inFlight = 0;
  protected readonly shadowInFlight = new Map<string, number>();
  protected stopped = false;
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
    this.breakers = new RouteBreakers(options.now);
    this.restarts = new RestartBudget(options.now, options.restartDelaysMs);
  }

  async start(
    binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(__dirname, '../native/aac-http')
  ): Promise<void> {
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
    this.minuteTimer = setInterval(() => this.logMinute(), 60_000);
    this.minuteTimer.unref();
    // The Rust child's RSS joins the event-loop monitor's minute line (design 4.2).
    this.removeMonitorFields = addEventLoopMonitorFields(() => this.monitorFields());
    let failure: string | null = 'unhealthy';
    try {
      const result = await acquireRustServiceDetailed(getCcsDir(), binary, this.active);
      failure = result.failure;
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
  }

  stop(): void {
    this.stopped = true;
    if (this.restartTimer) clearTimeout(this.restartTimer);
    if (this.minuteTimer) clearInterval(this.minuteTimer);
    this.restartTimer = null;
    this.minuteTimer = null;
    this.removeMonitorFields?.();
    this.removeMonitorFields = null;
    for (const request of this.active) request.destroy();
    this.removeAgentConnection?.();
    this.removeAgentConnection = null;
    this.lease?.close();
    this.lease = null;
  }

  protected adopt(lease: RustLease): void {
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
    this.lease = lease;
    lease.onExit((code, signal) => this.lost(lease, 'exit', { code, signal }));
  }

  /** The service died or stopped answering: TS answers until a restart succeeds (L1). */
  protected lost(lease: RustLease, cause: string, context: Record<string, unknown> = {}): void {
    if (this.stopped || this.lease !== lease) return;
    this.removeAgentConnection?.();
    this.removeAgentConnection = null;
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
    if (delay === null) {
      this.gaveUp = true;
      logger.error(
        'rust.service.gave_up',
        'Rust service restarted too often; off until the next dashboard start',
        { restarts: this.restarts.used }
      );
      return;
    }
    const binary = this.binary;
    this.restartTimer = setTimeout(() => {
      void (async () => {
        let failure: string | null = 'unhealthy';
        try {
          const result = await acquireRustServiceDetailed(getCcsDir(), binary, this.active);
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
      });
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
