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
  loadClaudeHistoryPolicy,
  synchronizeClaudeHistoryBeforeOpen,
  type ClaudeHistorySyncObserver,
} from './claude-history-sync-service';

const pendingOpens = new Map<string, Promise<void>>();
const recentOpens = new Map<string, number>();

export const CLAUDE_HISTORY_UNCONFIRMED_MESSAGE =
  'Claude history copy is unconfirmed. Verify it has stopped before opening this profile.';

/** Open is held by an unconfirmed history copy (a durable marker); never retried here. */
export class ClaudeHistoryOpenHeldError extends ConfigError {
  constructor() {
    super(CLAUDE_HISTORY_UNCONFIRMED_MESSAGE);
    this.name = 'ClaudeHistoryOpenHeldError';
  }
}

/** Progress of one Open, for the 202 progress view. Counts and states only. */
export interface ClaudeOpenObserver extends ClaudeHistorySyncObserver {
  /** The optional history copy finished; `createdCount` is its confirmed record count. */
  synchronized?: (result: { status: string; createdCount: number }) => void;
  /** The launcher is about to run. */
  opening?: () => void;
}

function notify(callback: () => void): void {
  try {
    callback();
  } catch {
    /* Progress is informational only. */
  }
}

async function resolveOpenTarget(id: string, platform: 'mac' | 'windows') {
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
  if (claudeHistoryOpenHeld(id, platform)) throw new ClaudeHistoryOpenHeldError();
  return { profile, launcher };
}

/**
 * Whether this Open would run the managed history copy first (a verified
 * private policy exists for the profile). False whenever that is not certain;
 * the ordinary Open then reports any refusal exactly as before.
 */
export async function claudeOpenUsesManagedHistory(
  id: string,
  platform: 'mac' | 'windows'
): Promise<boolean> {
  try {
    const { profile } = await resolveOpenTarget(id, platform);
    return (await loadClaudeHistoryPolicy(profile)) !== null;
  } catch {
    return false;
  }
}

/** The same refusals as an Open (404 profile, 409 launcher or held copy), without opening. */
export async function assertClaudeDesktopOpenAllowed(
  id: string,
  platform: 'mac' | 'windows'
): Promise<void> {
  await resolveOpenTarget(id, platform);
}

/** Resolve a fixed manifest entry and coalesce repeated clicks. Client paths and commands are ignored. */
export async function openClaudeDesktopProfile(
  id: string,
  platform: 'mac' | 'windows' = 'mac',
  observer: ClaudeOpenObserver = {}
): Promise<void> {
  const { profile, launcher } = await resolveOpenTarget(id, platform);

  const key = JSON.stringify([getCcsDir(), id, platform, launcher.sshHost, launcher.launcherPath]);
  const pending = pendingOpens.get(key);
  if (pending) return pending;
  if (Date.now() - (recentOpens.get(key) ?? 0) < 1000) return;

  const opening = (async (): Promise<void> => {
    // Optional neutral index copy runs within this same coalesced authorized
    // Open. It never stops an active profile or resumes a Code session.
    let historySync;
    try {
      historySync = await synchronizeClaudeHistoryBeforeOpen(profile, platform, observer);
    } catch {
      /* Copy is optional. */
    }
    if (historySync) {
      const { status, createdCount } = historySync;
      notify(() => observer.synchronized?.({ status, createdCount }));
    }
    if (
      claudeHistoryOpenHeld(id, platform) ||
      historySync?.reason === 'create_only_transaction_unconfirmed'
    )
      throw new ClaudeHistoryOpenHeldError();
    notify(() => observer.opening?.());
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
