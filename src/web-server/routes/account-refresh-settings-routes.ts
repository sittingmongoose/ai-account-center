import type { Router } from 'express';
import path from 'node:path';
import { getCcsDir } from '../../utils/config-manager';
import { withPrivateFileLock } from '../services/private-json-store';
import { respondWithRust, writeWithRust } from '../rust-service';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import {
  ACCOUNT_REFRESH_SETTINGS_FILE,
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
  router.get('/settings', (req, res) =>
    respondWithRust(req, res, '/api/accounts/settings', deps.read ?? readAccountRefreshSettings)
  );
  router.put('/settings', async (req, res) => {
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
      const input = req.body;
      const ccs = getCcsDir();
      const saved = await withPrivateFileLock(
        path.join(ccs, ACCOUNT_REFRESH_SETTINGS_FILE),
        async () => {
          const value = deps.write
            ? deps.write(input)
            : await writeWithRust(
                req,
                res,
                '/api/accounts/settings',
                'account-refresh-settings',
                () => input,
                (value): value is AccountRefreshSettings =>
                  isAccountRefreshSettings(value) &&
                  value.refreshIntervalSeconds === input.refreshIntervalSeconds,
                () => writeAccountRefreshSettings(input, ccs)
              );
          deps.onChanged?.();
          return value;
        }
      );
      res.json(saved);
    } catch {
      res.status(500).json({ error: 'Usage refresh settings could not be saved.' });
    }
  });
  return router;
}

export default createAccountRefreshSettingsRouter();
