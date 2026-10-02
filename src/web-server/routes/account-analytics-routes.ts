import { Router } from 'express';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { getAccountAnalytics } from '../services/account-analytics-service';
import { defaultAnalyticsProviderIds } from '../services/account-analytics-providers';
import {
  ACCOUNT_ANALYTICS_RANGE_PRESETS,
  ANALYTICS_QUERY_ERRORS,
  AccountAnalyticsQueryError,
  validateAccountAnalyticsRangeShape,
  type AccountAnalyticsQueryErrorCode,
  type AccountAnalyticsRangePreset,
} from '../services/account-analytics-range';
import type { AccountAnalytics, AccountAnalyticsQuery } from '../services/account-analytics-types';

const QUERY_KEYS = ['platform', 'range', 'provider', 'account', 'refresh', 'tz', 'from', 'to'];

export interface AccountAnalyticsRouterDeps {
  getAnalytics?: (query: AccountAnalyticsQuery) => Promise<AccountAnalytics>;
  /** Provider ids the server reports; `all` is always accepted as well. */
  providerIds?: () => readonly string[];
}

export function createAccountAnalyticsRouter(deps: AccountAnalyticsRouterDeps = {}): Router {
  const router = Router();
  router.get('/analytics', async (req, res): Promise<void> => {
    res.setHeader('Cache-Control', 'no-store');
    if (req.session?.authenticated !== true) {
      res.status(401).json({ error: 'Authentication required', code: 'auth_required' });
      return;
    }
    if (!isDashboardWebSocketOriginAllowed(req) || req.headers['sec-fetch-site'] === 'cross-site') {
      res.status(403).json({
        error: 'Account analytics requires the dashboard origin.',
        code: 'origin_required',
      });
      return;
    }
    const reject = (code: AccountAnalyticsQueryErrorCode): void => {
      res.status(400).json({ error: ANALYTICS_QUERY_ERRORS[code], code });
    };
    const keys = Object.keys(req.query);
    if (
      keys.some((key) => !QUERY_KEYS.includes(key)) ||
      keys.some((key) => typeof req.query[key] !== 'string')
    ) {
      reject('invalid_query');
      return;
    }
    const query = req.query as Record<string, string | undefined>;
    const platform = query.platform ?? 'mac';
    const range = query.range ?? '7d';
    const provider = query.provider ?? 'all';
    const account = query.account ?? 'all';
    const { refresh, tz, from, to } = query;
    if (
      (platform !== 'mac' && platform !== 'windows') ||
      !/^[a-zA-Z0-9][a-zA-Z0-9:@._+-]{0,319}$/.test(account) ||
      (refresh !== undefined && refresh !== 'true' && refresh !== 'false')
    ) {
      reject('invalid_query');
      return;
    }
    if (!ACCOUNT_ANALYTICS_RANGE_PRESETS.includes(range as AccountAnalyticsRangePreset)) {
      reject('invalid_range');
      return;
    }
    if (
      provider !== 'all' &&
      !(deps.providerIds ?? defaultAnalyticsProviderIds)().includes(provider)
    ) {
      reject('invalid_provider');
      return;
    }
    const request: AccountAnalyticsQuery = {
      platform,
      range: range as AccountAnalyticsRangePreset,
      provider: provider as AccountAnalyticsQuery['provider'],
      account,
      ...(refresh !== undefined ? { refresh: refresh === 'true' } : {}),
      ...(tz !== undefined ? { tz } : {}),
      ...(from !== undefined ? { from } : {}),
      ...(to !== undefined ? { to } : {}),
    };
    try {
      // Zone and date shape errors need no clock; retention is checked by the service.
      validateAccountAnalyticsRangeShape(request);
      res.json(await (deps.getAnalytics ?? getAccountAnalytics)(request));
    } catch (error) {
      if (error instanceof AccountAnalyticsQueryError) {
        reject(error.code);
        return;
      }
      res.status(500).json({
        error: 'Account analytics could not be read safely.',
        code: 'analytics_unavailable',
      });
    }
  });
  return router;
}

export default createAccountAnalyticsRouter();
