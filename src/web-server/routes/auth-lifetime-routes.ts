import type { Request, Response, Router } from 'express';
import { mutateConfig } from '../../config/config-loader-facade';
import { SESSION_LIFETIME_DAYS } from '../../config/schemas/auth';
import { withAuthWriteGate } from '../services/dashboard-auth-files';
import {
  audit,
  effectiveSessionLifetimeDays,
  invalidBody,
  isSessionLifetimeDays,
  readAuthBody,
  requireAuthConfigured,
  sendAuthError,
} from './auth-route-helpers';
import { requireBrowserSession } from './auth-session-routes';

/**
 * The session lifetime setting (Settings): 1, 7, 30, 90 or 365 days,
 * default 30. The session cookie and the idle expiry follow it.
 *
 * - `GET /api/auth/session-lifetime` reads it (any signed-in browser).
 * - `PUT /api/auth/session-lifetime` `{days}` saves it to config.yaml from
 *   any signed-in browser session, mirroring `session_timeout_hours` so older
 *   readers agree. It applies to the next sign-in; sessions already signed in
 *   keep their own cookie.
 */
export interface SessionLifetimeView {
  days: number;
  hours: number;
  options: readonly number[];
}

function view(): SessionLifetimeView {
  const days = effectiveSessionLifetimeDays();
  return { days, hours: days * 24, options: SESSION_LIFETIME_DAYS };
}

function readLifetime(req: Request, res: Response): void {
  if (!requireAuthConfigured(res)) return;
  if (!requireBrowserSession(req, res)) return;
  if (req.originalUrl.includes('?')) {
    sendAuthError(res, 400, 'unexpected_query', 'This request does not take a query string.');
    return;
  }
  res.json(view());
}

async function changeLifetime(req: Request, res: Response): Promise<void> {
  if (!requireAuthConfigured(res)) return;
  if (!requireBrowserSession(req, res)) return;
  const body = readAuthBody(req, res, 'required', ['days']);
  if (!body) return;
  if (!isSessionLifetimeDays(body.days)) {
    invalidBody(res);
    return;
  }
  const days = body.days;
  try {
    await withAuthWriteGate(async () => {
      mutateConfig((config) => {
        const existing = config.dashboard_auth;
        config.dashboard_auth = {
          enabled: existing?.enabled ?? true,
          username: existing?.username ?? '',
          password_hash: existing?.password_hash ?? '',
          session_timeout_hours: days * 24,
          session_lifetime_days: days,
          ...(typeof existing?.password_changed_at === 'string'
            ? { password_changed_at: existing.password_changed_at }
            : {}),
        };
      });
    });
  } catch {
    sendAuthError(res, 500, 'write_failed', 'The session lifetime could not be saved.');
    return;
  }
  audit('auth.session.lifetime_changed', 'Dashboard session lifetime changed', { days });
  res.json(view());
}

export function registerAuthLifetimeRoutes(router: Router): void {
  router.get('/session-lifetime', readLifetime);
  router.put('/session-lifetime', changeLifetime);
}
