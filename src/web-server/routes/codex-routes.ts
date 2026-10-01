import type { Request, Response } from 'express';
import { Router } from 'express';
import {
  isDashboardWebSocketOriginAllowed,
  requireLocalAccessWhenAuthDisabled,
} from '../middleware/auth-middleware';
import {
  CodexRawConfigConflictError,
  CodexRawConfigValidationError,
  getCodexDashboardDiagnostics,
  getCodexRawConfig,
  patchCodexConfig,
  saveCodexRawConfig,
} from '../services/codex-dashboard-service';
import {
  getCodexAuthProfilesSummary,
  invalidateCodexAuthProfilesCache,
} from '../../codex-auth/codex-auth-dashboard-service';
import {
  activateCodexProfile,
  CodexActivationError,
} from '../../codex-auth/activate-codex-profile';
import { getCodexProfileQuotas } from '../services/codex-profile-quota-service';

const router = Router();
const CODEX_CONFIG_ACCESS_ERROR =
  'Codex configuration endpoints require localhost access when dashboard auth is disabled.';

router.use('/config', (req: Request, res: Response, next) => {
  if (requireLocalAccessWhenAuthDisabled(req, res, CODEX_CONFIG_ACCESS_ERROR)) {
    next();
  }
});

router.get('/diagnostics', async (_req: Request, res: Response): Promise<void> => {
  try {
    res.json(await getCodexDashboardDiagnostics());
  } catch (error) {
    res.status(500).json({ error: (error as Error).message });
  }
});

// H6: email is PII — require localhost access when dashboard auth is disabled.
const CODEX_PROFILES_ACCESS_ERROR =
  'Codex auth profiles endpoint requires localhost access when dashboard auth is disabled.';

router.get('/profiles', async (req: Request, res: Response): Promise<void> => {
  if (!requireLocalAccessWhenAuthDisabled(req, res, CODEX_PROFILES_ACCESS_ERROR)) {
    return;
  }
  try {
    res.json(await getCodexAuthProfilesSummary());
  } catch (error) {
    res.status(500).json({ error: (error as Error).message });
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
  try {
    const activated = await activateCodexProfile(req.params.name);
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
          message:
            'Codex is busy or another account activation is running. Try again when work finishes.',
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
      res.status(failure.status).json({ error: failure.message, code: error.code });
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

router.get('/config/raw', async (_req: Request, res: Response): Promise<void> => {
  try {
    res.json(await getCodexRawConfig());
  } catch (error) {
    res.status(500).json({ error: (error as Error).message });
  }
});

router.put('/config/raw', async (req: Request, res: Response): Promise<void> => {
  try {
    const { rawText, expectedMtime } = req.body ?? {};

    if (typeof rawText !== 'string') {
      res.status(400).json({ error: 'rawText must be a string.' });
      return;
    }
    if (
      expectedMtime !== undefined &&
      (typeof expectedMtime !== 'number' || !Number.isFinite(expectedMtime))
    ) {
      res.status(400).json({ error: 'expectedMtime must be a finite number when provided.' });
      return;
    }

    res.json(await saveCodexRawConfig({ rawText, expectedMtime }));
  } catch (error) {
    if (error instanceof CodexRawConfigValidationError) {
      res.status(400).json({ error: error.message });
      return;
    }
    if (error instanceof CodexRawConfigConflictError) {
      res.status(409).json({ error: error.message, mtime: error.mtime });
      return;
    }
    res.status(500).json({ error: (error as Error).message });
  }
});

router.patch('/config/patch', async (req: Request, res: Response): Promise<void> => {
  try {
    const body = req.body ?? {};
    if (typeof body.kind !== 'string' || body.kind.trim().length === 0) {
      res.status(400).json({ error: 'kind is required.' });
      return;
    }
    if (
      body.expectedMtime !== undefined &&
      (typeof body.expectedMtime !== 'number' || !Number.isFinite(body.expectedMtime))
    ) {
      res.status(400).json({ error: 'expectedMtime must be a finite number when provided.' });
      return;
    }

    res.json(await patchCodexConfig(body));
  } catch (error) {
    if (error instanceof CodexRawConfigValidationError) {
      res.status(400).json({ error: error.message });
      return;
    }
    if (error instanceof CodexRawConfigConflictError) {
      res.status(409).json({ error: error.message, mtime: error.mtime });
      return;
    }
    res.status(500).json({ error: (error as Error).message });
  }
});

export default router;
