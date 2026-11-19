import type { Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import {
  getAccountAnalytics,
  type AccountAnalyticsComposer,
  type AccountAnalyticsOwnedComposer,
} from '../services/account-analytics-service';
import type { AccountAnalyticsComposed } from '../services/account-analytics-compose';
import { RustService, composeWithRust } from '../rust-service';
import { isRustAnalyticsComposed, rustAnalyticsInput } from '../rust-account-input';
import { isRustAnalyticsOwned, rustAnalyticsOwnedInput } from '../rust-analytics-owned-input';
import { rustGroupOn } from '../rust-service-groups';
import { rustStoreHosted } from '../rust-service-transport';
import { openAacStore } from '../../store/store-service';
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
import {
  readAnalyticsCaptureSettings,
  writeAnalyticsCaptureSettings,
  type AnalyticsCaptureSettings,
} from '../services/analytics-capture-settings';
import { clearAnalyticsRemoteCwds } from '../services/analytics-remote-sources';

const QUERY_KEYS = ['platform', 'range', 'provider', 'account', 'refresh', 'tz', 'from', 'to'];

export interface AccountAnalyticsRouterDeps {
  getAnalytics?: (
    query: AccountAnalyticsQuery,
    composer?: AccountAnalyticsComposer,
    owned?: AccountAnalyticsOwnedComposer
  ) => Promise<AccountAnalytics>;
  /** Provider ids the server reports; `all` is always accepted as well. */
  providerIds?: () => readonly string[];
  readCapture?: () => AnalyticsCaptureSettings;
  writeCapture?: (value: AnalyticsCaptureSettings) => void;
  clearCwds?: () => { folders: number; caches: number };
}

export function createAccountAnalyticsRouter(deps: AccountAnalyticsRouterDeps = {}): Router {
  const router = createApiRouter();
  router.get('/analytics', async (req, res): Promise<void> => {
    res.setHeader('Cache-Control', 'no-store');
    if (authKind(req) !== 'session') {
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
    // A provider of the server's table, or "other": CLI usage on a route no provider claims.
    if (
      provider !== 'all' &&
      provider !== 'other' &&
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
      // After every check above: the M3 Rust service may compose this answer from the inputs
      // the analytics service already read. Off mode, or no service, keeps the TS composition.
      const composer: AccountAnalyticsComposer = async (facts, legacy) =>
        (await composeWithRust(
          req,
          res,
          '/api/accounts/analytics',
          () => rustAnalyticsInput(facts),
          legacy,
          isRustAnalyticsComposed
        )) as AccountAnalyticsComposed;
      // R5/A2: with the analytics group on and the store hosted, Rust reads the store itself
      // and composes the whole answer. Off, shadow, unhosted or any decline keeps the TS path.
      const owned: AccountAnalyticsOwnedComposer = async (facts) => {
        const service: unknown = res.locals.rustService;
        if (!(service instanceof RustService) || service.mode !== 'on') return null;
        if (!rustGroupOn('analytics') || !rustStoreHosted(facts.scope)) return null;
        const store = (await openAacStore({ ccsDir: facts.scope })).store;
        if (!store) return null;
        const input = await rustAnalyticsOwnedInput(facts, store);
        if (!input) return null;
        const answer = await composeWithRust(
          req,
          res,
          '/api/accounts/analytics',
          input,
          () => null,
          isRustAnalyticsOwned
        );
        return (answer as AccountAnalytics | null) ?? null;
      };
      res.json(await (deps.getAnalytics ?? getAccountAnalytics)(request, composer, owned));
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
  /*
   * What Analytics captures beyond usage numbers: `cwd`, each session's working folder, kept in the
   * local cache only. On by default; turning it off also clears the folders already saved.
   */
  router.use('/analytics/capture', (req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    if (authKind(req) !== 'session') {
      res.status(401).json({ error: 'Authentication required', code: 'auth_required' });
      return;
    }
    if (Object.keys(req.query).length !== 0) {
      res.status(400).json({ error: 'Unexpected capture settings query.', code: 'invalid_query' });
      return;
    }
    next();
  });
  router.get('/analytics/capture', (_req, res) => {
    res.json((deps.readCapture ?? readAnalyticsCaptureSettings)());
  });
  router.put('/analytics/capture', (req, res) => {
    if (
      typeof req.headers.origin !== 'string' ||
      !req.headers.origin ||
      !isDashboardWebSocketOriginAllowed(req)
    ) {
      res.status(403).json({
        error: 'Capture settings require the dashboard origin.',
        code: 'origin_required',
      });
      return;
    }
    if (!req.is('application/json')) {
      res.status(415).json({ error: 'Capture settings require application/json.' });
      return;
    }
    const body = req.body as Record<string, unknown> | null;
    if (
      !body ||
      typeof body !== 'object' ||
      Array.isArray(body) ||
      Object.keys(body).length !== 1 ||
      typeof body.cwd !== 'boolean'
    ) {
      res
        .status(400)
        .json({ error: 'Send {"cwd": true} or {"cwd": false}.', code: 'invalid_body' });
      return;
    }
    try {
      // The setting first, so a scan finishing now saves no folder; then the folders already saved.
      (deps.writeCapture ?? writeAnalyticsCaptureSettings)({ cwd: body.cwd });
      const cleared = body.cwd ? undefined : (deps.clearCwds ?? clearAnalyticsRemoteCwds)();
      res.json({
        ...(deps.readCapture ?? readAnalyticsCaptureSettings)(),
        ...(cleared ? { cleared } : {}),
      });
    } catch {
      res.status(500).json({ error: 'Capture settings could not be saved.', code: 'save_failed' });
    }
  });
  return router;
}

export default createAccountAnalyticsRouter();
