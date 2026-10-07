/**
 * The T3 usage hub: a read-only, CLIProxyAPI-compatible facade so T3 Code can
 * show subscription quota bars for every AAC-managed Codex and Claude account.
 * Mounted at `/v0/management` on the dashboard's own listener (T3 drops any
 * path prefix from a hub URL, see usage-hub-contract.ts).
 *
 * It answers exactly two requests, from the dashboard's cache only:
 * - `GET /v0/management/auth-files`: one entry per Codex and Claude account;
 * - `POST /v0/management/api-call`: only the Codex `wham/usage` and Claude
 *   `oauth/usage` reads T3 makes, answered with the cached reading in the
 *   upstream shape. It never proxies: no upstream request is ever made, no
 *   header T3 sends is used, and every other URL, method or body is refused.
 *
 * A request that came through the LAN HTTPS proxy (`dashboard_tls.trusted_proxy:
 * lan-https-proxy`, decided by the socket peer alone) is answered 404 before
 * any of this, so the hub never faces the internet; the proxy should also block
 * the path.
 *
 * Guard, in order (every answer is JSON and `no-store`):
 * 1. 120 requests per minute per client address (429 `rate_limited`);
 * 2. 10 refused keys per 15 minutes per client address (429 `rate_limited`);
 * 3. a key in the query string: 400 `key_in_query` (keys travel in headers only);
 * 4. no key configured: 404 `usage_hub_off`; an unreadable key file: 503;
 * 5. the dashboard's secure-transport rule (CONTRACT-auth-devices 2a,
 *    `isSecureTransport`): loopback with a loopback Host, in-process TLS, a
 *    trusted local TLS proxy, or a peer in `dashboard_network.trusted_networks`
 *    while `dashboard_network.trust_local_network` is on. Anything else is 403
 *    `secure_transport_required`, before the key is looked at;
 * 6. the key, from `Authorization: Bearer` or `X-Management-Key`, compared in
 *    constant time with the stored SHA-256: 401 `missing_management_key` or
 *    `invalid_management_key`.
 *
 * The key is separate from the dashboard password and device tokens: it opens
 * only these read-only routes, and dashboard sessions or device tokens never
 * open them.
 */
import type { NextFunction, Request, Response, Router } from 'express';
import rateLimit from 'express-rate-limit';
import { createLogger } from '../../services/logging';
import { rateLimitClientKey } from '../middleware/rate-limit-keys';
import { isLanProxyPeer, isSecureTransport } from '../middleware/secure-transport';
import { getDashboardTlsSettings } from '../services/dashboard-tls-config';
import { createApiRouter } from '../routes/api-router';
import {
  CLAUDE_USAGE_URL,
  CODEX_CREDITS_URL,
  CODEX_USAGE_URL,
  type UsageHubApiCallRequest,
  type UsageHubApiCallResponse,
  type UsageHubAuthFiles,
} from './usage-hub-contract';
import { createUsageHubAccountSource, type UsageHubAccountSource } from './usage-hub-accounts';
import {
  readUsageHubKeyState,
  usageHubKeyMatches,
  USAGE_HUB_KEY_PREFIX,
  type UsageHubKeyState,
} from './usage-hub-key-store';

const logger = createLogger('usage-hub');

export const USAGE_HUB_REQUESTS_PER_MINUTE = 120;
export const USAGE_HUB_FAILURES_PER_WINDOW = 10;
const ONE_MINUTE = 60 * 1000;
const FIFTEEN_MINUTES = 15 * ONE_MINUTE;
const MAX_AUTH_INDEX_LENGTH = 128;

export const USAGE_HUB_ENABLE_HINT =
  'Turn it on with: ai-account-center dashboard usage-hub generate --stdout';

export interface UsageHubRouterDeps {
  accounts?: UsageHubAccountSource;
  readKeyState?: () => Promise<UsageHubKeyState>;
  isSecure?: (req: Request) => boolean;
  /** True when the request came through the LAN HTTPS proxy (then 404). Tests only. */
  isProxied?: (req: Request) => boolean;
  requestsPerMinute?: number;
  failuresPerWindow?: number;
}

function send(res: Response, status: number, body: unknown): void {
  res.status(status).json(body);
}

function refuse(res: Response, status: number, code: string, error: string): void {
  send(res, status, { error, code });
}

function singleHeader(value: string | string[] | undefined): string | undefined {
  return Array.isArray(value) ? (value.length === 1 ? value[0] : undefined) : value;
}

/** The presented key: `Authorization: Bearer <key>` first, else `X-Management-Key`. */
export function presentedManagementKey(req: Pick<Request, 'headers'>): string | null {
  const authorization = singleHeader(req.headers.authorization);
  if (authorization !== undefined) {
    const match = /^\s*bearer\s+(\S+)\s*$/i.exec(authorization);
    return match ? match[1] : '';
  }
  const header = singleHeader(req.headers['x-management-key']);
  return header === undefined ? null : header.trim();
}

function queryCarriesKey(req: Request): boolean {
  const url = typeof req.originalUrl === 'string' ? req.originalUrl : (req.url ?? '');
  const query = url.includes('?') ? url.slice(url.indexOf('?')) : '';
  let decoded = query;
  try {
    decoded = decodeURIComponent(query);
  } catch {
    /* Keep the raw query. */
  }
  return decoded.includes(USAGE_HUB_KEY_PREFIX) || query.includes(USAGE_HUB_KEY_PREFIX);
}

function audit(level: 'debug' | 'warn', event: string, message: string, reason: string): void {
  try {
    logger[level](event, message, { reason });
  } catch {
    /* Logging never changes the answer. */
  }
}

function limited(message: string) {
  return (req: Request, res: Response): void => {
    const reset = (req as Request & { rateLimit?: { resetTime?: Date } }).rateLimit?.resetTime;
    const seconds = Math.max(1, reset ? Math.ceil((reset.getTime() - Date.now()) / 1000) : 60);
    res.setHeader('Retry-After', String(seconds));
    send(res, 429, { error: message, code: 'rate_limited', retryAfterSeconds: seconds });
  };
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

const JSON_HEADER: Record<string, string[]> = { 'Content-Type': ['application/json'] };

export function createUsageHubRouter(deps: UsageHubRouterDeps = {}): Router {
  const router = createApiRouter();
  const accounts = deps.accounts ?? createUsageHubAccountSource();
  const readKeyState = deps.readKeyState ?? readUsageHubKeyState;
  const isSecure = deps.isSecure ?? ((req: Request) => isSecureTransport(req));
  const isProxied = deps.isProxied ?? ((req: Request) => isLanProxyPeer(req));

  router.use((_req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    next();
  });
  router.use((req: Request, res: Response, next: NextFunction) => {
    if (!isProxied(req)) {
      next();
      return;
    }
    audit('warn', 'usage_hub.refused', 'A usage hub request was refused', 'lan_proxy');
    refuse(res, 404, 'not_found', 'The usage hub does not answer through a reverse proxy.');
  });
  router.use(
    rateLimit({
      windowMs: ONE_MINUTE,
      limit: deps.requestsPerMinute ?? USAGE_HUB_REQUESTS_PER_MINUTE,
      keyGenerator: rateLimitClientKey,
      standardHeaders: true,
      legacyHeaders: false,
      handler: limited('Too many usage hub requests. Try again later.'),
    })
  );
  router.use(
    rateLimit({
      windowMs: FIFTEEN_MINUTES,
      limit: deps.failuresPerWindow ?? USAGE_HUB_FAILURES_PER_WINDOW,
      standardHeaders: false,
      legacyHeaders: false,
      keyGenerator: rateLimitClientKey,
      skipSuccessfulRequests: true,
      requestWasSuccessful: (_req, res) => res.statusCode !== 401,
      handler: limited('Too many wrong usage hub keys. Try again later.'),
    })
  );

  router.use(async (req: Request, res: Response, next: NextFunction) => {
    if (queryCarriesKey(req)) {
      refuse(res, 400, 'key_in_query', 'Send the management key only in a request header.');
      return;
    }
    const keyState = await readKeyState();
    if (keyState.state === 'off') {
      refuse(res, 404, 'usage_hub_off', `The usage hub is off. ${USAGE_HUB_ENABLE_HINT}`);
      return;
    }
    if (keyState.state === 'invalid') {
      refuse(res, 503, 'usage_hub_key_unreadable', 'The usage hub key cannot be read safely.');
      return;
    }
    if (!isSecure(req)) {
      audit('warn', 'usage_hub.refused', 'A usage hub request was refused', 'insecure_transport');
      send(res, 403, {
        error:
          'Use loopback, HTTPS or a trusted local network address for the usage hub (dashboard_network.trust_local_network).',
        code: 'secure_transport_required',
        secureOrigin: getDashboardTlsSettings().publicOrigin,
      });
      return;
    }
    const presented = presentedManagementKey(req);
    if (presented === null || presented.length === 0) {
      audit('warn', 'usage_hub.refused', 'A usage hub request was refused', 'missing_key');
      refuse(res, 401, 'missing_management_key', 'A management key is required.');
      return;
    }
    if (!usageHubKeyMatches(presented, keyState.record)) {
      audit('warn', 'usage_hub.refused', 'A usage hub request was refused', 'invalid_key');
      refuse(res, 401, 'invalid_management_key', 'The management key is not valid.');
      return;
    }
    next();
  });

  router.get('/auth-files', async (_req: Request, res: Response) => {
    const list = await accounts.read();
    const body: UsageHubAuthFiles = { files: list.map((account) => account.authFile) };
    send(res, 200, body);
  });

  router.post('/api-call', async (req: Request, res: Response) => {
    const body: unknown = req.body;
    if (!isPlainObject(body)) {
      refuse(res, 400, 'invalid_request', 'Send the api-call as a JSON object.');
      return;
    }
    const request = body as UsageHubApiCallRequest;
    const authIndex = request.auth_index;
    if (
      typeof authIndex !== 'string' ||
      authIndex.length === 0 ||
      authIndex.length > MAX_AUTH_INDEX_LENGTH
    ) {
      refuse(res, 400, 'invalid_request', 'auth_index must be a string from auth-files.');
      return;
    }
    const unsupported = (reason: string, error: string): void => {
      audit('debug', 'usage_hub.api_call_refused', 'A usage hub api-call was refused', reason);
      refuse(res, 403, 'unsupported_api_call', error);
    };
    const method = request.method === undefined ? 'GET' : request.method;
    if (
      typeof method !== 'string' ||
      method.toUpperCase() !== 'GET' ||
      request.data !== undefined
    ) {
      unsupported(
        'method',
        'AI Account Center answers only GET usage reads. It is not a proxy and changes nothing.'
      );
      return;
    }
    const url = request.url;
    if (typeof url === 'string' && url.startsWith(CODEX_CREDITS_URL)) {
      unsupported('reset_credits', 'AI Account Center does not offer Codex reset credits to T3.');
      return;
    }
    if (url !== CODEX_USAGE_URL && url !== CLAUDE_USAGE_URL) {
      unsupported(
        'url',
        'AI Account Center answers only the Codex and Claude usage reads T3 makes. It is not a proxy.'
      );
      return;
    }
    const account = (await accounts.read()).find(
      (candidate) => candidate.authFile.auth_index === authIndex
    );
    if (!account) {
      refuse(res, 404, 'unknown_auth_index', 'No account has this auth_index. List auth-files.');
      return;
    }
    const expected = account.authFile.provider === 'codex' ? CODEX_USAGE_URL : CLAUDE_USAGE_URL;
    if (url !== expected) {
      unsupported('provider', 'This usage read does not match the account provider.');
      return;
    }
    const answer: UsageHubApiCallResponse = account.usage
      ? { status_code: 200, header: JSON_HEADER, body: JSON.stringify(account.usage) }
      : {
          status_code: 503,
          header: JSON_HEADER,
          body: JSON.stringify({
            error: {
              type: 'no_cached_reading',
              message: account.authFile.status_message,
            },
          }),
        };
    send(res, 200, answer);
  });

  router.post('/reset-quota', (_req: Request, res: Response) => {
    refuse(
      res,
      403,
      'unsupported_api_call',
      'AI Account Center does not reset quotas for T3. It only reports usage.'
    );
  });

  router.all('*', (_req: Request, res: Response) => {
    refuse(
      res,
      404,
      'not_found',
      'AI Account Center offers only auth-files and api-call usage reads here.'
    );
  });

  router.use((error: unknown, _req: Request, res: Response, next: NextFunction) => {
    if (res.headersSent) {
      next(error);
      return;
    }
    refuse(res, 500, 'internal_error', 'The usage hub could not answer safely.');
  });

  return router;
}
