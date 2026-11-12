/** Thin authenticated HTTP/native adapter. Policy and durable coordination live in Rust.
 * A source must read PRIVATE exact-grant snapshots on the credential-owning host and
 * verify the bound identity again immediately before send. Public usage rows are never
 * a source. Until that seam exists the production router is explicitly unavailable.
 */
import { randomBytes } from 'crypto';
import { broadcastResetClaimChange } from '../dashboard-events';
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
/** Fixed native refusal codes the browser may word for the owner. They name no account, grant or host. */
const SAFE_REASONS = new Set([
  'check-again-required',
  'claim-token-outstanding',
  'confirmation-invalid',
  'cooldown',
  'grant-changed',
  'identity-changed',
  'identity-unconfirmed',
  'rate-limited',
  'stale-inventory',
  'provider-read-only',
  'unknown-grant-kind',
  'exact-grant-unavailable',
  'unsupported-grant-id',
  'credential-host-unavailable',
  'grant-unavailable',
  'refill-windows-unconfirmed',
  'signed-out',
]);
/** A native rejection reads `<code>: <message>`; only an allowlisted code is passed on. */
function safeReason(error: unknown): string | null {
  const code = error instanceof Error ? error.message.split(':', 1)[0].trim() : '';
  return SAFE_REASONS.has(code) ? code : null;
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
  async function readSnapshot(account: string): Promise<unknown> {
    const source = deps.source;
    if (!source) throw new ProviderError('Reset source unavailable.', 'reset-claims');
    const snapshot = await source.read(account);
    if (!snapshot || typeof snapshot !== 'object' || Array.isArray(snapshot)) {
      throw new ProviderError('Reset source unavailable.', 'reset-claims');
    }
    return snapshot;
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
      if (!deps.source) {
        res.json({
          ...(await call('pending', account)),
          status: 'unavailable',
          reason: 'credential-host-unavailable',
          grants: [],
        });
        return;
      }
      let snapshot: unknown;
      try {
        snapshot = await readSnapshot(account);
      } catch (error) {
        // Recovery and audit come from one native transaction, including on read failure.
        res.json({
          ...(await call('pending', account)),
          status: 'unavailable',
          reason: safeReason(error) ?? 'reset-inventory-unavailable',
          grants: [],
        });
        return;
      }
      // The native transaction binds the account and returns inventory, recovery, audit and revision together.
      res.json(await call('inventory', account, { snapshot }));
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
      // Set just before the one provider write: a failure after it may have spent the chosen grant.
      let sent = false;
      try {
        if (op !== 'check') {
          await call(op === 'prepare' ? 'prepareGate' : 'claimGate', account, {
            key: req.body.grantKey,
          });
        }
        const snapshot = await readSnapshot(account);
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
        broadcastResetClaimChange();
        let result: { outcome: string; retryAfterMs?: number } = { outcome: 'ambiguous' };
        sent = true;
        try {
          result = await deps.source.send(write);
        } catch {
          // Never retry here: a timeout may have spent the chosen grant.
        }
        let after: unknown = null;
        try {
          after = await readSnapshot(account);
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
      } catch (error) {
        // A refusal's complete native snapshot is ordered like every successful response. Storage failure
        // leaves no authoritative evidence; the client retains its independent send/read obligations.
        let evidence: unknown = null;
        try {
          evidence = (await call('pending', account)).evidence ?? null;
        } catch {
          // Do not invent evidence when recovery storage cannot be read.
        }
        res.status(409).json({
          code: 'reset-state-unavailable',
          action: 'check-again',
          reason: safeReason(error),
          sent,
          evidence,
        });
      } finally {
        // Prepare, settlement, and token burns invalidate every signed-in reset view.
        broadcastResetClaimChange();
      }
    });
  }
  return router;
}
export default createResetClaimRouter({ source: createResetSourceAdapter() });
