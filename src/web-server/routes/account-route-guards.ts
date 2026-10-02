import type { NextFunction, Request, Response } from 'express';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { authKind } from '../middleware/request-auth';
import { isSecureTransport } from '../middleware/secure-transport';

/**
 * Guards shared by the account lifecycle routes (CONTRACT-registry-lifecycle
 * section 1):
 * - every response is `Cache-Control: no-store`;
 * - a browser session is required (401 `auth_required`); a device token gets
 *   403 `device_scope`;
 * - no query string (400 `unexpected_query`) unless a route documents one;
 * - mutations need the dashboard Origin (403 `origin_required`),
 *   application/json (415 `json_required`) and a body of at most 8 KB that a
 *   strict schema accepts (400 `invalid_body`);
 * - keys, authorization codes and device codes need a secure transport
 *   (403 `secure_transport_required`, with a terminal fallback where one exists).
 * Errors are `{error, code}` with fixed sentences; input is never echoed.
 */
export const MAX_LIFECYCLE_BODY_BYTES = 8 * 1024;

export function sendError(
  res: Response,
  status: number,
  code: string,
  error: string,
  extra: Record<string, unknown> = {}
): void {
  res.status(status).json({ error, code, ...extra });
}

/** Mounted in front of the lifecycle routes; `queryAllowed` names routes with a documented query. */
export function accountRouteGuard(queryAllowed: (req: Request) => boolean = () => false) {
  return (req: Request, res: Response, next: NextFunction): void => {
    res.setHeader('Cache-Control', 'no-store');
    const kind = authKind(req);
    if (kind === 'device') {
      sendError(res, 403, 'device_scope', 'This action needs a signed-in dashboard session.');
      return;
    }
    if (kind !== 'session') {
      sendError(res, 401, 'auth_required', 'Authentication required');
      return;
    }
    if (req.originalUrl.includes('?') && !queryAllowed(req)) {
      sendError(res, 400, 'unexpected_query', 'This request does not take a query string.');
      return;
    }
    next();
  };
}

function bodyTooLarge(req: Request): boolean {
  const declared = Number(req.headers['content-length']);
  if (Number.isFinite(declared) && declared > MAX_LIFECYCLE_BODY_BYTES) return true;
  try {
    return Buffer.byteLength(JSON.stringify(req.body ?? null), 'utf8') > MAX_LIFECYCLE_BODY_BYTES;
  } catch {
    return true;
  }
}

/** Origin, JSON and size; returns the body as a plain object, or null after answering. */
export function mutationBody(req: Request, res: Response): Record<string, unknown> | null {
  if (
    typeof req.headers.origin !== 'string' ||
    !req.headers.origin ||
    !isDashboardWebSocketOriginAllowed(req)
  ) {
    sendError(res, 403, 'origin_required', 'This change requires the dashboard origin.');
    return null;
  }
  if (!req.is('application/json')) {
    sendError(res, 415, 'json_required', 'This change requires application/json.');
    return null;
  }
  const body: unknown = req.body;
  if (bodyTooLarge(req) || !body || typeof body !== 'object' || Array.isArray(body)) {
    invalidBody(res);
    return null;
  }
  return body as Record<string, unknown>;
}

export function invalidBody(res: Response): void {
  sendError(res, 400, 'invalid_body', 'The request body is not valid for this action.');
}

/** Exactly these keys: every `required` one and nothing outside `required` + `optional`. */
export function hasOnlyKeys(
  body: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[] = []
): boolean {
  const keys = Object.keys(body);
  return (
    required.every((key) => Object.prototype.hasOwnProperty.call(body, key)) &&
    keys.every((key) => required.includes(key) || optional.includes(key))
  );
}

export interface TerminalFallback {
  kind: 'terminal';
  host: 'ubuntu';
  command: string;
}

/** Rule 5: false after answering 403 when the request did not arrive over a secure transport. */
export function requireSecureTransport(
  req: Request,
  res: Response,
  fallback: TerminalFallback | null = null
): boolean {
  if (isSecureTransport(req)) return true;
  sendError(
    res,
    403,
    'secure_transport_required',
    'Use the secure dashboard address (HTTPS or a tunnel) for keys and sign-in codes.',
    fallback ? { fallback } : {}
  );
  return false;
}
