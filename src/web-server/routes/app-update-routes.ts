import type { NextFunction, Request, Response, Router } from 'express';
import { RustService } from '../rust-service';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import {
  AppUpdateBusyError,
  getAppUpdateService,
  type AppUpdateService,
} from '../services/app-update-service';

export function createAppUpdateRouter(service?: AppUpdateService): Router {
  const router = createApiRouter();
  router.use((req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    if (authKind(req) !== 'session') {
      res.status(401).json({ error: 'Authentication required' });
      return;
    }
    next();
  });
  router.get('/status', async (req, res) => {
    try {
      if (await answerFromRust(req, res, 'status')) return;
    } catch {
      res.status(503).json({ error: 'App update status could not be read safely.' });
      return;
    }
    res.json((service ?? getAppUpdateService()).getStatus());
  });
  router.post('/start', mutationGuard, async (req, res) => {
    try {
      if (await answerFromRust(req, res, 'start')) return;
    } catch {
      res.status(500).json({ error: 'App updates could not start safely.' });
      return;
    }
    const updater = service ?? getAppUpdateService();
    try {
      res.status(202).json(updater.start());
    } catch (error) {
      if (error instanceof AppUpdateBusyError) {
        res
          .status(409)
          .json({ error: 'An app update is already running.', ...updater.getStatus() });
        return;
      }
      res.status(500).json({ error: 'App updates could not start safely.' });
    }
  });
  router.post('/cancel', mutationGuard, async (req, res) => {
    try {
      if (await answerFromRust(req, res, 'cancel')) return;
    } catch {
      res.status(503).json({ error: 'App update cancellation could not be confirmed.' });
      return;
    }
    const outcome = (service ?? getAppUpdateService()).cancel();
    if (outcome.notOwner) {
      res.status(409).json({
        error: 'This update is run by another dashboard process, so it cannot be cancelled here.',
        code: 'not_owner',
        job: outcome.job,
        cancelling: false,
      });
      return;
    }
    res.status(outcome.cancelling ? 202 : 200).json(outcome);
  });
  return router;
}

function mutationGuard(req: Request, res: Response, next: NextFunction): void {
  if (
    typeof req.headers.origin !== 'string' ||
    !req.headers.origin ||
    !isDashboardWebSocketOriginAllowed(req)
  ) {
    res.status(403).json({ error: 'App updates require the dashboard origin.' });
    return;
  }
  if (!req.is('application/json')) {
    res.status(415).json({ error: 'App updates require application/json.' });
    return;
  }
  if (
    Object.keys(req.query).length !== 0 ||
    !req.body ||
    typeof req.body !== 'object' ||
    Array.isArray(req.body) ||
    Object.keys(req.body).length !== 0
  ) {
    res.status(400).json({ error: 'Provide an empty JSON object.' });
    return;
  }
  next();
}

async function answerFromRust(
  req: Request,
  res: Response,
  operation: 'status' | 'start' | 'cancel'
) {
  const service: unknown = res.locals.rustService;
  if (!(service instanceof RustService)) return false;
  const reply = await service.appUpdate(req, operation);
  if (!reply) return false;
  res.status(reply.status).json(reply.body);
  return true;
}

export default createAppUpdateRouter();
