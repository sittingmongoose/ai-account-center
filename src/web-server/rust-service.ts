/** M3 strangler: existing endpoint guards run before any verified principal is constructed. */
import type { Request, Response } from 'express';
import type http from 'http';
import path from 'path';
import { createLogger } from '../services/logging';
import { getCcsDir } from '../utils/config-manager';
import routes from './rust-service-routes.json';
import { acquireRustService, type RustLease } from './rust-service-lifecycle';
import { requestRust, type RustReply, type VerifiedRustPrincipal } from './rust-service-transport';
import { canonical, orderedLike, principal, validReply } from './rust-service-values';
export type { VerifiedRustPrincipal } from './rust-service-transport';
export { skeletonDiagnostics } from './rust-service-values';

export type RustServiceMode = 'off' | 'shadow' | 'on';
const logger = createLogger('rust-service');

export class RustService {
  readonly mode: RustServiceMode;
  /** Counts actual replies; contracts assert this to prevent an accidental all-TS pass. */
  readonly stats = { served: 0, shadowed: 0, differences: 0, fallbacks: 0 };
  private lease: RustLease | null = null;
  private active = new Set<http.ClientRequest>();
  private inFlight = 0;
  private stopped = false;
  private fallbackLogged = false;

  constructor(mode: string = process.env.AAC_RUST_SERVICE ?? 'off') {
    this.mode = mode === 'on' || mode === 'shadow' ? mode : 'off';
  }

  async start(
    binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(__dirname, '../native/aac-http')
  ): Promise<void> {
    if (this.mode === 'off') return;
    try {
      this.lease = await acquireRustService(getCcsDir(), binary, this.active);
    } catch {
      /* Safe TS fallback. */
    }
    if (!this.lease) this.fallback();
    else logger.info('rust.service.ready', 'Private Rust service ready', { mode: this.mode });
  }

  stop(): void {
    this.stopped = true;
    for (const request of this.active) request.destroy();
    this.lease?.close();
    this.lease = null;
  }

  private fallback(): void {
    this.stats.fallbacks++;
    if (this.fallbackLogged) return;
    this.fallbackLogged = true;
    logger.warn(
      'rust.service.fallback',
      'Rust service absent, busy or unhealthy; TypeScript fallback active',
      { mode: this.mode }
    );
  }

  /**
   * The shared forwarding gate of every read method: an owned GET/HEAD route, a running service,
   * a verified principal and a free in-flight slot. Anything else is a logged TS fallback.
   */
  private forwardable(req: Request, route: string): VerifiedRustPrincipal | null {
    const owned = routes.some((r) => r.path === route && r.method === 'GET' && r.owner === 'rust');
    if (this.mode === 'off' || !owned || this.stopped || !['GET', 'HEAD'].includes(req.method))
      return null;
    const verified = principal(req, route);
    if (!this.lease || !verified || this.inFlight >= 16) {
      this.fallback();
      return null;
    }
    return verified;
  }

  private forward(route: string, verified: VerifiedRustPrincipal, input?: unknown) {
    const lease = this.lease as RustLease;
    this.inFlight++;
    return requestRust(
      lease.connection,
      route.replace('{*path}', 'read'),
      verified,
      this.active,
      input
    ).finally(() => {
      this.inFlight--;
    });
  }

  private difference(route: string, status: number, message = 'Rust read response differs'): void {
    this.stats.differences++;
    logger.warn('rust.service.shadow_difference', message, { route, status });
  }

  /**
   * Shadow mode: TS already answered `reference`. The Rust reply is compared in the background by
   * serialized bytes (key order included); an unaccepted reply (`undefined`) is a logged fallback,
   * and a Rust answer where TS answered none is a difference.
   */
  private shadowCompare(
    route: string,
    pending: Promise<RustReply>,
    reference: unknown,
    accepted: (body: unknown) => unknown
  ): void {
    void pending
      .then((result) => {
        this.stats.shadowed++;
        const value = result.status === 200 ? accepted(result.body) : undefined;
        if (value === undefined) {
          this.fallback();
          return;
        }
        if (reference === undefined || JSON.stringify(reference) !== JSON.stringify(value))
          this.difference(route, result.status);
      })
      .catch(() => {
        this.fallback();
      });
  }

  /** Immutable reads stay with the existing collector/store owner. Rust assembles a candidate;
   * this transitional gate admits it only when it exactly equals the TS reference for that read.
   * No caller headers or request body enter the private input envelope.
   */
  async read(req: Request, route: string, expected: unknown, input: unknown): Promise<unknown> {
    const verified = this.forwardable(req, route);
    if (!verified) return expected;
    const pending = this.forward(route, verified, input);
    const admit = (result: RustReply): unknown => {
      if (this.mode === 'shadow') this.stats.shadowed++;
      if (result.status !== 200) {
        this.fallback();
        return expected;
      }
      if (canonical(expected) !== canonical(result.body)) {
        this.difference(
          route,
          result.status,
          'Rust read candidate differs; TypeScript fallback active'
        );
        this.fallback();
        return expected;
      }
      this.fallbackLogged = false;
      if (this.mode === 'shadow') return expected;
      this.stats.served++;
      return orderedLike(result.body, expected);
    };
    if (this.mode === 'shadow') {
      void pending.then(admit).catch(() => {
        this.fallback();
      });
      return expected;
    }
    try {
      return admit(await pending);
    } catch {
      this.fallback();
      return expected;
    }
  }

  /**
   * Compute in Rust first from immutable inputs the guarded TS handler already read; legacy
   * composition is lazy in on mode. An input the builder cannot carry exactly (null) never
   * leaves TS. Shadow serves TS and compares the serialized bytes, key order included, in the
   * background. Collectors, stores and writers remain the owners.
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
    let resolved: unknown;
    try {
      resolved = typeof input === 'function' ? (input as () => unknown)() : input;
    } catch {
      resolved = null;
    }
    if (resolved === undefined || resolved === null) {
      this.fallback();
      return legacy();
    }
    const pending = this.forward(route, verified, resolved);
    if (this.mode === 'shadow') {
      let reference: unknown;
      try {
        reference = await legacy();
      } catch (error) {
        void pending.catch(() => undefined);
        throw error;
      }
      this.shadowCompare(route, pending, reference, (body) => (accepts(body) ? body : undefined));
      return reference;
    }
    try {
      const result = await pending;
      if (result.status === 200 && accepts(result.body)) {
        this.stats.served++;
        this.fallbackLogged = false;
        return result.body;
      }
    } catch {
      /* Bounded, logged legacy fallback. */
    }
    this.fallback();
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
    let verified = this.forwardable(req, route);
    // Rust reads its own canonical boot root. Another or symlinked folder (whose one-time
    // warning TS logs) stays with the TS reader.
    if (verified && this.lease?.connection.ccsDir !== root) {
      this.fallback();
      verified = null;
    }
    if (!verified) {
      await legacy();
      return;
    }
    const pending = this.forward(route, verified);
    if (this.mode === 'shadow') {
      let reference: unknown;
      try {
        reference = await legacy();
      } catch (error) {
        void pending.catch(() => undefined);
        throw error;
      }
      this.shadowCompare(route, pending, reference, (body) => accept(body)?.value);
      return;
    }
    try {
      const result = await pending;
      const accepted = result.status === 200 ? accept(result.body) : null;
      if (accepted) {
        this.stats.served++;
        this.fallbackLogged = false;
        try {
          accepted.effect?.();
        } catch {
          /* Logging is best effort. */
        }
        res.json(accepted.value);
        return;
      }
    } catch {
      /* Bounded, logged legacy fallback. */
    }
    this.fallback();
    await legacy();
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
    const verified = principal(req, route);
    if (!this.lease || !verified || this.inFlight >= 16) {
      this.fallback();
      res.json(tsAnswer());
      return;
    }
    const lease = this.lease;
    const query = req.originalUrl.includes('?')
      ? req.originalUrl.slice(req.originalUrl.indexOf('?'))
      : '';
    this.inFlight++;
    const rustAnswer = requestRust(lease.connection, route + query, verified, this.active).finally(
      () => {
        this.inFlight--;
      }
    );
    if (this.mode === 'shadow') {
      const body = tsAnswer();
      res.json(body);
      void rustAnswer
        .then((result) => {
          this.stats.shadowed++;
          if (result.status !== 200) {
            this.fallback();
            return;
          }
          if (canonical(body) !== canonical(result.body)) {
            this.stats.differences++;
            logger.warn('rust.service.shadow_difference', 'Rust shadow response differs', {
              route,
              status: result.status,
            });
          }
        })
        .catch(() => {
          this.fallback();
        });
      return;
    }
    try {
      const result = await rustAnswer;
      if (validReply(route, result)) {
        this.stats.served++;
        this.fallbackLogged = false;
        res.json(result.body);
        return;
      }
    } catch {
      /* Never send a partial or unhealthy service response. */
    }
    this.fallback();
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
