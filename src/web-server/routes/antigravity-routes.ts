import { AntigravityError } from '../../antigravity/errors';
import type { Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { registerAntigravityActivateRoutes } from './antigravity-activate-route';
import { registerAntigravityRecoverRoute } from './antigravity-recover-route';
import { authKind } from '../middleware/request-auth';
import { ConfirmationBindings } from './caller-bound-confirmations';
import { isAntigravityAutoSwitchSettingsPatch } from '../../antigravity/auto-switch/settings';
import { isNativeVersion } from '../../antigravity/native-version';
import { ANTIGRAVITY_AUTO_SWITCH_MESSAGES } from '../../antigravity/auto-switch/monitor';
import type { AntigravityAutoSwitchOutcome } from '../../antigravity/auto-switch/types';
import type {
  AntigravityApiDependencies,
  AntigravityAutoSettings,
  AntigravityInventory,
} from '../../antigravity/usage-contract';
import {
  displayText,
  email,
  record,
  safeId,
  timestamp,
  publicDashboardAccount as publicAccount,
  publicDashboardAccount,
} from '../../antigravity/usage-normalization';
import { markPassedResets } from '../services/account-window-reset';

export type DashboardOriginGuard = (req: Request) => boolean;
const AUTO_KEYS = [
  'enabled',
  'thresholdUsedPercent',
  'pollIntervalSeconds',
  'maxQuotaAgeSeconds',
  'cooldownSeconds',
  'selectedHostIds',
  'requestedPoolId',
];
const AUTO_OUTCOMES = Object.keys(ANTIGRAVITY_AUTO_SWITCH_MESSAGES);

export function isAutoSettingsPatch(value: unknown): value is Partial<AntigravityAutoSettings> {
  return isAntigravityAutoSwitchSettingsPatch(value);
}

/** All public returns are rebuilt so adapter-private fields cannot leak accidentally. */
export function publicInventory(value: AntigravityInventory): AntigravityInventory {
  if (value.hostId !== 'ubuntu' || !Array.isArray(value.profiles) || value.profiles.length > 16)
    throw new AntigravityError('Invalid Antigravity inventory.');
  const ids = new Set<string>();
  return {
    schemaVersion: 1,
    hostId: 'ubuntu',
    ...(typeof value.activationSupported === 'boolean'
      ? { activationSupported: value.activationSupported }
      : {}),
    ...(value.nativeUpdatePaused
      ? {
          nativeUpdatePaused: {
            installedVersion: isNativeVersion(value.nativeUpdatePaused.installedVersion)
              ? value.nativeUpdatePaused.installedVersion
              : null,
          },
        }
      : {}),
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

export function publicAutoStatus(value: unknown): Record<string, unknown> {
  if (!record(value)) throw new AntigravityError('Invalid automatic switching status.');
  const settings: Record<string, unknown> = {};
  for (const key of AUTO_KEYS)
    if (Object.prototype.hasOwnProperty.call(value, key)) settings[key] = value[key];
  if (
    !isAutoSettingsPatch(settings) ||
    AUTO_KEYS.some((key) => !Object.prototype.hasOwnProperty.call(settings, key))
  )
    throw new AntigravityError('Invalid automatic switching status.');
  const outcome =
    typeof value.outcome === 'string' && AUTO_OUTCOMES.includes(value.outcome)
      ? (value.outcome as AntigravityAutoSwitchOutcome)
      : 'error';
  return {
    ...settings,
    selectedHostIds: ['ubuntu'],
    outcome,
    message: ANTIGRAVITY_AUTO_SWITCH_MESSAGES[outcome],
    activationInProgress: value.activationInProgress === true,
    lastCheckedAt: timestamp(value.lastCheckedAt),
    lastSwitchedAt: timestamp(value.lastSwitchedAt),
    lastProfileId: safeId(value.lastProfileId) ? value.lastProfileId : null,
    lastHostId: value.lastHostId === 'ubuntu' ? 'ubuntu' : null,
  };
}

/** A browser session, or a paired device token on a tray route (the /api guard checks scope). */
function authenticated(req: Request, res: Response): boolean {
  if (authKind(req) !== null) return true;
  res.status(401).json({ error: 'Authentication required' });
  return false;
}

function writeAllowed(req: Request, res: Response, originAllowed: DashboardOriginGuard): boolean {
  if (!authenticated(req, res)) return false;
  if (!originAllowed(req)) {
    res.status(403).json({
      error: 'Antigravity account controls require the dashboard origin.',
    });
    return false;
  }
  if (!req.is('application/json')) {
    res.status(415).json({
      error: 'Antigravity account controls require application/json.',
    });
    return false;
  }
  return true;
}

/** Pass the existing isDashboardWebSocketOriginAllowed guard at integration. */
export function createAntigravityRouter(
  deps: AntigravityApiDependencies,
  originAllowed: DashboardOriginGuard,
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
  router.get('/auto-switch', (req, res) => {
    if (!authenticated(req, res)) return;
    if (Object.keys(req.query).length) {
      res.status(400).json({ error: 'Unexpected settings query.' });
      return;
    }
    try {
      res.json(publicAutoStatus(deps.getAutoSwitchStatus()));
    } catch {
      res.status(500).json({
        error: 'Antigravity automatic switching status is unavailable.',
      });
    }
  });
  const confirmations = new ConfirmationBindings();
  router.put('/auto-switch', (req, res) => {
    if (!writeAllowed(req, res, originAllowed)) return;
    if (Object.keys(req.query).length || !isAutoSettingsPatch(req.body)) {
      res.status(400).json({
        error: 'Provide valid Antigravity automatic switching settings for Ubuntu.',
      });
      return;
    }
    try {
      res.json(publicAutoStatus(deps.updateAutoSwitchSettings(req.body)));
    } catch {
      res.status(500).json({
        error: 'Antigravity automatic switching settings could not be saved safely.',
      });
    }
  });
  registerAntigravityActivateRoutes(
    router,
    deps,
    (req, res) => writeAllowed(req, res, originAllowed),
    confirmations
  );
  registerAntigravityRecoverRoute(router, deps, (req, res) =>
    writeAllowed(req, res, originAllowed)
  );
  return router;
}

import {
  getAntigravityRuntime,
  getAntigravityAutoSwitchStatus,
} from '../../antigravity/runtime-service';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';

export default createAntigravityRouter(
  {
    getInventory: async () =>
      getAntigravityRuntime()?.getInventory() ?? {
        schemaVersion: 1,
        hostId: 'ubuntu',
        profiles: [],
        activationSupported: false,
      },
    getAccounts: (options) => getAntigravityRuntime()?.getAccounts(options) ?? Promise.resolve([]),
    activate: (request) =>
      getAntigravityRuntime()?.activate(request) ??
      Promise.resolve({
        status: 'unsupported-runtime-probe',
        profileId: request.profileId,
        hostId: 'ubuntu',
      }),
    recover: () =>
      getAntigravityRuntime()?.recover() ??
      Promise.resolve({ status: 'no-recovery-pending', hostId: 'ubuntu' }),
    getAutoSwitchStatus: getAntigravityAutoSwitchStatus,
    updateAutoSwitchSettings: (patch) => {
      const runtime = getAntigravityRuntime();
      if (!runtime) throw new AntigravityError('Antigravity switching is not configured.');
      return runtime.updateAutoSwitchSettings(patch);
    },
    invalidateUsage: () => getAntigravityRuntime()?.invalidateUsage?.(),
  },
  isDashboardWebSocketOriginAllowed
);
