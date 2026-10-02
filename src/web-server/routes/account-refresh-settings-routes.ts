import type { Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { rescheduleAccountAnalyticsSampling } from '../services/account-analytics-service';
import {
  isAccountRefreshSettings,
  readAccountRefreshSettings,
  writeAccountRefreshSettings,
  type AccountRefreshSettings,
} from '../services/account-refresh-settings';

interface SettingsRouterDeps {
  read?: () => AccountRefreshSettings;
  write?: (value: AccountRefreshSettings) => AccountRefreshSettings;
  onChanged?: () => void;
}

export function createAccountRefreshSettingsRouter(deps: SettingsRouterDeps = {}): Router {
  const router = createApiRouter();
  router.use('/settings', (req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    if (authKind(req) !== 'session') {
      res.status(401).json({ error: 'Authentication required' });
      return;
    }
    if (Object.keys(req.query).length !== 0) {
      res.status(400).json({ error: 'Unexpected settings query.' });
      return;
    }
    next();
  });
  router.get('/settings', (_req, res) => {
    res.json((deps.read ?? readAccountRefreshSettings)());
  });
  router.put('/settings', (req, res) => {
    if (
      typeof req.headers.origin !== 'string' ||
      !req.headers.origin ||
      !isDashboardWebSocketOriginAllowed(req)
    ) {
      res.status(403).json({ error: 'Settings require the dashboard origin.' });
      return;
    }
    if (!req.is('application/json')) {
      res.status(415).json({ error: 'Settings require application/json.' });
      return;
    }
    if (!isAccountRefreshSettings(req.body)) {
      res.status(400).json({ error: 'Choose a whole number of seconds from 30 to 3600.' });
      return;
    }
    try {
      const saved = (deps.write ?? writeAccountRefreshSettings)(req.body);
      (deps.onChanged ?? rescheduleAccountAnalyticsSampling)();
      res.json(saved);
    } catch {
      res.status(500).json({ error: 'Usage refresh settings could not be saved.' });
    }
  });
  return router;
}

export default createAccountRefreshSettingsRouter();
