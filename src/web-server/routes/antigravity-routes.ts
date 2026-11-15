import type { Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import type { AntigravityApiDependencies } from '../../antigravity/usage-contract';
import { publicDashboardAccount } from '../../antigravity/usage-normalization';
import { publicInventory, publicQuotas } from '../../antigravity/public-reads';
import { composeWithRust } from '../rust-service';
import {
  isRustAntigravityInventory,
  isRustAntigravityQuotas,
  rustAntigravityInventoryInput,
  rustAntigravityQuotasInput,
} from '../rust-profile-input';

export { publicDashboardAccount as publicAccount, publicInventory, publicQuotas };

/** A browser session, or a paired device token on a tray route (the /api guard checks scope). */
function authenticated(req: Request, res: Response): boolean {
  if (authKind(req) !== null) return true;
  res.status(401).json({ error: 'Authentication required' });
  return false;
}

/** The read-only Antigravity routes: saved profiles and their usage. */
export function createAntigravityRouter(
  deps: AntigravityApiDependencies,
  now: () => number = Date.now
): Router {
  const router = createApiRouter();
  router.use((_req, res, next) => {
    res.setHeader('Cache-Control', 'no-store');
    next();
  });
  router.get('/profiles', async (req, res) => {
    if (!authenticated(req, res)) return;
    if (Object.keys(req.query).length) {
      res.status(400).json({ error: 'Unexpected inventory query.' });
      return;
    }
    try {
      // The runtime owns the read; the M3 Rust service may compose the public copy.
      const inventory = await deps.getInventory();
      res.json(
        await composeWithRust(
          req,
          res,
          '/api/antigravity/profiles',
          () => rustAntigravityInventoryInput(inventory),
          () => publicInventory(inventory),
          isRustAntigravityInventory
        )
      );
    } catch {
      res.status(500).json({ error: 'Antigravity accounts could not be read safely.' });
    }
  });
  router.get('/profiles/quotas', async (req, res) => {
    if (!authenticated(req, res)) return;
    if (
      Object.keys(req.query).some((key) => key !== 'refresh') ||
      (req.query.refresh !== undefined &&
        req.query.refresh !== 'true' &&
        req.query.refresh !== 'false')
    ) {
      res.status(400).json({ error: 'Select a valid refresh value.' });
      return;
    }
    try {
      const accounts = await deps.getAccounts({ refresh: req.query.refresh === 'true' });
      // One instant per response, so every account is judged against the same clock.
      const at = now();
      res.json(
        await composeWithRust(
          req,
          res,
          '/api/antigravity/profiles/quotas',
          () => rustAntigravityQuotasInput(accounts, at),
          () => publicQuotas(accounts, at),
          isRustAntigravityQuotas
        )
      );
    } catch {
      res.status(500).json({ error: 'Antigravity usage could not be read safely.' });
    }
  });
  return router;
}

import { getAntigravityRuntime } from '../../antigravity/runtime-service';

export default createAntigravityRouter({
  getInventory: async () =>
    getAntigravityRuntime()?.getInventory() ?? {
      schemaVersion: 1,
      hostId: 'ubuntu',
      profiles: [],
    },
  getAccounts: (options) => getAntigravityRuntime()?.getAccounts(options) ?? Promise.resolve([]),
});
