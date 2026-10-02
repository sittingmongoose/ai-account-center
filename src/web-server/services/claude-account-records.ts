import {
  CLAUDE_HOSTS,
  parseLauncher,
  type ClaudeHost,
  type ClaudeHostLauncher,
  type PendingClaudeProfile,
} from './claude-account-stores';

/** Claude profiles as the lifecycle sees them: inventory entries with an id, or pending ones. */
export interface ClaudeProfileRecord {
  id: string;
  source: 'inventory' | 'pending';
  label: string;
  email: string | null;
  hosts: Partial<Record<ClaudeHost, ClaudeHostLauncher>>;
  isDefault: boolean;
  /** A host launcher lacks the data folder or ssh alias a host step needs. */
  incomplete: boolean;
  entry: Record<string, unknown>;
}

export interface PublicTrashEntry {
  trashId: string;
  provider: 'claude';
  label: string;
  trashedAt: string;
  purgeAfter: string;
  state: 'trashed' | 'deleting';
}

export function stamp(now: number): string {
  return new Date(now).toISOString().replace(/\.\d{3}Z$/, 'Z');
}

export function trashName(id: string, now: number): string {
  return `${id}-${stamp(now).replace(/[-:]/g, '')}`;
}

function hostLauncher(value: unknown): ClaudeHostLauncher | null {
  if (!value || typeof value !== 'object') return null;
  const raw = value as Record<string, unknown>;
  return parseLauncher(
    Object.fromEntries(
      ['launcherName', 'launcherPath', 'profilePath', 'startMenuPath', 'sshHost']
        .filter((key) => raw[key] !== undefined)
        .map((key) => [key, raw[key]])
    )
  );
}

/**
 * True for the Claude desktop app's own data folder on a computer (`.../Application Support/Claude` on the
 * Mac, `%APPDATA%\Claude` on Windows): the last part of the path, split on `/` and `\`, is `Claude`. Both
 * hosts' file systems ignore case by default, so `claude` names the same folder.
 */
export function isDefaultClaudeDataFolder(profilePath: unknown): boolean {
  if (typeof profilePath !== 'string') return false;
  const last =
    profilePath
      .replace(/[\\/]+$/, '')
      .split(/[\\/]/)
      .pop() ?? '';
  return last.toLowerCase() === 'claude';
}

/**
 * A launcher that is a computer's default Claude profile: the inventory marks it `isDefault`, or its data
 * folder is the app's own (the same rule as the dashboard's `claudeDefaultProfile`), so an entry that lost
 * its flag is still never removed.
 */
function isDefaultLauncher(raw: Record<string, unknown> | undefined | null): boolean {
  return !!raw && (raw.isDefault === true || isDefaultClaudeDataFolder(raw.profilePath));
}

export function inventoryRecord(entry: Record<string, unknown>): ClaudeProfileRecord | null {
  if (typeof entry.id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(entry.id)) return null;
  const hosts: Partial<Record<ClaudeHost, ClaudeHostLauncher>> = {};
  let incomplete = false;
  let isDefault = false;
  for (const host of CLAUDE_HOSTS) {
    const raw = entry[host] as Record<string, unknown> | undefined;
    if (!raw) continue;
    if (isDefaultLauncher(raw)) isDefault = true;
    const launcher = hostLauncher(raw);
    if (launcher) hosts[host] = launcher;
    else incomplete = true;
  }
  const email = typeof entry.email === 'string' ? entry.email : null;
  return {
    id: entry.id,
    source: 'inventory',
    label: email ?? entry.id,
    email,
    hosts,
    isDefault,
    incomplete,
    entry,
  };
}

export function pendingRecord(profile: PendingClaudeProfile): ClaudeProfileRecord {
  return {
    id: profile.id,
    source: 'pending',
    label: profile.label ?? profile.id,
    email: null,
    hosts: { mac: profile.mac, windows: profile.windows },
    // a pending profile is never the app's own folder; if one ever names it, it is protected all the same
    isDefault:
      isDefaultClaudeDataFolder(profile.mac.profilePath) ||
      isDefaultClaudeDataFolder(profile.windows.profilePath),
    incomplete: false,
    entry: { ...profile },
  };
}

/**
 * True when a trash entry's stored profile was a computer's default Claude
 * profile. The default is never put in the trash (Remove refuses it), so this
 * is defence in depth for the purge route.
 */
export function trashEntryIsDefault(entry: {
  source: 'inventory' | 'pending';
  entry: Record<string, unknown>;
}): boolean {
  const raw = entry.entry;
  if (entry.source === 'pending') {
    const mac = raw.mac as Record<string, unknown> | undefined;
    const windows = raw.windows as Record<string, unknown> | undefined;
    return (
      isDefaultClaudeDataFolder(mac?.profilePath) || isDefaultClaudeDataFolder(windows?.profilePath)
    );
  }
  return (
    isDefaultLauncher(raw.mac as Record<string, unknown> | undefined) ||
    isDefaultLauncher(raw.windows as Record<string, unknown> | undefined)
  );
}
