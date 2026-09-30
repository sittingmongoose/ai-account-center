import { Router } from 'express';
import { listClaudeDesktopProfiles } from '../services/claude-desktop-profile-service';

const router = Router();

router.get('/desktop-profiles', async (_req, res): Promise<void> => {
  try {
    res.json({ profiles: await listClaudeDesktopProfiles() });
  } catch {
    res.status(500).json({ error: 'Claude desktop profiles could not be read safely.' });
  }
});

export default router;
