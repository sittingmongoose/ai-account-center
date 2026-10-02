import type { Router } from 'express';
import { createApiRouter } from './api-router';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { getAccountAnalytics } from '../services/account-analytics-service';
import type { AccountAnalytics, AccountAnalyticsQuery } from '../services/account-analytics-types';

const PROVIDERS = [
  'all',
  'claude',
  'codex',
  'antigravity',
  'muse',
  'cursor',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
];

export interface AccountAnalyticsRouterDeps {
  getAnalytics?: (query: AccountAnalyticsQuery) => Promise<AccountAnalytics>;
}

export function createAccountAnalyticsRouter(deps: AccountAnalyticsRouterDeps = {}): Router {
  const router = createApiRouter();
  router.get('/analytics', async (req, res): Promise<void> => {
    res.setHeader('Cache-Control', 'no-store');
    if (req.session?.authenticated !== true) {
      res.status(401).json({ error: 'Authentication required' });
      return;
    }
    if (!isDashboardWebSocketOriginAllowed(req) || req.headers['sec-fetch-site'] === 'cross-site') {
      res.status(403).json({ error: 'Account analytics requires the dashboard origin.' });
      return;
    }
    const platform = req.query.platform ?? 'mac';
    const range = req.query.range ?? '7d';
    const provider = req.query.provider ?? 'all';
    const account = req.query.account ?? 'all';
    const refresh = req.query.refresh;
    if (
      (platform !== 'mac' && platform !== 'windows') ||
      (range !== '24h' && range !== '7d' && range !== '30d') ||
      typeof provider !== 'string' ||
      !PROVIDERS.includes(provider) ||
      typeof account !== 'string' ||
      !/^[a-zA-Z0-9][a-zA-Z0-9:@._+-]{0,319}$/.test(account) ||
      (refresh !== undefined && refresh !== 'true' && refresh !== 'false') ||
      Object.keys(req.query).some(
        (key) => !['platform', 'range', 'provider', 'account', 'refresh'].includes(key)
      )
    ) {
      res.status(400).json({
        error: 'Select a valid platform, time range, provider, account, and refresh flag.',
      });
      return;
    }
    try {
      res.json(
        await (deps.getAnalytics ?? getAccountAnalytics)({
          platform,
          range,
          provider: provider as AccountAnalyticsQuery['provider'],
          account,
          ...(refresh !== undefined ? { refresh: refresh === 'true' } : {}),
        })
      );
    } catch {
      res.status(500).json({ error: 'Account analytics could not be read safely.' });
    }
  });
  return router;
}

export default createAccountAnalyticsRouter();
