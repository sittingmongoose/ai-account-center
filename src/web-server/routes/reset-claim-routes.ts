/** Thin authenticated HTTP/native adapter. Policy and durable coordination live in Rust.
 * A source must read PRIVATE exact-grant snapshots on the credential-owning host and
 * verify the bound identity again immediately before send. Public usage rows are never
 * a source. Until that seam exists the production router is explicitly unavailable.
 */
import { randomBytes } from 'crypto';
import { ProviderError } from '../../errors/error-types';
import * as fs from 'fs';
import * as path from 'path';
import type { Router } from 'express';
import type { NativeStatusHandle } from '../../status-native/native-loader';
import { loadNativeStatus } from '../../status-native/native-loader';
import { getCcsDir } from '../../utils/config-manager';
import { authKind } from '../middleware/request-auth';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { callerKey } from './caller-bound-confirmations';
import { createApiRouter } from './api-router';
import { createResetSourceAdapter } from '../services/reset-source-adapter';

export interface ResetSource {
  /** Fresh identity-bound private Snapshot JSON; never include credentials or email. */
  read(account: string): Promise<unknown>;
  /** Must target exactly request.identity and exact grant in request.body; no fallback. */
  send(request: ResetWrite): Promise<{ outcome: string; retryAfterMs?: number }>;
}
export interface ResetWrite {
  method: string;
  body: Record<string, unknown>;
  identity: Record<string, string>;
  attemptKey: string;
  sourceBinding: string;
  routeAccount: string;
}
export interface ResetRouteDeps {
  native?: NativeStatusHandle;
  source?: ResetSource;
  ccsDir?: string;
  now?: () => number;
}
export function createResetClaimRouter(deps: ResetRouteDeps = {}): Router {
  const router = createApiRouter();
  let handle = deps.native;
  const now = deps.now ?? Date.now;
  async function call(op: string, account: string, args: Record<string, unknown> = {}) {
    if (!handle) {
      const loaded = loadNativeStatus();
      if (!loaded.module) throw new ProviderError('Reset backend unavailable.', 'reset-claims');
      handle = new loaded.module.NativeStatus();
    }
    const dir = path.join(deps.ccsDir ?? getCcsDir(), 'account-usage', 'reset-claims');
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    return JSON.parse(
      await handle.call(
        'resetClaims',
        JSON.stringify({
          dbPath: path.join(dir, 'claims.sqlite'),
          op,
          account,
          nowMs: now(),
          entropy: randomBytes(32).toString('hex'),
          ...args,
        })
      )
    );
  }
  router.use('/:accountId/reset-claims', (req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    if (authKind(req) !== 'session') {
      res.status(403).json({ code: 'browser-session-required' });
      return;
    }
    if (req.originalUrl.includes('?')) {
      res.status(400).json({ code: 'unexpected-query' });
      return;
    }
    if (req.method !== 'GET') {
      if (typeof req.headers.origin !== 'string' || !isDashboardWebSocketOriginAllowed(req)) {
        res.status(403).json({ code: 'origin-required' });
        return;
      }
      if (!req.is('application/json')) {
        res.status(415).json({ code: 'json-required' });
        return;
      }
      const body = req.body;
      const fields = req.path.endsWith('/prepare')
        ? ['grantKey']
        : req.path.endsWith('/check')
          ? ['attemptKey']
          : ['grantKey', 'confirmationToken'];
      if (
        !body ||
        typeof body !== 'object' ||
        Array.isArray(body) ||
        Object.keys(body).length !== fields.length ||
        !fields.every((k) => typeof body[k] === 'string' && /^[a-f0-9]{64}$/.test(body[k]))
      ) {
        res.status(400).json({ code: 'invalid-body' });
        return;
      }
    }
    next();
  });
  router.get('/:accountId/reset-claims', async (req, res) => {
    try {
      const account = String(req.params.accountId);
      const history = await call('history', account);
      if (!deps.source) {
        res.json({
          status: 'unavailable',
          reason: 'credential-host-unavailable',
          grants: [],
          history,
        });
        return;
      }
      const snapshot = await deps.source.read(account);
      res.json({ ...(await call('inventory', account, { snapshot })), history });
    } catch {
      res.status(503).json({ status: 'unavailable', code: 'reset-inventory-unavailable' });
    }
  });
  for (const op of ['prepare', 'claim', 'check'] as const) {
    router.post(`/:accountId/reset-claims${op === 'claim' ? '' : `/${op}`}`, async (req, res) => {
      if (!deps.source) {
        res.status(503).json({ code: 'credential-host-unavailable' });
        return;
      }
      const account = String(req.params.accountId);
      try {
        if (op !== 'check') {
          await call(op === 'prepare' ? 'prepareGate' : 'claimGate', account, {
            key: req.body.grantKey,
          });
        }
        const snapshot = await deps.source.read(account);
        if (op === 'prepare') {
          res.json(
            await call('prepare', account, {
              key: req.body.grantKey,
              caller: callerKey(req),
              snapshot,
            })
          );
          return;
        }
        if (op === 'check') {
          res.json(await call('check', account, { key: req.body.attemptKey, snapshot }));
          return;
        }
        const write = (await call('begin', account, {
          key: req.body.grantKey,
          caller: callerKey(req),
          token: req.body.confirmationToken,
          snapshot,
        })) as ResetWrite;
        let result: { outcome: string; retryAfterMs?: number } = { outcome: 'ambiguous' };
        try {
          result = await deps.source.send(write);
        } catch {
          // Never retry here: a timeout may have spent the chosen grant.
        }
        let after: unknown = null;
        try {
          after = await deps.source.read(account);
        } catch {
          // Provider success remains unconfirmed when the post-claim read fails.
        }
        res.json(
          await call('finish', account, {
            key: write.attemptKey,
            outcome: result.outcome,
            retryAfterMs: result.retryAfterMs ?? null,
            snapshot: after,
          })
        );
      } catch {
        // Fixed response: neither native errors nor upstream responses may echo identities.
        res.status(409).json({ code: 'reset-state-unavailable', action: 'check-again' });
      }
    });
  }
  return router;
}
export default createResetClaimRouter({ source: createResetSourceAdapter() });
