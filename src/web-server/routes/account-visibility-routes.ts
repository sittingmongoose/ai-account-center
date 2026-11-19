import type { Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { createLogger } from '../../services/logging';
import { getCcsDir } from '../../utils/config-manager';
import { broadcastDashboardEvent } from '../dashboard-events';
import { readOwnedWithRust, writeWithRust } from '../rust-service';
import { acceptRustVisibility } from '../rust-account-input';
import {
  ACCOUNT_VISIBILITY_FILE,
  parseVisibilityBody,
  parseVisibilityDelta,
  patchAccountVisibility,
  type AccountVisibilityDelta,
  readAccountVisibility,
  updateAccountVisibility,
  type AccountVisibility,
  type AccountVisibilityRead,
  type AccountVisibilityUpdate,
  type AccountVisibilityUpdateResult,
} from '../services/account-visibility';
import { withPrivateFileLock } from '../services/private-json-store';
import { tsWritesRow } from '../../shared/private-files';
import path from 'node:path';

/**
 * GET and PUT /api/accounts/visibility (CONTRACT-registry-lifecycle section 4).
 * A browser session is required; the PUT also needs the dashboard Origin,
 * application/json, a strict body of at most 8 KB and no query string. The PUT
 * body may name any non-empty subset of `hiddenProviders`, `hiddenAccountIds`,
 * `trayHiddenProviders` and `trayHiddenAccountIds`; a list it leaves out is
 * unchanged, so the dashboard and tray switches never change each other.
 * Errors are `{error, code}` with fixed sentences and never echo input.
 */
const MAX_BODY_BYTES = 8 * 1024;
const logger = createLogger('account-visibility');

export interface AccountVisibilityRouterDeps {
  read?: () => Promise<AccountVisibilityRead>;
  write?: (update: AccountVisibilityUpdate) => Promise<AccountVisibilityUpdateResult>;
  patch?: (delta: AccountVisibilityDelta) => Promise<AccountVisibilityUpdateResult>;
  /** After a saved change: tell the /ws clients to re-read the dashboard. */
  onChanged?: () => void;
  audit?: (counts: {
    hiddenProviders: number;
    hiddenAccountIds: number;
    trayHiddenProviders: number;
    trayHiddenAccountIds: number;
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

  router.get('/visibility', async (req, res): Promise<void> => {
    // After the guards above, the M3 Rust service may read the same file; this is the fallback.
    const legacy = async (): Promise<unknown> => {
      const current = await read().catch((): AccountVisibilityRead => ({ state: 'unavailable' }));
      if (current.state !== 'ok') {
        fail(res, 500, 'visibility_unavailable', 'Account visibility could not be read safely.');
        return undefined;
      }
      res.json(current.visibility);
      return current.visibility;
    };
    if (deps.read) {
      await legacy();
      return;
    }
    const directory = getCcsDir();
    await readOwnedWithRust(
      req,
      res,
      '/api/accounts/visibility',
      directory,
      acceptRustVisibility(directory),
      legacy
    );
  });

  router.route('/visibility').put(save).patch(save);

  async function save(req: Request, res: Response): Promise<void> {
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
    const patch = req.method === 'PATCH';
    const update = bodyTooLarge(req)
      ? null
      : patch
        ? parseVisibilityDelta(req.body)
        : parseVisibilityBody(req.body);
    if (!update) {
      fail(
        res,
        400,
        'invalid_body',
        'Send visibility lists with known providers and at most 128 valid ids each.'
      );
      return;
    }
    let saved: AccountVisibility;
    try {
      const legacy = () =>
        patch
          ? (deps.patch ?? ((delta) => patchAccountVisibility(getCcsDir(), delta)))(
              update as AccountVisibilityDelta
            )
          : write(update as AccountVisibilityUpdate);
      let result: AccountVisibilityUpdateResult;
      if ((patch ? deps.patch : deps.write) || tsWritesRow('account-visibility')) {
        result = await legacy();
      } else {
        const directory = getCcsDir();
        const accept = acceptRustVisibility(directory);
        result = await withPrivateFileLock(
          path.join(directory, ACCOUNT_VISIBILITY_FILE),
          async () => {
            const reply = await writeWithRust<
              AccountVisibilityUpdateResult & { folderWarnings?: unknown }
            >(
              req,
              res,
              '/api/accounts/visibility',
              'account-visibility',
              update,
              (body): body is AccountVisibilityUpdateResult & { folderWarnings?: unknown } => {
                if (!body || typeof body !== 'object' || !('state' in body)) return false;
                if (body.state === 'unavailable') return Object.keys(body).length === 1;
                if (
                  body.state !== 'saved' ||
                  !('visibility' in body) ||
                  !('folderWarnings' in body)
                )
                  return false;
                return !!accept({
                  visibility: body.visibility,
                  folderWarnings: body.folderWarnings,
                });
              },
              legacy
            );
            if (reply.state === 'saved' && reply.folderWarnings)
              accept({
                visibility: reply.visibility,
                folderWarnings: reply.folderWarnings,
              })?.effect();
            return reply;
          }
        );
      }
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
      trayHiddenAccountIds: saved.trayHiddenAccountIds.length,
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
  }

  return router;
}

export default createAccountVisibilityRouter();
