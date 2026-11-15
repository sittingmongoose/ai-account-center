/** M3 strangler: existing endpoint guards run before any verified principal is constructed. */
import type { Request, Response } from 'express';
import type http from 'http';
import path from 'path';
import { createHash } from 'crypto';
import { createLogger } from '../services/logging';
import { getCcsDir } from '../utils/config-manager';
import { authKind, requestDevice } from './middleware/request-auth';
import { isAccountRefreshSettings } from './services/account-refresh-settings';
import routes from './rust-service-routes.json';
import {
  acquireRustService,
  PUBLIC_HEALTH_PRINCIPAL,
  type RustLease,
} from './rust-service-lifecycle';
import { requestRust, type RustReply, type VerifiedRustPrincipal } from './rust-service-transport';
export type { VerifiedRustPrincipal } from './rust-service-transport';

export type RustServiceMode = 'off' | 'shadow' | 'on';
const logger = createLogger('rust-service');

/** Honest TS fallback for the new signed-in diagnostics endpoint. */
export function skeletonDiagnostics() {
  return {
    jobs: [],
    scheduler: { status: 'unavailable', lastSuccessAt: null, cause: 'jobs_not_migrated' },
    store: { status: 'unavailable', lastSuccessAt: null, cause: 'store_owned_by_ts' },
    computers: [],
  };
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
  if (value && typeof value === 'object')
    return (
      '{' +
      Object.entries(value)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([k, v]) => JSON.stringify(k) + ':' + canonical(v))
        .join(',') +
      '}'
    );
  return JSON.stringify(value);
}

function principal(req: Request, route: string): VerifiedRustPrincipal | null {
  // No caller-provided principal/role/secret header is inspected or copied.
  if (authKind(req) === 'session' && req.sessionID)
    return {
      kind: 'session',
      id: createHash('sha256').update(req.sessionID).digest('hex'),
      role: 'admin',
    };
  const device = requestDevice(req);
  if (device) return { kind: 'device', id: device.deviceId, role: 'device' };
  return route === '/api/health' ? PUBLIC_HEALTH_PRINCIPAL : null;
}

function validReply(route: string, result: RustReply): boolean {
  if (result.status !== 200) return false;
  if (route === '/api/health') return canonical(result.body) === '{"status":"ok"}';
  if (route === '/api/accounts/settings') return isAccountRefreshSettings(result.body);
  return route === '/api/diag' && canonical(result.body) === canonical(skeletonDiagnostics());
}

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

  /** Immutable reads stay with the existing collector/store owner. Rust assembles a candidate;
   * this transitional gate admits it only when it exactly equals the TS reference for that read.
   * No caller headers or request body enter the private input envelope.
   */
  async read(req: Request, route: string, expected: unknown, input: unknown): Promise<unknown> {
    const entry = routes.find((r) => r.path === route && r.method === 'GET');
    if (
      this.mode === 'off' ||
      entry?.owner !== 'rust' ||
      this.stopped ||
      !['GET', 'HEAD'].includes(req.method)
    )
      return expected;
    const verified = principal(req, route);
    if (!this.lease || !verified || this.inFlight >= 16) {
      this.fallback();
      return expected;
    }
    this.inFlight++;
    const pending = requestRust(
      this.lease.connection,
      route.replace('{*path}', 'read'),
      verified,
      this.active,
      input
    ).finally(() => {
      this.inFlight--;
    });
    const admit = (result: RustReply): unknown => {
      if (this.mode === 'shadow') this.stats.shadowed++;
      if (result.status !== 200) {
        this.fallback();
        return expected;
      }
      if (canonical(expected) !== canonical(result.body)) {
        this.stats.differences++;
        logger.warn(
          'rust.service.shadow_difference',
          'Rust read candidate differs; TypeScript fallback active',
          { route, status: result.status }
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

  /** Compute in Rust first; legacy composition is lazy in on mode. Collectors remain owners. */
  async compose(
    req: Request,
    route: string,
    input: unknown,
    legacy: () => unknown | Promise<unknown>,
    accepts: (body: unknown) => boolean
  ): Promise<unknown> {
    const owned = routes.some((r) => r.path === route && r.method === 'GET' && r.owner === 'rust');
    if (this.mode === 'off' || !owned || this.stopped || !['GET', 'HEAD'].includes(req.method))
      return legacy();
    const verified = principal(req, route);
    if (!this.lease || !verified || this.inFlight >= 16) {
      this.fallback();
      return legacy();
    }
    let resolved: unknown;
    try {
      resolved = typeof input === 'function' ? (input as () => unknown)() : input;
    } catch {
      this.fallback();
      return legacy();
    }
    this.inFlight++;
    const pending = requestRust(
      this.lease.connection,
      route.replace('{*path}', 'read'),
      verified,
      this.active,
      resolved
    ).finally(() => {
      this.inFlight--;
    });
    if (this.mode === 'shadow') {
      let reference: unknown;
      try {
        reference = await legacy();
      } catch (error) {
        void pending.catch(() => undefined);
        throw error;
      }
      void pending
        .then((result) => {
          this.stats.shadowed++;
          if (result.status !== 200 || !accepts(result.body)) {
            this.fallback();
            return;
          }
          if (canonical(reference) !== canonical(result.body)) {
            this.stats.differences++;
            logger.warn('rust.service.shadow_difference', 'Rust read response differs', {
              route,
              status: result.status,
            });
          }
        })
        .catch(() => {
          this.fallback();
        });
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

/** Match the legacy serializer's property order without replacing any candidate values. */
function orderedLike(candidate: unknown, reference: unknown): unknown {
  if (Array.isArray(reference) && Array.isArray(candidate))
    return candidate.map((value, index) => orderedLike(value, reference[index]));
  if (reference && candidate && typeof reference === 'object' && typeof candidate === 'object') {
    const values = candidate as Record<string, unknown>;
    return Object.fromEntries(
      Object.entries(reference).map(([key, value]) => [key, orderedLike(values[key], value)])
    );
  }
  return candidate;
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
