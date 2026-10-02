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

export function inventoryRecord(entry: Record<string, unknown>): ClaudeProfileRecord | null {
  if (typeof entry.id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(entry.id)) return null;
  const hosts: Partial<Record<ClaudeHost, ClaudeHostLauncher>> = {};
  let incomplete = false;
  let isDefault = false;
  for (const host of CLAUDE_HOSTS) {
    const raw = entry[host] as Record<string, unknown> | undefined;
    if (!raw) continue;
    if (raw.isDefault === true) isDefault = true;
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
    isDefault: false,
    incomplete: false,
    entry: { ...profile },
  };
}
