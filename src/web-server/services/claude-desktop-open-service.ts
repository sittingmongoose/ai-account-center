import { ConfigError, ProfileError } from '../../errors/error-types';
import { getCcsDir } from '../../utils/config-manager';
import {
  canOpenClaudeMacProfile,
  listClaudeDesktopProfiles,
} from './claude-desktop-profile-service';
import { openClaudeMacLauncher } from './claude-desktop-transport';

const pendingOpens = new Map<string, Promise<void>>();
const recentOpens = new Map<string, number>();

/** Resolve a fixed manifest entry and coalesce repeated clicks. Client paths and commands are ignored. */
export async function openClaudeDesktopProfile(id: string): Promise<void> {
  const profile = (await listClaudeDesktopProfiles()).find((entry) => entry.id === id);
  if (!profile) throw new ProfileError('Claude desktop profile was not found.');
  if (!canOpenClaudeMacProfile(profile) || !profile.mac) {
    throw new ConfigError('Claude Mac launcher is not configured.');
  }

  const key = JSON.stringify([getCcsDir(), id, profile.mac.sshHost, profile.mac.launcherPath]);
  const pending = pendingOpens.get(key);
  if (pending) return pending;
  if (Date.now() - (recentOpens.get(key) ?? 0) < 1000) return;

  const opening = openClaudeMacLauncher(profile.mac);
  pendingOpens.set(key, opening);
  try {
    await opening;
    if (recentOpens.size >= 128) recentOpens.clear();
    recentOpens.set(key, Date.now());
  } finally {
    pendingOpens.delete(key);
  }
}
