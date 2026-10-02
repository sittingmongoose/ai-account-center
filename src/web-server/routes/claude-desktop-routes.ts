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
import { openClaudeDesktopProfile } from '../services/claude-desktop-open-service';
import { ClaudeDesktopTransportError } from '../services/claude-desktop-transport';
import { getClaudeDesktopUsage } from '../services/claude-desktop-usage-service';

const router = createApiRouter();

router.get('/desktop-profiles', async (req, res): Promise<void> => {
  if (!requireDashboardSession(req, res)) return;
  try {
    res.json({ profiles: await listClaudeDesktopProfileMetadata() });
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

  try {
    await openClaudeDesktopProfile(req.params.id, req.body.platform);
    res.json({ opened: true, id: req.params.id, platform: req.body.platform });
  } catch (error) {
    if (error instanceof ProfileError) {
      res.status(404).json({ error: 'Claude desktop profile was not found.' });
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
