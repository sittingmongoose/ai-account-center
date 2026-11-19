import type { Router } from 'express';
import { createApiRouter } from './api-router';
import { getCcsDir } from '../../utils/config-manager';
import { authKind } from '../middleware/request-auth';
import {
  getAccountDashboard,
  readAccountDashboardForHome,
  withPassedResetsAt,
  type AccountDashboardRequestContext,
} from '../services/account-dashboard-service';
import { isSecureTransport } from '../middleware/secure-transport';
import { dashboardCollectionService, withOfferedViews } from '../views/offered-views';
import { answerDashboardFed } from '../views/dashboard-feed';
import type {
  AccountDashboard,
  ClaudeDashboardPlatform,
} from '../services/account-dashboard-types';

export interface AccountDashboardRouterDeps {
  readDashboard?: typeof readAccountDashboardForHome;
  scope?: () => string;
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
      // providers[].signIn constrains flows carrying a key or code to secure transport.
      const secureTransport = isSecureTransport(req);
      const context = { secureTransport };
      const service = dashboardCollectionService(res);
      if (service && (!deps.getDashboard || deps.readDashboard)) {
        const read = await (deps.readDashboard ?? readAccountDashboardForHome)(
          platform,
          refresh === 'true',
          context
        );
        const dashboard = withOfferedViews(read.dashboard, res);
        if (
          await answerDashboardFed(req, res, service, {
            platform,
            secure: secureTransport,
            dashboard,
            now: read.now,
            scope: (deps.scope ?? getCcsDir)(),
          })
        )
          return;
        res.json(withPassedResetsAt(dashboard, read.now));
        return;
      }
      res.json(
        withOfferedViews(
          await (deps.getDashboard ?? getAccountDashboard)(platform, refresh === 'true', context),
          res
        )
      );
    } catch {
      res.status(500).json({ error: 'Account usage could not be read safely.' });
    }
  });
  return router;
}

export default createAccountDashboardRouter();
