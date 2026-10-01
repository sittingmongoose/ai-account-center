import type { Request, Response } from 'express';
import { Router } from 'express';
import {
  isDashboardWebSocketOriginAllowed,
  requireLocalAccessWhenAuthDisabled,
} from '../middleware/auth-middleware';
import {
  getCodexAuthProfilesSummary,
  invalidateCodexAuthProfilesCache,
} from '../../codex-auth/codex-auth-dashboard-service';
import {
  activateCodexProfile,
  CodexActivationError,
} from '../../codex-auth/activate-codex-profile';
import { isCodexActivationBody } from '../../codex-auth/codex-activation-confirmation';
import { getCodexProfileQuotas } from '../services/codex-profile-quota-service';
import {
  getCodexAutoSwitchService,
  isCodexAutoSwitchSettings,
} from '../services/codex-auto-switch-service';

const router = Router();
// H6: email is PII — require localhost access when dashboard auth is disabled.
const CODEX_PROFILES_ACCESS_ERROR =
  'Codex auth profiles endpoint requires localhost access when dashboard auth is disabled.';

router.get('/profiles', async (req: Request, res: Response): Promise<void> => {
  if (!requireLocalAccessWhenAuthDisabled(req, res, CODEX_PROFILES_ACCESS_ERROR)) {
    return;
  }
  try {
    res.json(await getCodexAuthProfilesSummary());
  } catch {
    res.status(500).json({ error: 'Codex profiles could not be read.' });
  }
});

router.get('/profiles/quotas', async (req: Request, res: Response): Promise<void> => {
  if (!requireLocalAccessWhenAuthDisabled(req, res, CODEX_PROFILES_ACCESS_ERROR)) {
    return;
  }
  try {
    res.json(await getCodexProfileQuotas());
  } catch {
    res.status(500).json({ error: 'Codex profile usage could not be read safely.' });
  }
});

router.get('/profiles/auto-switch', (req: Request, res: Response): void => {
  if (req.session?.authenticated !== true) {
    res.status(401).json({ error: 'Authentication required' });
    return;
  }
  res.json(getCodexAutoSwitchService().getStatus());
});

router.put('/profiles/auto-switch', (req: Request, res: Response): void => {
  if (req.session?.authenticated !== true) {
    res.status(401).json({ error: 'Authentication required' });
    return;
  }
  if (!isDashboardWebSocketOriginAllowed(req)) {
    res.status(403).json({ error: 'Codex automatic switching requires the dashboard origin.' });
    return;
  }
  if (!req.is('application/json')) {
    res.status(415).json({ error: 'Codex automatic switching requires application/json.' });
    return;
  }
  if (!isCodexAutoSwitchSettings(req.body)) {
    res
      .status(400)
      .json({ error: 'Provide an enabled boolean or a remaining threshold integer from 1 to 99.' });
    return;
  }
  try {
    res.json(getCodexAutoSwitchService().updateSettings(req.body));
  } catch {
    res
      .status(500)
      .json({ error: 'Codex automatic switching settings could not be saved safely.' });
  }
});

router.post('/profiles/:name/activate', async (req: Request, res: Response): Promise<void> => {
  if (!requireLocalAccessWhenAuthDisabled(req, res, CODEX_PROFILES_ACCESS_ERROR)) {
    return;
  }
  // Apply the dashboard's same-origin policy to writes even when auth is enabled.
  if (!isDashboardWebSocketOriginAllowed(req)) {
    res.status(403).json({ error: 'Codex account activation requires the dashboard origin.' });
    return;
  }
  if (!req.is('application/json')) {
    res.status(415).json({ error: 'Codex account activation requires application/json.' });
    return;
  }
  if (!isCodexActivationBody(req.body)) {
    res.status(400).json({ error: 'Provide an empty object or a valid confirmationToken.' });
    return;
  }
  try {
    const activated = req.body.confirmationToken
      ? await activateCodexProfile(req.params.name, {
          confirmationToken: req.body.confirmationToken,
        })
      : await activateCodexProfile(req.params.name);
    res.json({
      success: true,
      name: activated.name,
      email: activated.email,
      plan: activated.plan,
      codexHome: activated.codexHome,
      previousEmail: activated.previousEmail,
    });
  } catch (error) {
    // Never return raw filesystem, process, or auth.json errors to the browser.
    if (error instanceof CodexActivationError) {
      const failures = {
        busy: {
          status: 409,
          message: error.details?.confirmation
            ? 'Another Codex program is running. Review the warning before stopping and switching.'
            : error.details?.reason === 'activation_running'
              ? 'Another Codex account activation is already running. Wait for it to finish.'
              : 'A running Codex process cannot be safely restarted. Close its owning program and try again.',
        },
        confirmation_stale: {
          status: 409,
          message:
            'The running Codex programs or account changed. Activate again to review a new warning.',
        },
        invalid_profile: { status: 400, message: 'The selected profile has no valid saved login.' },
        invalid_codex_home: {
          status: 400,
          message: 'Account activation requires the shared ~/.codex home.',
        },
        restart_failed: {
          status: 500,
          message: 'Codex could not restart. Check its processes before retrying activation.',
        },
        verification_failed: {
          status: 500,
          message:
            'The activated account could not be verified. Refresh the account list before retrying.',
        },
        auth_read_failed: {
          status: 500,
          message: 'The saved Codex login could not be read safely.',
        },
        auth_write_failed: {
          status: 500,
          message: 'The Codex login could not be installed safely.',
        },
      };
      const failure = failures[error.code];
      const offer = error.details?.confirmation;
      res.status(failure.status).json({
        error: failure.message,
        code: error.code,
        ...(error.details?.reason ? { reason: error.details.reason } : {}),
        ...(offer
          ? {
              confirmation: {
                token: offer.token,
                expiresAt: offer.expiresAt,
                targetProfile: offer.targetProfile,
                processes: offer.processes.map(({ label, pid, role }) => ({ label, pid, role })),
                warning: offer.warning,
              },
            }
          : {}),
      });
      return;
    }
    res.status(500).json({
      error: 'Codex account activation failed. Refresh the account list before retrying.',
    });
  } finally {
    // A restart error may happen after auth.json changed; never retain the old account summary.
    invalidateCodexAuthProfilesCache();
  }
});

export default router;
