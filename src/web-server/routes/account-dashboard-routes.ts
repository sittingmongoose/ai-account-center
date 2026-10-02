import type { Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import {
  getAccountDashboard,
  type AccountDashboardRequestContext,
} from '../services/account-dashboard-service';
import { isSecureTransport } from '../middleware/secure-transport';
import type {
  AccountDashboard,
  ClaudeDashboardPlatform,
} from '../services/account-dashboard-types';

export interface AccountDashboardRouterDeps {
  getDashboard?: (
    platform: ClaudeDashboardPlatform,
    refresh: boolean,
    context: AccountDashboardRequestContext
  ) => Promise<AccountDashboard>;
}

export function createAccountDashboardRouter(deps: AccountDashboardRouterDeps = {}): Router {
  const router = createApiRouter();
  router.get('/dashboard', async (req, res): Promise<void> => {
    if (authKind(req) === null) {
      res.status(401).json({ error: 'Authentication required' });
      return;
    }
    const platform = req.query.platform ?? 'mac';
    const refresh = req.query.refresh ?? 'false';
    if (
      (platform !== 'mac' && platform !== 'windows') ||
      (refresh !== 'true' && refresh !== 'false') ||
      Object.keys(req.query).some((key) => key !== 'platform' && key !== 'refresh')
    ) {
      res.status(400).json({ error: 'Select a valid desktop platform and refresh value.' });
      return;
    }
    res.setHeader('Cache-Control', 'no-store');
    try {
      res.json(
        await (deps.getDashboard ?? getAccountDashboard)(platform, refresh === 'true', {
          // providers[].signIn: flows that carry a key or code need a secure transport.
          secureTransport: isSecureTransport(req),
        })
      );
    } catch {
      res.status(500).json({ error: 'Account usage could not be read safely.' });
    }
  });
  return router;
}

export default createAccountDashboardRouter();
