/** M3 strangler: existing endpoint guards run before any verified principal is constructed. */
import type { Request, Response } from 'express';
import { performance } from 'perf_hooks';
import routes from './rust-service-routes.json';
import type { RustLease } from './rust-service-lifecycle';
import {
  canonicalRoot,
  pushRustFeedText,
  requestRust,
  FRONT_DOOR_PRINCIPAL,
  RUST_VIEW_CAP,
  rustFailureKind,
  type RustReply,
  type VerifiedRustPrincipal,
} from './rust-service-transport';
import { rustViewBytes, type RustViewBytes } from './rust-view-reply';
import { canonical, orderedLike, principal, validReply } from './rust-service-values';
import { logger, RustSupervisor } from './rust-service-supervisor';
export type { VerifiedRustPrincipal } from './rust-service-transport';
export { skeletonDiagnostics } from './rust-service-values';
export { rustServiceMode, type RustServiceMode } from './rust-service-supervisor';

/** Requests to Rust in flight per front door. */
const IN_FLIGHT = 16;
/** Shadow comparisons in flight per route (L1); the rest are skipped and counted. */
export const SHADOW_IN_FLIGHT = 2;

/** The roles the table admits on a route (TS gates from the same table Rust dispatches from). */
function routeRoles(route: string): readonly string[] {
  const row = routes.find((r) => r.path === route && r.owner === 'rust') as
    { roles?: string[] } | undefined;
  return row?.roles ?? [];
}

const now = () => performance.now();

export class RustService extends RustSupervisor {
  /**
   * The shared forwarding gate of every read method: an owned GET/HEAD route, a running service,
   * a verified principal the table admits, a closed breaker and a free in-flight slot. Anything
   * else is a TS answer, counted with its reason.
   */
  private forwardable(req: Request, route: string): VerifiedRustPrincipal | null {
    const row = routes.find((r) => r.path === route && r.method === 'GET' && r.owner === 'rust');
    const owned = !!row;
    // DESIGN-M3-COMPLETE 1.6: standalone profiles stay in TS until M6. Their current
    // composition inputs contain identities, login state, private paths and history, not
    // a minimal comparison DTO. Shadow must not copy those facts into a second process.
    if (this.mode === 'shadow' && row?.group === 'profiles') {
      this.fallback(route, { kind: 'unavailable', reason: 'private_input' });
      return null;
    }
    if (this.mode === 'off' || !owned || this.stopped || !['GET', 'HEAD'].includes(req.method))
      return null;
    const verified = principal(req, route);
    const reason = !verified
      ? 'principal'
      : !routeRoles(route).includes(verified.role)
        ? 'role'
        : !this.lease
          ? 'down'
          : this.inFlight >= IN_FLIGHT
            ? 'inflight'
            : this.breakers.blocked(route)
              ? 'breaker'
              : null;
    if (reason) {
      this.fallback(route, { kind: 'unavailable', reason });
      return null;
    }
    return verified;
  }

  /** Shadow: at most two comparisons in flight per route; the rest are skipped and counted. */
  private shadowSlot(route: string): (() => void) | null {
    const used = this.shadowInFlight.get(route) ?? 0;
    if (used >= SHADOW_IN_FLIGHT) {
      this.record(route, { kind: 'skipped' });
      return null;
    }
    this.shadowInFlight.set(route, used + 1);
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.shadowInFlight.set(route, (this.shadowInFlight.get(route) ?? 1) - 1);
    };
  }

  /**
   * Dispatch one request. The lease is read here, not at admission: shadow awaits the TS answer
   * in between, and the service may have gone meanwhile (review M1). Nothing here throws: every
   * outcome is the returned promise, so callers always release their slot and keep their TS
   * answer. The half-open breaker probe is reserved only here, once the request really goes.
   */
  private forward(
    route: string,
    verified: VerifiedRustPrincipal,
    input?: unknown,
    query = '',
    headers?: Record<string, string>
  ): Promise<RustReply> {
    const lease: RustLease | null = this.lease;
    if (!lease || this.stopped)
      return Promise.reject(Object.assign(new Error('Rust service down'), { unavailable: 'down' }));
    if (!this.breakers.allow(route))
      return Promise.reject(
        Object.assign(new Error('Rust route breaker'), { unavailable: 'breaker' })
      );
    this.inFlight++;
    let pending: Promise<RustReply>;
    try {
      pending = requestRust(
        lease.connection,
        route.replace('{*path}', 'read') + query,
        verified,
        this.active,
        input,
        'GET',
        // R1: a fed view carries its ids in headers and comes back as bytes, never parsed here.
        headers ? { headers, raw: true, cap: RUST_VIEW_CAP } : {}
      );
    } catch (error) {
      pending = Promise.reject(error);
    }
    return pending
      .then(
        (reply) => {
          this.breakers.success(route);
          this.replied(lease);
          return reply;
        },
        (error: unknown) => {
          const kind = rustFailureKind(error);
          if (this.breakers.failure(route))
            logger.warn('rust.service.breaker_open', 'Rust route breaker open for 30 s', {
              route,
              reason: kind,
            });
          this.transportFailed(lease, kind);
          throw Object.assign(error as object, { rustFailure: kind });
        }
      )
      .finally(() => {
        this.inFlight--;
      });
  }

  private failed(route: string, error: unknown): void {
    const unavailable = (error as { unavailable?: string } | undefined)?.unavailable;
    if (unavailable) {
      this.fallback(route, { kind: 'unavailable', reason: unavailable });
      return;
    }
    const kind =
      (error as { rustFailure?: string } | undefined)?.rustFailure ?? rustFailureKind(error);
    this.fallback(route, { kind: 'failed', reason: kind });
  }

  /** A non-200 reply: the service's named reason, or `unnamed`. */
  private declined(route: string, result: RustReply): void {
    this.fallback(route, { kind: 'declined', reason: result.decline ?? 'unnamed' });
  }

  private difference(route: string): void {
    this.stats.differences++;
    this.record(route, { kind: 'different' });
    // State changes and suppressed counts are logged by the supervisor.
  }

  /**
   * Shadow mode: TS already answered `reference`. The Rust reply is compared in the background by
   * serialized bytes (key order included); an unaccepted reply (`undefined`) is a logged fallback,
   * and a Rust answer where TS answered none is a difference. Equal answers record the pair of
   * timings: the TS path and the Rust path on the same request.
   */
  private shadowCompare(
    route: string,
    pending: Promise<RustReply>,
    reference: unknown,
    accepted: (body: unknown) => unknown,
    tsMs: number,
    rustStarted: number,
    release: () => void
  ): void {
    void pending
      .then((result) => {
        const rustMs = now() - rustStarted;
        this.stats.shadowed++;
        const value = result.status === 200 ? accepted(result.body) : undefined;
        if (value === undefined) {
          if (result.status === 200) this.fallback(route, { kind: 'failed', reason: 'refused' });
          else this.declined(route, result);
          return;
        }
        if (reference === undefined || JSON.stringify(reference) !== JSON.stringify(value))
          this.difference(route);
        else {
          this.record(route, { kind: 'equal' });
          this.pair(route, tsMs, rustMs);
        }
      })
      .catch((error: unknown) => {
        this.failed(route, error);
      })
      .finally(release);
  }

  /** Immutable reads stay with the existing collector/store owner. Rust assembles a candidate;
   * this transitional gate admits it only when it exactly equals the TS reference for that read.
   * No caller headers or request body enter the private input envelope.
   */
  async read(req: Request, route: string, expected: unknown, input: unknown): Promise<unknown> {
    const verified = this.forwardable(req, route);
    if (!verified) return expected;
    const release = this.mode === 'shadow' ? this.shadowSlot(route) : () => {};
    if (!release) return expected;
    const started = now();
    const pending = this.forward(route, verified, input);
    const admit = (result: RustReply): unknown => {
      if (this.mode === 'shadow') this.stats.shadowed++;
      if (result.status !== 200) {
        this.declined(route, result);
        return expected;
      }
      if (canonical(expected) !== canonical(result.body)) {
        this.difference(route);
        this.stats.fallbacks++;
        return expected;
      }
      // The TS reference was read before the bridge: only the Rust path is timed here.
      this.pair(route, null, now() - started);
      if (this.mode === 'shadow') {
        this.record(route, { kind: 'equal' });
        return expected;
      }
      this.stats.served++;
      this.record(route, { kind: 'served' });
      return orderedLike(result.body, expected);
    };
    if (this.mode === 'shadow') {
      void pending
        .then(admit)
        .catch((error: unknown) => {
          this.failed(route, error);
        })
        .finally(release);
      return expected;
    }
    try {
      return admit(await pending);
    } catch (error) {
      this.failed(route, error);
      return expected;
    }
  }

  /**
   * Compute in Rust first from immutable inputs the guarded TS handler already read; legacy
   * composition is lazy in on mode. An input the builder cannot carry exactly (null) never
   * leaves TS. Shadow serves TS first, then asks Rust and compares the serialized bytes, key
   * order included, in the background. Collectors, stores and writers remain the owners.
   */
  async compose(
    req: Request,
    route: string,
    input: unknown,
    legacy: () => unknown | Promise<unknown>,
    accepts: (body: unknown) => boolean
  ): Promise<unknown> {
    const verified = this.forwardable(req, route);
    if (!verified) return legacy();
    const release = this.mode === 'shadow' ? this.shadowSlot(route) : () => {};
    if (!release) return legacy();
    const inputStarted = now();
    let resolved: unknown;
    try {
      resolved = typeof input === 'function' ? (input as () => unknown)() : input;
    } catch {
      resolved = null;
    }
    const inputMs = now() - inputStarted;
    if (resolved === undefined || resolved === null) {
      release();
      this.fallback(route, { kind: 'unavailable', reason: 'input' });
      return legacy();
    }
    if (this.mode === 'shadow') {
      let reference: unknown;
      const tsStarted = now();
      try {
        reference = await legacy();
      } catch (error) {
        release();
        throw error;
      }
      // Wonder L5: the user waited for the envelope build before the TS answer, so the served
      // latency carries it; the Rust path already does (`rustStarted` below).
      const tsMs = inputMs + (now() - tsStarted);
      const rustStarted = now() - inputMs;
      this.shadowCompare(
        route,
        this.forward(route, verified, resolved),
        reference,
        (body) => (accepts(body) ? body : undefined),
        tsMs,
        rustStarted,
        release
      );
      return reference;
    }
    const rustStarted = now() - inputMs;
    try {
      const result = await this.forward(route, verified, resolved);
      if (result.status === 200 && accepts(result.body)) {
        this.stats.served++;
        this.record(route, { kind: 'served' });
        this.pair(route, null, now() - rustStarted);
        return result.body;
      }
      if (result.status === 200) this.fallback(route, { kind: 'failed', reason: 'refused' });
      else this.declined(route, result);
    } catch (error) {
      this.failed(route, error);
    }
    return legacy();
  }

  /**
   * A read Rust answers from the owner's existing file under its own boot root. `accept`
   * validates the reply and returns the body plus any effect TS still owns (a one-time folder
   * warning), applied only when Rust's answer is served. `legacy` runs the original handler once
   * and returns the 200 body it sent (undefined otherwise). Shadow runs legacy and compares; it
   * never applies the effect.
   */
  async owned(
    req: Request,
    res: Response,
    route: string,
    root: string,
    accept: (body: unknown) => { value: unknown; effect?: () => void } | null,
    legacy: () => Promise<unknown>
  ): Promise<void> {
    let answered = false;
    const value = await this.ownedValue(req, route, root, accept, () => {
      answered = true;
      return legacy();
    });
    if (!answered) res.json(value);
  }

  /**
   * `owned`'s read for a handler that uses the answer rather than sending it (Home's view reads
   * the desktop profiles through it, D7): the accepted Rust value, or what `legacy` returns (the
   * TS reader's value; it may throw). The same gate, root check, shadow comparison and effect.
   */
  async ownedValue(
    req: Request,
    route: string,
    root: string,
    accept: (body: unknown) => { value: unknown; effect?: () => void } | null,
    legacy: () => Promise<unknown>
  ): Promise<unknown> {
    let verified = this.forwardable(req, route);
    // Rust reads its own canonical boot root; a symlinked spelling of it names the same boot
    // (Wonder L4). Another folder (whose one-time warning TS logs) stays with the TS reader.
    if (verified && this.lease?.connection.ccsDir !== canonicalRoot(root)) {
      this.fallback(route, { kind: 'unavailable', reason: 'root' });
      verified = null;
    }
    if (!verified) return legacy();
    if (this.mode === 'shadow') {
      const release = this.shadowSlot(route);
      if (!release) return legacy();
      let reference: unknown;
      const tsStarted = now();
      try {
        reference = await legacy();
      } catch (error) {
        release();
        throw error;
      }
      const tsMs = now() - tsStarted;
      const rustStarted = now();
      this.shadowCompare(
        route,
        this.forward(route, verified),
        reference,
        (body) => accept(body)?.value,
        tsMs,
        rustStarted,
        release
      );
      return reference;
    }
    const rustStarted = now();
    try {
      const result = await this.forward(route, verified);
      const accepted = result.status === 200 ? accept(result.body) : null;
      if (accepted) {
        this.stats.served++;
        this.record(route, { kind: 'served' });
        this.pair(route, null, now() - rustStarted);
        try {
          accepted.effect?.();
        } catch {
          /* Logging is best effort. */
        }
        return accepted.value;
      }
      if (result.status === 200) this.fallback(route, { kind: 'failed', reason: 'refused' });
      else this.declined(route, result);
    } catch (error) {
      this.failed(route, error);
    }
    return legacy();
  }

  /**
   * R1: the boot a fed source belongs to. Feeds are held per service boot, so a new boot (a restart, an
   * upgrade) starts with none and the front door pushes again. Null without a lease, or when the service
   * reads another root than `scope` (the TS readers answer for another root).
   */
  feedBoot(scope: string): string | null {
    const lease = this.lease;
    return lease && !this.stopped && lease.connection.ccsDir === canonicalRoot(scope)
      ? lease.connection.bootId
      : null;
  }

  /**
   * R1: push one generation (data and facts already JSON text) to the current boot. The held generation the
   * service names after the push, or null when the push did not reach it (a transport failure also tells the
   * supervisor, as any request's does).
   */
  async pushFeed(
    bootId: string,
    source: string,
    generation: string,
    data: string,
    facts: string
  ): Promise<string | null> {
    const lease = this.lease;
    if (!lease || this.stopped || lease.connection.bootId !== bootId) return null;
    try {
      const reply = await pushRustFeedText(
        lease.connection,
        source,
        generation,
        data,
        facts,
        this.active
      );
      this.replied(lease);
      const held = (reply.body as { generation?: unknown } | null)?.generation;
      return reply.status === 200 && typeof held === 'string' ? held : null;
    } catch (error) {
      this.transportFailed(lease, rustFailureKind(error));
      return null;
    }
  }

  /**
   * R1: a view Rust answers from fed generations. The request has no body: `headers` carry the generation ids,
   * the clock and the page's context. The finished bytes, ETag and view version come back unparsed. Anything
   * else is counted and the caller's TS handler answers: a named decline returns its word (`stale` tells the
   * caller to push again), a failure or another reply null. The same admission as every read: an admitted
   * principal, a lease, a free slot, a closed breaker. Serves and named declines also count in the route's
   * fed-only counters (`fedServed`, `fedDeclined`), apart from the envelope read's successes on the same route.
   */
  async fedView(
    req: Request,
    route: string,
    headers: Record<string, string>,
    view: string
  ): Promise<RustViewBytes | string | null> {
    if (this.mode !== 'on') return null;
    const verified = this.forwardable(req, route);
    if (!verified) return null;
    const started = now();
    try {
      const result = await this.forward(route, verified, undefined, '', headers);
      const bytes = rustViewBytes(result, view);
      if (bytes) {
        this.stats.served++;
        this.record(route, { kind: 'fedServed' });
        this.pair(route, null, now() - started);
        return bytes;
      }
      if (result.status === 200) this.fallback(route, { kind: 'failed', reason: 'refused' });
      else {
        const reason = result.decline ?? 'unnamed';
        this.fallback(route, { kind: 'fedDeclined', reason });
        return reason;
      }
    } catch (error) {
      this.failed(route, error);
    }
    return null;
  }

  /** R1: a fed request the front door itself declined before asking Rust (its reason is counted). */
  fedDeclined(route: string, reason: string): void {
    this.fallback(route, { kind: 'unavailable', reason });
  }

  /**
   * R4: an internal GET the front door's background owners read (the owned Status snapshots,
   * the Scoreboard input the facts are computed over). The reserved front-door principal, the
   * boot secret, no caller credentials. Null when the service is absent or declines; the
   * transport failure tells the supervisor, as any request's does.
   */
  async internalJson(path: string): Promise<unknown | null> {
    const lease = this.lease;
    if (!lease || this.stopped || this.mode !== 'on') return null;
    try {
      const result = await requestRust(lease.connection, path, FRONT_DOOR_PRINCIPAL, this.active);
      this.replied(lease);
      return result.status === 200 ? result.body : null;
    } catch (error) {
      this.transportFailed(lease, rustFailureKind(error));
      return null;
    }
  }

  /**
   * G16b: one internal write (a forwarded send) with its own deadline. `unreached` only when the
   * request never reached the service (no lease, or the connection was refused): the caller may then
   * do the work itself. Any other failure is `uncertain`: the service may have done it.
   */
  async internalSend(
    path: string,
    body: unknown,
    deadlineMs: number
  ): Promise<
    | { kind: 'reply'; status: number; body: unknown; decline: string | null }
    | { kind: 'unreached' }
    | { kind: 'uncertain' }
  > {
    const lease = this.lease;
    if (!lease || this.stopped || this.mode !== 'on') return { kind: 'unreached' };
    try {
      const result = await requestRust(
        lease.connection,
        path,
        FRONT_DOOR_PRINCIPAL,
        this.active,
        body,
        'PUT',
        { deadlineMs, cap: 2 * 1024 * 1024 }
      );
      this.replied(lease);
      return { kind: 'reply', status: result.status, body: result.body, decline: result.decline };
    } catch (error) {
      const kind = rustFailureKind(error);
      this.transportFailed(lease, kind);
      return kind === 'connect' ? { kind: 'unreached' } : { kind: 'uncertain' };
    }
  }

  async respond(
    req: Request,
    res: Response,
    route: string,
    tsAnswer: () => unknown
  ): Promise<void> {
    const entry =
      routes.find(
        (r) =>
          (r.method === req.method || (req.method === 'HEAD' && r.method === 'GET')) &&
          r.path === route
      ) ?? routes[routes.length - 1];
    if (this.mode === 'off' || entry?.owner !== 'rust' || this.stopped) {
      res.json(tsAnswer());
      return;
    }
    const verified = this.forwardable(req, route);
    if (!verified) {
      res.json(tsAnswer());
      return;
    }
    const query = req.originalUrl.includes('?')
      ? req.originalUrl.slice(req.originalUrl.indexOf('?'))
      : '';
    if (this.mode === 'shadow') {
      const release = this.shadowSlot(route);
      const tsStarted = now();
      const body = tsAnswer();
      const tsMs = now() - tsStarted;
      res.json(body);
      if (!release) return;
      const rustStarted = now();
      void this.forward(route, verified, undefined, query)
        .then((result) => {
          const rustMs = now() - rustStarted;
          this.stats.shadowed++;
          if (result.status !== 200) {
            this.declined(route, result);
            return;
          }
          if (canonical(body) !== canonical(result.body)) {
            this.difference(route);
          } else {
            this.record(route, { kind: 'equal' });
            this.pair(route, tsMs, rustMs);
          }
        })
        .catch((error: unknown) => {
          this.failed(route, error);
        })
        .finally(release);
      return;
    }
    const rustStarted = now();
    try {
      const result = await this.forward(route, verified, undefined, query);
      if (validReply(route, result)) {
        this.stats.served++;
        this.record(route, { kind: 'served' });
        this.pair(route, null, now() - rustStarted);
        res.json(result.body);
        return;
      }
      if (result.status === 200) this.fallback(route, { kind: 'failed', reason: 'refused' });
      else this.declined(route, result);
    } catch (error) {
      /* Never send a partial or unhealthy service response. */
      this.failed(route, error);
    }
    res.json(tsAnswer());
  }
}

/** Route handlers call this only after their original TS policy and validation middleware. */
export async function respondWithRust(
  req: Request,
  res: Response,
  route: string,
  answer: () => unknown
): Promise<void> {
  const service: unknown = res.locals.rustService;
  if (service instanceof RustService) await service.respond(req, res, route, answer);
  else res.json(answer());
}

/** `/api/diag?bridge=1`: the bridge's state, counters and paired timings (L1). */
export function rustBridgeDiagnostics(res: Response): Record<string, unknown> {
  const service: unknown = res.locals.rustService;
  return service instanceof RustService ? service.diagnostics() : { mode: 'off' };
}

/** Called only after the original endpoint guards and one immutable source read. */
export async function readWithRust(
  req: Request,
  res: Response,
  route: string,
  expected: unknown,
  input: unknown = { snapshot: expected }
): Promise<unknown> {
  const service: unknown = res.locals.rustService;
  return service instanceof RustService ? service.read(req, route, expected, input) : expected;
}

/** Off mode retains the original file streamer without an additional read/copy. */
export function rustReadsEnabled(res: Response): boolean {
  const service: unknown = res.locals.rustService;
  return service instanceof RustService && service.mode === 'shadow';
}

/** Guarded route adapters use this with immutable reader inputs and a lazy legacy composer. */
export async function composeWithRust(
  req: Request,
  res: Response,
  route: string,
  input: unknown,
  legacy: () => unknown | Promise<unknown>,
  accepts: (body: unknown) => boolean
): Promise<unknown> {
  const service: unknown = res.locals.rustService;
  return service instanceof RustService
    ? service.compose(req, route, input, legacy, accepts)
    : legacy();
}

/** Guarded file-read routes: Rust reads the owner's file; the original handler is the fallback. */
export async function readOwnedWithRust(
  req: Request,
  res: Response,
  route: string,
  root: string,
  accept: (body: unknown) => { value: unknown; effect?: () => void } | null,
  legacy: () => Promise<unknown>
): Promise<void> {
  const service: unknown = res.locals.rustService;
  if (service instanceof RustService) await service.owned(req, res, route, root, accept, legacy);
  else await legacy();
}

/** `readOwnedWithRust` for a handler that uses the answer itself: the value, not a response. */
export async function readOwnedValueWithRust(
  req: Request,
  res: Response,
  route: string,
  root: string,
  accept: (body: unknown) => { value: unknown; effect?: () => void } | null,
  legacy: () => Promise<unknown>
): Promise<unknown> {
  const service: unknown = res.locals.rustService;
  return service instanceof RustService
    ? service.ownedValue(req, route, root, accept, legacy)
    : legacy();
}
