/**
 * L1 bridge floor, pure parts: the per-route breaker, the restart budget, the per-route counters
 * with their state-change log, and the paired timings shadow runs need (design section 4.2).
 * Clocks are injected so the rules are unit-tested without waiting.
 */

/** 3 transport failures or timeouts in a row open a route's breaker for 30 s, then one probe. */
export const BREAKER_FAILURES = 3;
export const BREAKER_OPEN_MS = 30_000;

export type BreakerState = 'closed' | 'open' | 'half-open';

interface Breaker {
  failures: number;
  openUntil: number;
  probing: boolean;
}

export class RouteBreakers {
  private readonly breakers = new Map<string, Breaker>();
  constructor(private readonly now: () => number = Date.now) {}

  private get(route: string): Breaker {
    let breaker = this.breakers.get(route);
    if (!breaker) {
      breaker = { failures: 0, openUntil: 0, probing: false };
      this.breakers.set(route, breaker);
    }
    return breaker;
  }

  state(route: string): BreakerState {
    const breaker = this.breakers.get(route);
    if (!breaker || breaker.openUntil === 0) return 'closed';
    return this.now() < breaker.openUntil || breaker.probing ? 'open' : 'half-open';
  }

  /** A new boot: the failures counted were the previous process's. */
  reset(): void {
    this.breakers.clear();
  }

  /** Would `allow` refuse now? Reserves nothing (admission checks call this first). */
  blocked(route: string): boolean {
    const breaker = this.breakers.get(route);
    if (!breaker || breaker.openUntil === 0) return false;
    return this.now() < breaker.openUntil || breaker.probing;
  }

  /**
   * May this request go to Rust? After the open period exactly one probe goes. It reserves that
   * probe, so call it only at dispatch: a reserved probe must end in `success` or `failure`.
   */
  allow(route: string): boolean {
    const breaker = this.get(route);
    if (breaker.openUntil === 0) return true;
    if (this.now() < breaker.openUntil || breaker.probing) return false;
    breaker.probing = true;
    return true;
  }

  /** Any reply, a named decline included: the transport works. */
  success(route: string): void {
    const breaker = this.get(route);
    breaker.failures = 0;
    breaker.openUntil = 0;
    breaker.probing = false;
  }

  /** A transport failure or timeout. Returns true when this one opened the breaker. */
  failure(route: string): boolean {
    const breaker = this.get(route);
    const probe = breaker.probing;
    breaker.probing = false;
    breaker.failures++;
    if (probe || breaker.failures >= BREAKER_FAILURES) {
      const opened = breaker.openUntil === 0 || probe;
      breaker.openUntil = this.now() + BREAKER_OPEN_MS;
      return opened;
    }
    return false;
  }
}

/** Restart backoff after a crash: 1 s, 5 s, 30 s, 2 min, 10 min; at most 5 in an hour. */
export const RESTART_DELAYS_MS = [1_000, 5_000, 30_000, 120_000, 600_000];
export const RESTARTS_PER_HOUR = 5;

export class RestartBudget {
  private readonly restarts: number[] = [];
  constructor(
    private readonly now: () => number = Date.now,
    private readonly delays: readonly number[] = RESTART_DELAYS_MS
  ) {}

  /** The delay before the next restart, or null when the hour's budget is used up. */
  next(): number | null {
    const now = this.now();
    while (this.restarts.length > 0 && now - this.restarts[0] >= 3_600_000) this.restarts.shift();
    if (this.restarts.length >= RESTARTS_PER_HOUR) return null;
    const delay = this.delays[Math.min(this.restarts.length, this.delays.length - 1)];
    this.restarts.push(now + delay);
    return delay;
  }

  get used(): number {
    return this.restarts.length;
  }
}

/** One request's outcome on a route. */
export type RouteOutcome =
  | { kind: 'served' }
  | { kind: 'equal' }
  | { kind: 'declined'; reason: string }
  | { kind: 'failed'; reason: string }
  | { kind: 'different' }
  | { kind: 'unavailable'; reason: string }
  | { kind: 'skipped' };

/** The paired samples kept per route. */
const PAIRS = 512;

export interface RouteCounters {
  served: number;
  equal: number;
  different: number;
  skipped: number;
  declined: Record<string, number>;
  failed: Record<string, number>;
  unavailable: Record<string, number>;
  /** The latest state word: `serving`, `declined:<reason>`, `failed:<kind>`, ... */
  state: string;
  /** Paired timings: TS path and Rust path on the same request (ms; null when one did not run). */
  pairs: { ts: number | null; rust: number | null }[];
  pairCursor: number;
}

export interface TimingSummary {
  samples: number;
  p50Ms: number | null;
  p95Ms: number | null;
}

export function summarize(values: number[]): TimingSummary {
  if (values.length === 0) return { samples: 0, p50Ms: null, p95Ms: null };
  const sorted = [...values].sort((a, b) => a - b);
  const at = (q: number) => sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * q) - 1)];
  const ms = (value: number) => Math.round(value * 1000) / 1000;
  return { samples: sorted.length, p50Ms: ms(at(0.5)), p95Ms: ms(at(0.95)) };
}

function stateOf(outcome: RouteOutcome): string | null {
  switch (outcome.kind) {
    case 'served':
    case 'equal':
      return 'serving';
    case 'different':
      return 'different';
    case 'skipped':
      return null;
    default:
      return `${outcome.kind}:${outcome.reason}`;
  }
}

export type StateLog = (route: string, state: string, previous: string, count: number) => void;

/** Counters per (route, outcome, reason); a log line on each change of a route's state. */
export class RouteStats {
  readonly routes = new Map<string, RouteCounters>();
  /** Outcomes since the previous state change, per route (the count in the next log line). */
  private readonly since = new Map<string, number>();
  constructor(private readonly log: StateLog) {}

  private counters(route: string): RouteCounters {
    let counters = this.routes.get(route);
    if (!counters) {
      counters = {
        served: 0,
        equal: 0,
        different: 0,
        skipped: 0,
        declined: {},
        failed: {},
        unavailable: {},
        state: 'new',
        pairs: [],
        pairCursor: 0,
      };
      this.routes.set(route, counters);
    }
    return counters;
  }

  record(route: string, outcome: RouteOutcome): void {
    const counters = this.counters(route);
    switch (outcome.kind) {
      case 'served':
        counters.served++;
        break;
      case 'equal':
        counters.equal++;
        break;
      case 'different':
        counters.different++;
        break;
      case 'skipped':
        counters.skipped++;
        break;
      default:
        counters[outcome.kind][outcome.reason] = (counters[outcome.kind][outcome.reason] ?? 0) + 1;
    }
    const state = stateOf(outcome);
    if (state === null) return;
    const count = (this.since.get(route) ?? 0) + 1;
    if (state !== counters.state) {
      const previous = counters.state;
      counters.state = state;
      this.since.set(route, 0);
      this.log(route, state, previous, count);
    } else this.since.set(route, count);
  }

  pair(route: string, ts: number | null, rust: number | null): void {
    const counters = this.counters(route);
    if (counters.pairs.length < PAIRS) counters.pairs.push({ ts, rust });
    else counters.pairs[counters.pairCursor] = { ts, rust };
    counters.pairCursor = (counters.pairCursor + 1) % PAIRS;
  }

  /** The diagnostics block: counters and paired timings per route, no request data. */
  snapshot(): Record<string, unknown> {
    const routes: Record<string, unknown> = {};
    for (const [route, c] of [...this.routes].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) {
      const both = c.pairs.filter((p) => p.ts !== null && p.rust !== null);
      routes[route] = {
        state: c.state,
        served: c.served,
        equal: c.equal,
        different: c.different,
        skipped: c.skipped,
        declined: c.declined,
        failed: c.failed,
        unavailable: c.unavailable,
        paired: {
          ts: summarize(both.map((p) => p.ts as number)),
          rust: summarize(both.map((p) => p.rust as number)),
        },
        rustOnly: summarize(
          c.pairs.filter((p) => p.ts === null && p.rust !== null).map((p) => p.rust as number)
        ),
      };
    }
    return routes;
  }
}
