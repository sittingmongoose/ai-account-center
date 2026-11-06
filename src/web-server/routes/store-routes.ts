/**
 * Read-only history store routes (docs/store.md 8.1): `/api/store/status` and `/api/usage/*`.
 * A signed-in session (from the dashboard origin) or a paired device token may read them; the
 * device scope admits only these GET routes. Errors are fixed sentences that never echo input.
 */
import type { Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import {
  aacStoreState,
  getAacStore,
  type AacStore,
  type SeriesRef,
} from '../../store/store-service';

const HOUR = 3_600_000;
const DAY = 86_400_000;
const SPANS: Record<string, number> = {
  '1h': HOUR,
  '6h': 6 * HOUR,
  '24h': DAY,
  '7d': 7 * DAY,
  '30d': 30 * DAY,
  '90d': 90 * DAY,
};
const MAX_RANGE_MS = 3650 * DAY;

const ERRORS = {
  auth_required: 'Authentication required',
  origin_required: 'The history store requires the dashboard origin.',
  invalid_query: 'The query is not valid for this route.',
  store_unavailable: 'The history store is not available on this computer.',
  store_failed: 'The history store could not answer.',
} as const;

type ErrorCode = keyof typeof ERRORS;

export interface StoreRouterDeps {
  store?: () => AacStore | null;
  now?: () => number;
}

function fail(res: Response, status: number, code: ErrorCode): void {
  res.status(status).json({ error: ERRORS[code], code });
}

/** Session from the dashboard origin, or a device token: true when the request may read. */
function allowed(req: Request, res: Response): boolean {
  res.setHeader('Cache-Control', 'no-store');
  const kind = authKind(req);
  if (kind === null) {
    fail(res, 401, 'auth_required');
    return false;
  }
  if (
    kind === 'session' &&
    (!isDashboardWebSocketOriginAllowed(req) || req.headers['sec-fetch-site'] === 'cross-site')
  ) {
    fail(res, 403, 'origin_required');
    return false;
  }
  return true;
}

/** The query as plain strings, or null when it holds a key outside `keys` or a repeated key. */
function query(req: Request, keys: readonly string[]): Record<string, string> | null {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(req.query)) {
    if (!keys.includes(key) || typeof value !== 'string' || value.length > 200) return null;
    out[key] = value;
  }
  return out;
}

function time(value: string | undefined, fallback: number): number | null {
  if (value === undefined) return fallback;
  if (!/^\d{4}-\d{2}-\d{2}T/.test(value)) return null;
  const epoch = Date.parse(value);
  return Number.isFinite(epoch) ? epoch : null;
}

const ACCOUNT = /^[a-zA-Z0-9][a-zA-Z0-9:@._+-]{0,159}$/;
const WINDOW = /^[A-Za-z0-9_.:-]{1,64}$/;
const PROVIDER = /^[a-z][a-z0-9-]{0,31}$/;

export function createStoreRouter(deps: StoreRouterDeps = {}): Router {
  const router = createApiRouter();
  const store = deps.store ?? getAacStore;
  router.get('/status', async (req, res): Promise<void> => {
    if (!allowed(req, res)) return;
    if (!query(req, []) || Object.keys(req.query).length > 0)
      return fail(res, 400, 'invalid_query');
    const current = store();
    if (!current) {
      res.json({ state: 'unavailable', reason: aacStoreState().reason ?? 'missing' });
      return;
    }
    try {
      res.json(await current.status());
    } catch {
      fail(res, 503, 'store_failed');
    }
  });
  return router;
}

interface SeriesFilter {
  accountIds: string[] | null;
  provider: string | null;
  window: string | null;
}

function seriesFilter(q: Record<string, string>): SeriesFilter | null {
  const account = q.account ?? 'all';
  const window = q.window ?? null;
  const provider = q.provider ?? null;
  if (account !== 'all' && !ACCOUNT.test(account)) return null;
  if (window !== null && !WINDOW.test(window)) return null;
  if (provider !== null && !PROVIDER.test(provider)) return null;
  return { accountIds: account === 'all' ? null : [account], provider, window };
}

export function createUsageRouter(deps: StoreRouterDeps = {}): Router {
  const router = createApiRouter();
  const store = deps.store ?? getAacStore;
  const now = deps.now ?? Date.now;

  router.get('/history', async (req, res): Promise<void> => {
    if (!allowed(req, res)) return;
    const q = query(req, ['account', 'window', 'provider', 'from', 'to', 'resolution']);
    const filter = q ? seriesFilter(q) : null;
    const at = now();
    const to = q ? time(q.to, at) : null;
    const from = q && to !== null ? time(q.from, to - 7 * DAY) : null;
    const resolution = q?.resolution ?? 'hour';
    if (
      !q ||
      !filter ||
      from === null ||
      to === null ||
      from > to ||
      to - from > MAX_RANGE_MS ||
      !['raw', 'hour', 'day'].includes(resolution)
    )
      return fail(res, 400, 'invalid_query');
    const current = store();
    if (!current) return fail(res, 503, 'store_unavailable');
    try {
      const points = await current.seriesPoints({
        fromMs: from,
        toMs: to,
        resolution: resolution as 'raw' | 'hour' | 'day',
        ...(filter.accountIds ? { accountIds: filter.accountIds } : {}),
        ...(filter.provider ? { provider: filter.provider } : {}),
        ...(filter.window ? { windowKey: filter.window } : {}),
      });
      res.json({ from: new Date(from).toISOString(), to: new Date(to).toISOString(), ...points });
    } catch {
      fail(res, 503, 'store_failed');
    }
  });

  router.get('/at', async (req, res): Promise<void> => {
    if (!allowed(req, res)) return;
    const q = query(req, ['t']);
    const t = q ? time(q.t, now()) : null;
    if (!q || t === null || t < 0) return fail(res, 400, 'invalid_query');
    const current = store();
    if (!current) return fail(res, 503, 'store_unavailable');
    try {
      res.json(await current.usageAt(t));
    } catch {
      fail(res, 503, 'store_failed');
    }
  });

  router.get('/burn', async (req, res): Promise<void> => {
    if (!allowed(req, res)) return;
    const q = query(req, ['account', 'window', 'provider', 'span']);
    const filter = q ? seriesFilter(q) : null;
    const spanNames = q?.span ? q.span.split(',') : Object.keys(SPANS);
    if (!q || !filter || spanNames.some((name) => !(name in SPANS)))
      return fail(res, 400, 'invalid_query');
    const current = store();
    if (!current) return fail(res, 503, 'store_unavailable');
    try {
      const at = now();
      const longest = Math.max(...spanNames.map((name) => SPANS[name]));
      // Which series exist: one hourly pass over the longest span.
      const points = await current.seriesPoints({
        fromMs: at - longest,
        toMs: at,
        resolution: 'hour',
        ...(filter.accountIds ? { accountIds: filter.accountIds } : {}),
        ...(filter.provider ? { provider: filter.provider } : {}),
        ...(filter.window ? { windowKey: filter.window } : {}),
      });
      const series = points.series;
      const refs: SeriesRef[] = series.map((s) => ({
        identity: s.identity,
        window: s.window.identity,
      }));
      const [burn, periods] = await Promise.all([
        current.burnLong(
          refs,
          spanNames.map((name) => SPANS[name]),
          at
        ),
        current.periods(refs, at - longest, at),
      ]);
      res.json({
        at: new Date(at).toISOString(),
        rule: 'Points used per hour across resets, from hourly rollups. A rate under 50% coverage is shown with its coverage only.',
        series: series.map((s, index) => ({
          accountId: s.accountId,
          provider: s.provider,
          plan: s.plan,
          window: { key: s.window.key, label: s.window.label, pool: s.window.poolLabel },
          spans: burn[index].spans.map((span, i) => ({
            span: spanNames[i],
            consumed: span.consumed,
            rate: span.coverage >= 0.5 ? span.rate : null,
            coverage: span.coverage,
          })),
          periods: periods[index].periods,
        })),
      });
    } catch {
      fail(res, 503, 'store_failed');
    }
  });

  return router;
}

export const storeRoutes = createStoreRouter();
export const usageRoutes = createUsageRouter();
