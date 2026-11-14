import type { Request, Response } from 'express';
import { createApiRouter } from './api-router';
import { requireDashboardSession } from '../middleware/auth-middleware';
import { getCodexAuthProfilesSummary } from '../../codex-auth/codex-auth-dashboard-service';
import { getCodexProfileQuotas } from '../services/codex-profile-quota-service';

const router = createApiRouter();
// H6: email is PII. Each route checks the session itself (localhost access when
// dashboard auth is disabled) instead of relying on the global guard alone.
const CODEX_PROFILES_ACCESS_ERROR =
  'Codex auth profiles endpoint requires localhost access when dashboard auth is disabled.';

router.get('/profiles', async (req: Request, res: Response): Promise<void> => {
  if (!requireDashboardSession(req, res, CODEX_PROFILES_ACCESS_ERROR)) {
    return;
  }
  try {
    res.json(await getCodexAuthProfilesSummary());
  } catch {
    res.status(500).json({ error: 'Codex profiles could not be read.' });
  }
});

router.get('/profiles/quotas', async (req: Request, res: Response): Promise<void> => {
  if (!requireDashboardSession(req, res, CODEX_PROFILES_ACCESS_ERROR)) {
    return;
  }
  try {
    res.json(await getCodexProfileQuotas());
  } catch {
    res.status(500).json({ error: 'Codex profile usage could not be read safely.' });
  }
});

export default router;
