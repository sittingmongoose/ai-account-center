/**
 * Router factory for every router mounted under /api.
 *
 * - Case-sensitive routing: an API route answers only to its canonical
 *   (lowercase) spelling, so a letter-case variant can never reach a handler
 *   that a case-sensitive path check in a guard did not see.
 * - Request errors stay in the request: Express 4 ignores the promise that an
 *   async handler returns, so a rejection would reach the process-wide
 *   unhandledRejection handler installed in src/ccs.ts, which exits the
 *   dashboard. Every handler registered through these methods forwards a
 *   thrown error or a rejected promise to next(error) instead. Handlers added
 *   through router.route(path) are not wrapped; register them with the
 *   router methods.
 */

import {
  Router,
  type ErrorRequestHandler,
  type NextFunction,
  type Request,
  type Response,
} from 'express';

const REGISTRATION_METHODS = ['use', 'all', 'get', 'post', 'put', 'patch', 'delete'] as const;

type RequestHandlerLike = (req: Request, res: Response, next: NextFunction) => unknown;

function forwardRequestErrors(handler: unknown): unknown {
  if (Array.isArray(handler)) return handler.map(forwardRequestErrors);
  // Paths stay as they are, and so do four-argument error handlers: Express
  // tells them apart from request handlers by their arity.
  if (typeof handler !== 'function' || handler.length > 3) return handler;
  const run = handler as RequestHandlerLike;
  return function forwardErrors(req: Request, res: Response, next: NextFunction): void {
    const fail = (error: unknown) =>
      next(error instanceof Error ? error : new Error('API request handler failed.'));
    try {
      const result = run(req, res, next);
      if (result && typeof (result as PromiseLike<unknown>).then === 'function') {
        (result as PromiseLike<unknown>).then(undefined, fail);
      }
    } catch (error) {
      fail(error);
    }
  };
}

/** A case-sensitive Express router whose handlers can never reject unobserved. */
export function createApiRouter(): Router {
  const router = Router({ caseSensitive: true });
  const methods = router as unknown as Record<string, (...args: unknown[]) => unknown>;
  for (const method of REGISTRATION_METHODS) {
    const register = methods[method];
    methods[method] = (...args: unknown[]) =>
      register.apply(router, args.map(forwardRequestErrors));
  }
  return router;
}

/**
 * Last handler of the /api router: a failed request ends as a generic 500 in
 * its own response, never a stack trace and never a process exit.
 */
export const apiErrorHandler: ErrorRequestHandler = (error, _req, res, next) => {
  if (res.headersSent) {
    // Express closes a response that already started.
    next(error);
    return;
  }
  res.status(500).json({ error: 'The request could not be completed safely.' });
};
