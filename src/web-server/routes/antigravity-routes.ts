import type { Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import type {
  AntigravityApiDependencies,
  AntigravityInventory,
} from '../../antigravity/usage-contract';
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

/**
 * GET /profiles' answer for the runtime's inventory: the runtime owns the read; the M3 Rust
 * service may compose the public copy. GET /api/views/home (D7) composes Home's copy the same way.
 */
function publicInventoryAnswer(req: Request, res: Response, inventory: AntigravityInventory) {
  return composeWithRust(
    req,
    res,
    '/api/antigravity/profiles',
    () => rustAntigravityInventoryInput(inventory),
    () => publicInventory(inventory),
    isRustAntigravityInventory
  );
}

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
      res.json(await publicInventoryAnswer(req, res, await deps.getInventory()));
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

/** The runtime's inventory, or the empty Ubuntu one when no runtime runs. Home's fed path reads it too (R1). */
export async function runtimeInventory(): Promise<AntigravityInventory> {
  return (
    getAntigravityRuntime()?.getInventory() ?? {
      schemaVersion: 1,
      hostId: 'ubuntu',
      profiles: [],
    }
  );
}

/**
 * Home's Antigravity inventory (D7): GET /profiles' answer, composed the same way. `read` is a runtime read the
 * caller already made (R1's fed path), so its fallback answers from the same read.
 */
export async function readAntigravityInventory(
  req: Request,
  res: Response,
  read?: { value: unknown }
): Promise<unknown> {
  return publicInventoryAnswer(
    req,
    res,
    read ? (read.value as AntigravityInventory) : await runtimeInventory()
  );
}

export default createAntigravityRouter({
  getInventory: runtimeInventory,
  getAccounts: (options) => getAntigravityRuntime()?.getAccounts(options) ?? Promise.resolve([]),
});
