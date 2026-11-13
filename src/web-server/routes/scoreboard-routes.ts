import { sendRevalidatedView } from './revalidated-view';
import type { Request, Router } from 'express';
import { createApiRouter } from './api-router';
import { sendError } from './account-route-guards';
import { requireBrowserSession } from './status-routes';
import {
  ALL_RUNS,
  getScoreboardService,
  type ScoreboardService,
} from '../scoreboard/scoreboard-service';
import type { ScoreboardFilters } from '../scoreboard/scoreboard-view';
import { isCalendarDate } from '../services/account-analytics-range';

/** The only query parameters `GET /api/scoreboard` takes. */
const FILTER_KEYS = new Set(['from', 'to', 'source', 'taskType']);
const TASK_TYPE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

type Parsed = { ok: true; filters: ScoreboardFilters } | { ok: false; code: string; error: string };

/**
 * The server-side filters (Scoreboard v2): `from` and `to` are local days (`YYYY-MM-DD`, `from`
 * not after `to`), `source` is all, real or benchmark, `taskType` a catalog-shaped id. Each once.
 */
export function parseScoreboardQuery(req: Request): Parsed {
  const url = new URL(req.originalUrl, 'http://localhost');
  const keys = [...url.searchParams.keys()];
  if (keys.some((key) => !FILTER_KEYS.has(key)))
    return {
      ok: false,
      code: 'unexpected_query',
      error: 'This request takes only from, to, source and taskType.',
    };
  if (new Set(keys).size !== keys.length)
    return { ok: false, code: 'invalid_filter', error: 'Each filter may be given once.' };
  const get = (key: string) => url.searchParams.get(key);
  const from = get('from');
  const to = get('to');
  const source = get('source') ?? 'all';
  const taskType = get('taskType');
  if ((from !== null && !isCalendarDate(from)) || (to !== null && !isCalendarDate(to)))
    return { ok: false, code: 'invalid_filter', error: 'Dates are days written YYYY-MM-DD.' };
  if (from !== null && to !== null && from > to)
    return { ok: false, code: 'invalid_filter', error: 'The first day is after the last day.' };
  if (source !== 'all' && source !== 'real' && source !== 'benchmark')
    return { ok: false, code: 'invalid_filter', error: 'Source is all, real or benchmark.' };
  if (taskType !== null && !TASK_TYPE.test(taskType))
    return { ok: false, code: 'invalid_filter', error: 'The task type is not a task type id.' };
  return { ok: true, filters: { ...ALL_RUNS, from, to, source, taskType } };
}

/**
 * The Model Scoreboard API (docs/scoreboard.md): `GET /api/scoreboard`, the whole view, filtered
 * by the optional `from`, `to`, `source` and `taskType`. A browser session only, never cached.
 * Errors are `{ error, code }` with fixed sentences.
 */
export function createScoreboardRouter(
  service: () => ScoreboardService = getScoreboardService
): Router {
  const router = createApiRouter();
  router.use(requireBrowserSession);

  router.get('/', async (req, res) => {
    const parsed = parseScoreboardQuery(req);
    if (!parsed.ok) {
      sendError(res, 400, parsed.code, parsed.error);
      return;
    }
    try {
      sendRevalidatedView(res, await service().view(parsed.filters));
    } catch {
      sendError(res, 500, 'scoreboard_failed', 'The scoreboard could not be built.');
    }
  });

  return router;
}

export default createScoreboardRouter();
