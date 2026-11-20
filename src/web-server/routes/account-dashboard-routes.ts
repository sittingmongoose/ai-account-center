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
      const service =
        process.env.AAC_CUT_WORKER_COLLECTION === '1' ? null : dashboardCollectionService(res);
      if (service && (!deps.getDashboard || deps.readDashboard)) {
        if (process.env.AAC_RUST_CUT_WORKER === '1' && process.platform !== 'win32') {
          const result = await service.fedView(
            req,
            '/api/accounts/dashboard',
            { 'x-aac-dashboard-secure': secureTransport ? '1' : '0' },
            'accounts-dashboard@1',
            `?platform=${platform}&refresh=${refresh}`
          );
          if (result && typeof result !== 'string') {
            const dashboard = withOfferedViews(JSON.parse(result.bytes.toString('utf8')), res);
            if (typeof res.locals.onRustDashboard === 'function')
              res.locals.onRustDashboard(platform, dashboard);
            res.json(dashboard);
            return;
          }
        }
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
