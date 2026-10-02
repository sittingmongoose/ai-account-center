import { createApiRouter } from './api-router';
import { ConfigError, ProfileError } from '../../errors/error-types';
import {
  isDashboardWebSocketOriginAllowed,
  requireDashboardSession,
} from '../middleware/auth-middleware';
import {
  CLAUDE_WINDOWS_PROFILE_IDS,
  listClaudeDesktopProfileMetadata,
} from '../services/claude-desktop-profile-service';
import {
  assertClaudeDesktopOpenAllowed,
  CLAUDE_HISTORY_UNCONFIRMED_MESSAGE,
  ClaudeHistoryOpenHeldError,
  claudeOpenUsesManagedHistory,
  openClaudeDesktopProfile,
} from '../services/claude-desktop-open-service';
import { getClaudeOpenOperations } from '../services/claude-open-operations';
import { ClaudeDesktopTransportError } from '../services/claude-desktop-transport';
import { getClaudeDesktopUsage } from '../services/claude-desktop-usage-service';
import { getCcsDir } from '../../utils/config-manager';

const router = createApiRouter();

router.get('/desktop-profiles', async (req, res): Promise<void> => {
  if (!requireDashboardSession(req, res)) return;
  try {
    const operations = getClaudeOpenOperations();
    const scope = getCcsDir();
    // `openOperation` is the progress of a 202 Open (null when none is known). Only a
    // profile with a manifest id can be opened, so only those rows carry the field.
    const profiles = (await listClaudeDesktopProfileMetadata()).map((profile) =>
      profile.id ? { ...profile, openOperation: operations.forProfile(scope, profile.id) } : profile
    );
    res.json({ profiles });
  } catch {
    res.status(500).json({ error: 'Claude desktop profiles could not be read safely.' });
  }
});

router.get('/desktop-profiles/usage', async (req, res): Promise<void> => {
  if (!requireDashboardSession(req, res)) return;
  const platform = req.query.platform;
  if (platform !== 'mac' && platform !== 'windows') {
    res.status(400).json({ error: 'Select a valid Claude desktop platform.' });
    return;
  }
  try {
    res.json(await getClaudeDesktopUsage(platform));
  } catch {
    res.status(500).json({ error: 'Claude desktop usage could not be read safely.' });
  }
});

router.post('/desktop-profiles/:id/open', async (req, res): Promise<void> => {
  if (req.session?.authenticated !== true) {
    res.status(401).json({ error: 'Authentication required' });
    return;
  }
  if (!isDashboardWebSocketOriginAllowed(req)) {
    res.status(403).json({ error: 'Claude account launch requires the dashboard origin.' });
    return;
  }
  if (!req.is('application/json')) {
    res.status(415).json({ error: 'Claude account launch requires application/json.' });
    return;
  }
  if (
    !/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(req.params.id) ||
    !req.body ||
    (req.body.platform !== 'mac' && req.body.platform !== 'windows') ||
    (req.body.platform === 'windows' && !CLAUDE_WINDOWS_PROFILE_IDS.has(req.params.id)) ||
    Object.keys(req.body).some((key) => key !== 'platform')
  ) {
    res.status(400).json({ error: 'Select a configured Claude desktop account and platform.' });
    return;
  }

  const id = req.params.id;
  const platform: 'mac' | 'windows' = req.body.platform;
  const scope = getCcsDir();
  const operations = getClaudeOpenOperations();
  // A click while its Open still runs joins it; the copy's own hold must not refuse it.
  const running = operations.running(scope, id, platform);
  if (running) {
    res.status(202).json({ id, platform, state: running.state, operationId: running.id });
    return;
  }
  try {
    if (!(await claudeOpenUsesManagedHistory(id, platform))) {
      // No managed history copy: the ordinary Open, answered when it is done.
      await openClaudeDesktopProfile(id, platform);
      res.json({ opened: true, id, platform });
      return;
    }
    // The same refusals as before, answered before any work starts.
    await assertClaudeDesktopOpenAllowed(id, platform);
    const operation = operations.start(scope, id, platform, (observer) =>
      openClaudeDesktopProfile(id, platform, observer)
    );
    res.status(202).json({ id, platform, state: operation.state, operationId: operation.id });
  } catch (error) {
    if (error instanceof ProfileError) {
      res.status(404).json({ error: 'Claude desktop profile was not found.' });
    } else if (error instanceof ClaudeHistoryOpenHeldError) {
      // Same 409 as before, now with the reason the clients should show.
      res
        .status(409)
        .json({ error: CLAUDE_HISTORY_UNCONFIRMED_MESSAGE, code: 'history_unconfirmed' });
    } else if (error instanceof ConfigError) {
      res
        .status(409)
        .json({ error: 'Claude desktop launcher is not configured for this platform.' });
    } else if (error instanceof ClaudeDesktopTransportError) {
      res.status(error.timedOut ? 504 : 502).json({ error: error.message });
    } else {
      res.status(500).json({ error: 'Claude account could not be opened safely.' });
    }
  }
});

export default router;
