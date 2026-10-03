import type { NextFunction, Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { accountRouteGuard, mutationBody, sendError } from './account-route-guards';
import { isSecureTransport } from '../middleware/secure-transport';
import { sessionKey } from '../services/account-confirmations';
import { LIFECYCLE_MESSAGES, LifecycleHttpError } from '../services/account-lifecycle-accounts';
import { addAccount, replaceKey, signInAgain } from '../services/account-lifecycle-actions';
import {
  cancelJob,
  openApp,
  readJob,
  recheck,
  relabel,
  submitJobCode,
} from '../services/account-lifecycle-extras';
import { registryListing } from '../services/account-lifecycle-listing';
import {
  defaultLifecycleEnv,
  type LifecycleContext,
  type LifecycleEnv,
  type LifecycleResult,
} from '../services/account-lifecycle-env';
import {
  purgeTrash,
  removeAccount,
  restoreTrash,
  trashListing,
} from '../services/account-lifecycle-removal';
import { JOB_ID_PATTERN } from '../services/signin-jobs';
import { antigravityTerminal } from '../services/account-lifecycle-helpers';

/**
 * The account lifecycle routes (CONTRACT-registry-lifecycle section 6), under
 * /api/accounts. Every route needs a browser session (device tokens get 403
 * `device_scope`) and answers `Cache-Control: no-store`; mutations also need
 * the dashboard Origin, application/json and a strict body of at most 8 KB.
 * Keys, authorization codes and device codes need a secure transport.
 *
 * GET    /registry                       the Accounts & Settings page
 * POST   /add                            Add (201, or 202 with a sign-in job)
 * POST   /:id/signin-again               a job (202) or a guide (200)
 * PUT    /:id/key                        Replace key
 * POST   /:id/remove                     {} then {confirmationToken}
 * POST   /:id/open                       Cursor on the Mac
 * POST   /:id/recheck                    one account now (10 s limit)
 * PATCH  /:id                            label
 * GET    /trash                          the 30-day trash
 * POST   /trash/:trashId/restore         {} then {confirmationToken}
 * POST   /trash/:trashId/purge           {} then {confirmationToken, confirm: "DELETE"}
 * GET    /signin-jobs/:jobId             poll a job
 * POST   /signin-jobs/:jobId/cancel      cancel it
 * POST   /signin-jobs/:jobId/code        supervised flows: the authorization code
 * GET    /signin-command?provider=&profile=   terminal fallback
 */
export interface AccountLifecycleRouterDeps {
  env?: () => LifecycleEnv;
}

function context(req: Request): LifecycleContext {
  return { secure: isSecureTransport(req), sessionKey: sessionKey(req.sessionID) };
}

function answer(res: Response, result: LifecycleResult): void {
  for (const [name, value] of Object.entries(result.headers ?? {})) res.setHeader(name, value);
  res.status(result.status).json(result.body);
}

function fail(res: Response, error: unknown): void {
  if (error instanceof LifecycleHttpError) {
    const extra = { ...error.extra };
    if (error.code === 'rate_limited' && typeof extra.retryAfterSeconds === 'number') {
      res.setHeader('Retry-After', String(extra.retryAfterSeconds));
    }
    sendError(
      res,
      error.status,
      error.code,
      LIFECYCLE_MESSAGES[error.code] ?? 'The request could not be completed.',
      extra
    );
    return;
  }
  // Never echo process output, paths or provider text.
  sendError(res, 500, 'internal_error', 'The request could not be completed safely.');
}

export function createAccountLifecycleRouter(deps: AccountLifecycleRouterDeps = {}): Router {
  const router = createApiRouter();
  let cached: LifecycleEnv | null = null;
  const env = () => (deps.env ? deps.env() : (cached ??= defaultLifecycleEnv()));
  const guard = accountRouteGuard();
  // The action routes (`/:id/<action>`) own their whole path shape, so any id
  // there is checked after the guard: malformed is 400 invalid_account, unknown
  // is 404 unknown_account. A bare `PATCH /:id` could be any other path under
  // /api/accounts, so only account-shaped segments reach it; the rest keep the
  // JSON 404.
  const accountPath = (req: Request, _res: Response, next: NextFunction) => {
    const id = req.params.id ?? '';
    next(id.includes(':') || id.startsWith('plan-opencode-go-console-') ? undefined : 'route');
  };
  const commandGuard = accountRouteGuard((req) => req.path === '/signin-command');

  const read =
    (action: (req: Request) => Promise<LifecycleResult> | LifecycleResult) =>
    async (req: Request, res: Response): Promise<void> => {
      try {
        answer(res, await action(req));
      } catch (error) {
        fail(res, error);
      }
    };
  const write =
    (
      action: (
        req: Request,
        body: Record<string, unknown>
      ) => Promise<LifecycleResult> | LifecycleResult
    ) =>
    async (req: Request, res: Response): Promise<void> => {
      const body = mutationBody(req, res);
      if (!body) return;
      try {
        answer(res, await action(req, body));
      } catch (error) {
        fail(res, error);
      }
    };
  const jobId = (req: Request): string => {
    if (!JOB_ID_PATTERN.test(req.params.jobId)) throw new LifecycleHttpError(404, 'unknown_job');
    return req.params.jobId;
  };

  router.get(
    '/registry',
    guard,
    read((req) => registryListing(env(), context(req)))
  );
  router.post(
    '/add',
    guard,
    write((req, body) => addAccount(env(), body, context(req)))
  );
  router.get(
    '/trash',
    guard,
    read(() => trashListing(env()))
  );
  router.post(
    '/trash/:trashId/restore',
    guard,
    write((req, body) => restoreTrash(env(), req.params.trashId, body, context(req)))
  );
  router.post(
    '/trash/:trashId/purge',
    guard,
    write((req, body) => purgeTrash(env(), req.params.trashId, body, context(req)))
  );
  router.get(
    '/signin-jobs/:jobId',
    guard,
    read((req) => readJob(env(), jobId(req), context(req)))
  );
  router.post(
    '/signin-jobs/:jobId/cancel',
    guard,
    write((req, body) => cancelJob(env(), jobId(req), body, context(req)))
  );
  router.post(
    '/signin-jobs/:jobId/code',
    guard,
    write((req, body) => submitJobCode(env(), jobId(req), body, context(req)))
  );
  router.get(
    '/signin-command',
    commandGuard,
    read((req) => {
      const query = req.query;
      if (
        Object.keys(query).some((key) => key !== 'provider' && key !== 'profile') ||
        query.provider !== 'antigravity' ||
        typeof query.profile !== 'string' ||
        !/^[a-z][a-z0-9_-]{0,47}$/.test(query.profile)
      ) {
        throw new LifecycleHttpError(400, 'invalid_body');
      }
      if (!env().antigravity) throw new LifecycleHttpError(409, 'not_implemented');
      // A fixed command with the validated profile name; it carries no secret.
      const { host, command } = antigravityTerminal(query.profile);
      return { status: 200, body: { host, command } };
    })
  );
  router.post(
    '/:id/signin-again',
    guard,
    write((req, body) => signInAgain(env(), req.params.id, body, context(req)))
  );
  router.put(
    '/:id/key',
    guard,
    write((req, body) => replaceKey(env(), req.params.id, body, context(req)))
  );
  router.post(
    '/:id/remove',
    guard,
    write((req, body) => removeAccount(env(), req.params.id, body, context(req)))
  );
  router.post(
    '/:id/open',
    guard,
    write((req, body) => openApp(env(), req.params.id, body))
  );
  router.post(
    '/:id/recheck',
    guard,
    write((req, body) => recheck(env(), req.params.id, body))
  );
  router.patch(
    '/:id',
    accountPath,
    guard,
    write((req, body) => relabel(env(), req.params.id, body, context(req)))
  );
  return router;
}

export default createAccountLifecycleRouter();
