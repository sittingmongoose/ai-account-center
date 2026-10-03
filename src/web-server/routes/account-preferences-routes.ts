import type { Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import {
  isDashboardPreferences,
  readDashboardPreferences,
  writeDashboardPreferences,
  type DashboardPreferences,
} from '../services/dashboard-preferences';

interface PreferencesRouterDeps {
  read?: () => DashboardPreferences;
  write?: (value: DashboardPreferences) => DashboardPreferences;
}

/**
 * Dashboard preferences (Settings): the display time zone, the history
 * snapshot cleanup and the extra usage-log sources. A signed-in browser
 * session reads and saves the whole shape; the time zone also becomes the
 * analytics day-bucket default and every displayed time follows it.
 */
export function createAccountPreferencesRouter(deps: PreferencesRouterDeps = {}): Router {
  const router = createApiRouter();
  router.use('/preferences', (req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    if (authKind(req) !== 'session') {
      res.status(401).json({ error: 'Authentication required' });
      return;
    }
    if (Object.keys(req.query).length !== 0) {
      res.status(400).json({ error: 'Unexpected preferences query.' });
      return;
    }
    next();
  });
  router.get('/preferences', (_req, res) => {
    res.json((deps.read ?? readDashboardPreferences)());
  });
  router.put('/preferences', (req, res) => {
    if (
      typeof req.headers.origin !== 'string' ||
      !req.headers.origin ||
      !isDashboardWebSocketOriginAllowed(req)
    ) {
      res.status(403).json({ error: 'Preferences require the dashboard origin.' });
      return;
    }
    if (!req.is('application/json')) {
      res.status(415).json({ error: 'Preferences require application/json.' });
      return;
    }
    if (!isDashboardPreferences(req.body)) {
      res.status(400).json({ error: 'Preferences are not valid.' });
      return;
    }
    try {
      res.json((deps.write ?? writeDashboardPreferences)(req.body));
    } catch {
      res.status(500).json({ error: 'Dashboard preferences could not be saved.' });
    }
  });
  return router;
}

export default createAccountPreferencesRouter();
