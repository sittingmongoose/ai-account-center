import { AntigravityError } from '../../antigravity/errors';
import type { Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { isNativeVersion } from '../../antigravity/native-version';
import { publicRuntimeServiceProblem } from '../../antigravity/runtime-health';
import type {
  AntigravityApiDependencies,
  AntigravityInventory,
} from '../../antigravity/usage-contract';
import {
  displayText,
  email,
  safeId,
  timestamp,
  publicDashboardAccount as publicAccount,
  publicDashboardAccount,
} from '../../antigravity/usage-normalization';
import { markPassedResets } from '../services/account-window-reset';

function runtimeServiceProblemField(value: unknown): Partial<AntigravityInventory> {
  const problem = publicRuntimeServiceProblem(value);
  return problem ? { runtimeServiceProblem: problem } : {};
}

/** All public returns are rebuilt so adapter-private fields cannot leak accidentally. */
export function publicInventory(value: AntigravityInventory): AntigravityInventory {
  if (value.hostId !== 'ubuntu' || !Array.isArray(value.profiles) || value.profiles.length > 16)
    throw new AntigravityError('Invalid Antigravity inventory.');
  const ids = new Set<string>();
  return {
    schemaVersion: 1,
    hostId: 'ubuntu',
    ...(value.nativeUpdatePaused
      ? {
          nativeUpdatePaused: {
            installedVersion: isNativeVersion(value.nativeUpdatePaused.installedVersion)
              ? value.nativeUpdatePaused.installedVersion
              : null,
          },
        }
      : {}),
    ...runtimeServiceProblemField(value.runtimeServiceProblem),
    profiles: value.profiles.map((profile) => {
      if (
        !safeId(profile.id) ||
        ids.has(profile.id) ||
        !email(profile.email) ||
        profile.hostId !== 'ubuntu'
      )
        throw new AntigravityError('Invalid Antigravity inventory.');
      ids.add(profile.id);
      return {
        id: profile.id,
        email: email(profile.email) as string,
        plan: displayText(profile.plan, 80),
        hostId: 'ubuntu',
        available: profile.available === true,
        selected: profile.selected === true,
        runtimeVerified: profile.runtimeVerified === true,
        verifiedAt: timestamp(profile.verifiedAt),
      };
    }),
  };
}

export { publicDashboardAccount as publicAccount };

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
      res.json(publicInventory(await deps.getInventory()));
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
      res.json({
        schemaVersion: 1,
        hostId: 'ubuntu',
        accounts: accounts.map((account) => markPassedResets(publicAccount(account), at)),
      });
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
