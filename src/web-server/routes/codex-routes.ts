import type { Request, Response } from 'express';
import { createApiRouter } from './api-router';
import { requireDashboardSession } from '../middleware/auth-middleware';
import {
  composeCodexAuthProfilesSummary,
  getCodexAuthProfilesSummary,
  type CodexAuthProfilesSummary,
} from '../../codex-auth/codex-auth-dashboard-service';
import {
  composeCodexProfileQuotas,
  getCodexProfileQuotas,
  type CodexProfileQuota,
} from '../services/codex-profile-quota-service';
import { composeWithRust } from '../rust-service';
import {
  isRustCodexProfiles,
  isRustCodexQuotas,
  rustCodexProfilesInput,
  rustCodexQuotasInput,
} from '../rust-profile-input';

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
    // Every owner read (registry, saved and live logins) stays in TS; the M3 Rust service may
    // compose the summary from them.
    res.json(
      await getCodexAuthProfilesSummary(
        undefined,
        async (facts) =>
          (await composeWithRust(
            req,
            res,
            '/api/codex/profiles',
            () => rustCodexProfilesInput(facts),
            () => composeCodexAuthProfilesSummary(facts),
            isRustCodexProfiles
          )) as CodexAuthProfilesSummary
      )
    );
  } catch {
    res.status(500).json({ error: 'Codex profiles could not be read.' });
  }
});

router.get('/profiles/quotas', async (req: Request, res: Response): Promise<void> => {
  if (!requireDashboardSession(req, res, CODEX_PROFILES_ACCESS_ERROR)) {
    return;
  }
  try {
    res.json(
      await getCodexProfileQuotas(
        {},
        async (facts) =>
          (await composeWithRust(
            req,
            res,
            '/api/codex/profiles/quotas',
            () => rustCodexQuotasInput(facts),
            () => composeCodexProfileQuotas(facts),
            isRustCodexQuotas
          )) as { profiles: CodexProfileQuota[] }
      )
    );
  } catch {
    res.status(500).json({ error: 'Codex profile usage could not be read safely.' });
  }
});

export default router;
