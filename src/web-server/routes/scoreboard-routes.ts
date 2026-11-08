import type { Router } from 'express';
import { createApiRouter } from './api-router';
import { sendError } from './account-route-guards';
import { requireBrowserSession } from './status-routes';
import { getScoreboardService, type ScoreboardService } from '../scoreboard/scoreboard-service';

/**
 * The Model Scoreboard API (docs/scoreboard.md): `GET /api/scoreboard`, the whole view. A browser
 * session only, never cached, no query string. Errors are `{ error, code }` with fixed sentences.
 */
export function createScoreboardRouter(
  service: () => ScoreboardService = getScoreboardService
): Router {
  const router = createApiRouter();
  router.use(requireBrowserSession);

  router.get('/', async (req, res) => {
    if (req.originalUrl.includes('?')) {
      sendError(res, 400, 'unexpected_query', 'This request does not take a query string.');
      return;
    }
    try {
      res.json(await service().view());
    } catch {
      sendError(res, 500, 'scoreboard_failed', 'The scoreboard could not be built.');
    }
  });

  return router;
}

export default createScoreboardRouter();
