import { Router } from 'express';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import {
  AppUpdateBusyError,
  getAppUpdateService,
  type AppUpdateService,
} from '../services/app-update-service';

export function createAppUpdateRouter(service?: AppUpdateService): Router {
  const router = Router();
  router.use((req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    if (req.session?.authenticated !== true) {
      res.status(401).json({ error: 'Authentication required' });
      return;
    }
    next();
  });
  router.get('/status', (_req, res) => res.json((service ?? getAppUpdateService()).getStatus()));
  router.post('/start', (req, res) => {
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
  return router;
}

export default createAppUpdateRouter();
