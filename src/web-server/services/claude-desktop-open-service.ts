import { ConfigError, ProfileError } from '../../errors/error-types';
import { getCcsDir } from '../../utils/config-manager';
import {
  canOpenClaudeMacProfile,
  canOpenClaudeWindowsProfile,
  listClaudeDesktopProfiles,
} from './claude-desktop-profile-service';
import { openClaudeMacLauncher, openClaudeWindowsLauncher } from './claude-desktop-transport';
import {
  claudeHistoryOpenHeld,
  synchronizeClaudeHistoryBeforeOpen,
} from './claude-history-sync-service';

const pendingOpens = new Map<string, Promise<void>>();
const recentOpens = new Map<string, number>();

/** Resolve a fixed manifest entry and coalesce repeated clicks. Client paths and commands are ignored. */
export async function openClaudeDesktopProfile(
  id: string,
  platform: 'mac' | 'windows' = 'mac'
): Promise<void> {
  const profile = (await listClaudeDesktopProfiles()).find((entry) => entry.id === id);
  if (!profile) throw new ProfileError('Claude desktop profile was not found.');
  if (
    platform === 'mac' ? !canOpenClaudeMacProfile(profile) : !canOpenClaudeWindowsProfile(profile)
  ) {
    throw new ConfigError('Claude desktop launcher is not configured for this platform.');
  }
  const launcher = profile[platform];
  if (!launcher)
    throw new ConfigError('Claude desktop launcher is not configured for this platform.');

  if (claudeHistoryOpenHeld(id, platform))
    throw new ConfigError(
      'Claude history copy is unconfirmed. Verify it has stopped before opening this profile.'
    );

  const key = JSON.stringify([getCcsDir(), id, platform, launcher.sshHost, launcher.launcherPath]);
  const pending = pendingOpens.get(key);
  if (pending) return pending;
  if (Date.now() - (recentOpens.get(key) ?? 0) < 1000) return;

  const opening = (async (): Promise<void> => {
    // Optional neutral index copy runs within this same coalesced authorized
    // Open. It never stops an active profile or resumes a Code session.
    let historySync;
    try {
      historySync = await synchronizeClaudeHistoryBeforeOpen(profile, platform);
    } catch {
      /* Copy is optional. */
    }
    if (
      claudeHistoryOpenHeld(id, platform) ||
      historySync?.reason === 'create_only_transaction_unconfirmed'
    )
      throw new ConfigError(
        'Claude history copy is unconfirmed. Verify it has stopped before opening this profile.'
      );
    if (platform === 'mac') await openClaudeMacLauncher(launcher);
    else await openClaudeWindowsLauncher(launcher, id);
  })();
  pendingOpens.set(key, opening);
  try {
    await opening;
    if (recentOpens.size >= 128) recentOpens.clear();
    recentOpens.set(key, Date.now());
  } finally {
    pendingOpens.delete(key);
  }
}
