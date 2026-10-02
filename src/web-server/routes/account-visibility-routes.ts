import type { Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { createLogger } from '../../services/logging';
import { getCcsDir } from '../../utils/config-manager';
import { broadcastDashboardEvent } from '../dashboard-events';
import {
  parseVisibilityBody,
  readAccountVisibility,
  updateAccountVisibility,
  type AccountVisibility,
  type AccountVisibilityRead,
  type AccountVisibilityUpdate,
  type AccountVisibilityUpdateResult,
} from '../services/account-visibility';

/**
 * GET and PUT /api/accounts/visibility (CONTRACT-registry-lifecycle section 4).
 * A browser session is required; the PUT also needs the dashboard Origin,
 * application/json, a strict body of at most 8 KB and no query string. The PUT
 * body may name any non-empty subset of `hiddenProviders`, `hiddenAccountIds`
 * and `trayHiddenProviders`; a list it leaves out is unchanged.
 * Errors are `{error, code}` with fixed sentences and never echo input.
 */
const MAX_BODY_BYTES = 8 * 1024;
const logger = createLogger('account-visibility');

export interface AccountVisibilityRouterDeps {
  read?: () => Promise<AccountVisibilityRead>;
  write?: (update: AccountVisibilityUpdate) => Promise<AccountVisibilityUpdateResult>;
  /** After a saved change: tell the /ws clients to re-read the dashboard. */
  onChanged?: () => void;
  audit?: (counts: {
    hiddenProviders: number;
    hiddenAccountIds: number;
    trayHiddenProviders: number;
  }) => void;
}

function fail(res: Response, status: number, code: string, error: string): void {
  res.status(status).json({ error, code });
}

function bodyTooLarge(req: Request): boolean {
  const declared = Number(req.headers['content-length']);
  if (Number.isFinite(declared) && declared > MAX_BODY_BYTES) return true;
  try {
    return Buffer.byteLength(JSON.stringify(req.body ?? null), 'utf8') > MAX_BODY_BYTES;
  } catch {
    return true;
  }
}

export function createAccountVisibilityRouter(deps: AccountVisibilityRouterDeps = {}): Router {
  const router = createApiRouter();
  const read = deps.read ?? (() => readAccountVisibility(getCcsDir()));
  const write = deps.write ?? ((update) => updateAccountVisibility(getCcsDir(), update));

  router.use('/visibility', (req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    if (authKind(req) !== 'session') {
      fail(res, 401, 'auth_required', 'Authentication required');
      return;
    }
    if (req.originalUrl.includes('?')) {
      fail(res, 400, 'unexpected_query', 'This request does not take a query string.');
      return;
    }
    next();
  });

  router.get('/visibility', async (_req, res): Promise<void> => {
    const current = await read().catch((): AccountVisibilityRead => ({ state: 'unavailable' }));
    if (current.state !== 'ok') {
      fail(res, 500, 'visibility_unavailable', 'Account visibility could not be read safely.');
      return;
    }
    res.json(current.visibility);
  });

  router.put('/visibility', async (req, res): Promise<void> => {
    if (
      typeof req.headers.origin !== 'string' ||
      !req.headers.origin ||
      !isDashboardWebSocketOriginAllowed(req)
    ) {
      fail(res, 403, 'origin_required', 'This change requires the dashboard origin.');
      return;
    }
    if (!req.is('application/json')) {
      fail(res, 415, 'json_required', 'This change requires application/json.');
      return;
    }
    const update = bodyTooLarge(req) ? null : parseVisibilityBody(req.body);
    if (!update) {
      fail(
        res,
        400,
        'invalid_body',
        'Send hiddenProviders or trayHiddenProviders with known providers and at most 128 valid ids.'
      );
      return;
    }
    let saved: AccountVisibility;
    try {
      const result = await write(update);
      if (result.state !== 'saved') {
        // A partial update cannot merge into a file that cannot be read safely.
        fail(res, 500, 'visibility_unavailable', 'Account visibility could not be read safely.');
        return;
      }
      saved = result.visibility;
    } catch {
      fail(res, 500, 'visibility_write_failed', 'Account visibility could not be saved.');
      return;
    }
    const counts = {
      hiddenProviders: saved.hiddenProviders.length,
      hiddenAccountIds: saved.hiddenAccountIds.length,
      trayHiddenProviders: saved.trayHiddenProviders.length,
    };
    // The change is saved; a failed log line or hint never changes the answer.
    try {
      (
        deps.audit ??
        ((value) => logger.info('accounts.visibility.changed', 'Account visibility changed', value))
      )(counts);
    } catch {
      /* Logging is best effort. */
    }
    try {
      (deps.onChanged ?? (() => broadcastDashboardEvent({ type: 'accounts-changed' })))();
    } catch {
      /* Clients also re-read on their own refresh. */
    }
    res.json(saved);
  });

  return router;
}

export default createAccountVisibilityRouter();
