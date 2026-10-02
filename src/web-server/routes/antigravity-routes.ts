import { AntigravityError } from '../../antigravity/errors';
import type { Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { callerKey, ConfirmationBindings } from './caller-bound-confirmations';
import { isAntigravityAutoSwitchSettingsPatch } from '../../antigravity/auto-switch/settings';
import { ANTIGRAVITY_AUTO_SWITCH_MESSAGES } from '../../antigravity/auto-switch/monitor';
import type { AntigravityAutoSwitchOutcome } from '../../antigravity/auto-switch/types';
import type { ActivationResult } from '../../antigravity/types';
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
const ACTIVATION_STATUSES = [
  'active',
  'already-active',
  'busy',
  'confirmation-required',
  'stale-confirmation',
  'invalid-profile',
  'unsupported-runtime-probe',
  'deferred',
  'failed-rolled-back',
  'recovery-required',
];
const ACTIVATION_REASONS = [
  'activation-running',
  'running-processes',
  'unreviewed-processes',
  'active-identity-changed',
  'quota-changed',
  'identity-verification-failed',
  'transaction-failed',
  'foreign-replacement',
];

function integer(value: unknown, minimum: number, maximum: number): value is number {
  return (
    typeof value === 'number' && Number.isInteger(value) && value >= minimum && value <= maximum
  );
}

function confirmationToken(value: unknown): value is string {
  return typeof value === 'string' && /^[A-Za-z0-9_-]{16,256}$/.test(value);
}

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

function activationResponse(result: ActivationResult, profileId: string): Record<string, unknown> {
  if (
    result.hostId !== 'ubuntu' ||
    result.profileId !== profileId ||
    !ACTIVATION_STATUSES.includes(result.status)
  )
    throw new AntigravityError('Invalid activation result.');
  const output: Record<string, unknown> = {
    status: result.status,
    profileId,
    hostId: 'ubuntu',
    ...(email(result.email) ? { email: email(result.email) } : {}),
  };
  if (result.reason && ACTIVATION_REASONS.includes(result.reason)) output.reason = result.reason;
  const offer = result.confirmation;
  if (offer) {
    if (
      offer.profileId !== profileId ||
      offer.hostId !== 'ubuntu' ||
      !confirmationToken(offer.token) ||
      !timestamp(offer.expiresAt) ||
      !email(offer.email) ||
      !Array.isArray(offer.processes) ||
      offer.processes.length > 32
    )
      throw new AntigravityError('Invalid activation confirmation.');
    output.confirmation = {
      token: offer.token,
      expiresAt: timestamp(offer.expiresAt),
      profileId,
      hostId: 'ubuntu',
      email: email(offer.email),
      warning:
        'Another Antigravity program is running on Ubuntu. Stop the listed programs, switch accounts, and restore their reviewed sessions?',
      processes: offer.processes.map((process) => {
        const labels = {
          cli: 'Antigravity CLI',
          desktop: 'Antigravity Desktop',
          'language-server': 'Antigravity language server',
        };
        if (
          !integer(process.pid, 1, 2_147_483_647) ||
          !Object.prototype.hasOwnProperty.call(labels, process.role)
        )
          throw new AntigravityError('Invalid activation process.');
        return {
          pid: process.pid,
          role: process.role,
          label: labels[process.role],
        };
      }),
    };
  }
  return output;
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
  originAllowed: DashboardOriginGuard
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
      res.json({
        schemaVersion: 1,
        hostId: 'ubuntu',
        accounts: (await deps.getAccounts({ refresh: req.query.refresh === 'true' })).map(
          publicAccount
        ),
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
  const activate =
    (confirmOnly: boolean) =>
    async (req: Request, res: Response): Promise<void> => {
      if (!writeAllowed(req, res, originAllowed)) return;
      if (
        !safeId(req.params.profileId) ||
        Object.keys(req.query).length ||
        !record(req.body) ||
        Object.keys(req.body).some((key) => !['hostId', 'confirmationToken'].includes(key)) ||
        req.body.hostId !== 'ubuntu' ||
        (req.body.confirmationToken !== undefined &&
          !confirmationToken(req.body.confirmationToken)) ||
        (confirmOnly && !confirmationToken(req.body.confirmationToken))
      ) {
        res.status(400).json({
          error:
            'Select an Antigravity profile on Ubuntu and provide a valid confirmation when required.',
        });
        return;
      }
      if (
        typeof req.body.confirmationToken === 'string' &&
        !confirmations.allows(req.body.confirmationToken, callerKey(req))
      ) {
        // Issued to another device or browser: never consumed here.
        res.status(409).json({
          status: 'stale-confirmation',
          profileId: req.params.profileId,
          hostId: 'ubuntu',
        });
        return;
      }
      let invalidate = false;
      try {
        const result = await deps.activate({
          profileId: req.params.profileId,
          hostId: 'ubuntu',
          mode: 'manual',
          ...(typeof req.body.confirmationToken === 'string'
            ? { confirmationToken: req.body.confirmationToken }
            : {}),
        });
        invalidate = [
          'active',
          'already-active',
          'failed-rolled-back',
          'recovery-required',
        ].includes(result.status);
        const status =
          result.status === 'active' || result.status === 'already-active'
            ? 200
            : result.status === 'invalid-profile'
              ? 400
              : result.status === 'failed-rolled-back' || result.status === 'recovery-required'
                ? 500
                : 409;
        const body = activationResponse(result, req.params.profileId);
        const offer = body.confirmation as { token?: unknown } | undefined;
        if (offer && typeof offer.token === 'string')
          confirmations.record(offer.token, callerKey(req));
        res.status(status).json(body);
      } catch {
        invalidate = true;
        res.status(500).json({
          error:
            'Antigravity account activation failed safely. Refresh the account list before retrying.',
        });
      } finally {
        if (invalidate) deps.invalidateUsage?.();
      }
    };
  router.post('/profiles/:profileId/activate', activate(false));
  router.post('/profiles/:profileId/confirm', activate(true));
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
