import { promises as fs } from 'fs';
import path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import { ValidationError } from '../../errors/error-types';

export interface ClaudeDesktopLauncher {
  launcherName: string;
  launcherPath?: string;
  profilePath?: string;
  isDefault?: boolean;
  sshHost?: string;
}

export interface ClaudeWindowsDesktopLauncher extends ClaudeDesktopLauncher {
  startMenuPath?: string;
}

export interface ClaudeDesktopProfile {
  id?: string;
  email: string;
  mac?: ClaudeDesktopLauncher;
  windows?: ClaudeWindowsDesktopLauncher;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function readText(value: unknown, maxLength: number): string {
  if (
    typeof value !== 'string' ||
    !value.trim() ||
    value.length > maxLength ||
    /[\u0000-\u001f\u007f]/.test(value)
  ) {
    throw new ValidationError('Invalid desktop profile metadata');
  }
  return value;
}

function readLauncher(value: unknown, windows: boolean): ClaudeWindowsDesktopLauncher {
  if (!isRecord(value)) {
    throw new ValidationError('Invalid desktop launcher metadata');
  }

  const launcher: ClaudeWindowsDesktopLauncher = {
    launcherName: readText(value.launcherName, 255),
  };
  // Remote paths come only from this private inventory; requests cannot override them.
  for (const field of ['launcherPath', 'profilePath'] as const) {
    if (value[field] !== undefined) launcher[field] = readText(value[field], 4096);
  }
  if (windows && value.startMenuPath !== undefined) {
    launcher.startMenuPath = readText(value.startMenuPath, 4096);
  }
  if (value.isDefault !== undefined) {
    if (typeof value.isDefault !== 'boolean') {
      throw new ValidationError('Invalid desktop launcher default metadata');
    }
    launcher.isDefault = value.isDefault;
  }
  if (value.sshHost !== undefined) {
    const host = readText(value.sshHost, 128);
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(host)) {
      throw new ValidationError('Invalid desktop SSH alias metadata');
    }
    launcher.sshHost = host;
  }
  return launcher;
}

function readProfile(value: unknown): ClaudeDesktopProfile {
  if (!isRecord(value)) {
    throw new ValidationError('Invalid desktop profile metadata');
  }
  const email = readText(value.email, 254);
  if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
    throw new ValidationError('Invalid desktop profile email');
  }

  const profile: ClaudeDesktopProfile = { email };
  if (value.id !== undefined) {
    const id = readText(value.id, 64);
    if (!/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(id)) {
      throw new ValidationError('Invalid desktop profile identifier');
    }
    profile.id = id;
  }
  if (value.mac !== undefined) profile.mac = readLauncher(value.mac, false);
  if (value.windows !== undefined) profile.windows = readLauncher(value.windows, true);
  if (!profile.mac && !profile.windows) {
    throw new ValidationError('Missing desktop launcher metadata');
  }
  return profile;
}

/** Read a private launcher inventory, without reading Claude credentials or live app state. */
export async function listClaudeDesktopProfiles(): Promise<ClaudeDesktopProfile[]> {
  let contents: string;
  try {
    contents = await fs.readFile(path.join(getCcsDir(), 'claude-desktop-profiles.json'), 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return [];
    throw error;
  }

  if (contents.length > 1024 * 1024)
    throw new ValidationError('Desktop profile manifest is too large');
  const manifest: unknown = JSON.parse(contents);
  if (
    !isRecord(manifest) ||
    manifest.version !== 1 ||
    !Array.isArray(manifest.profiles) ||
    manifest.profiles.length > 1000
  ) {
    throw new ValidationError('Invalid desktop profile manifest');
  }
  const profiles = manifest.profiles.map(readProfile);
  const ids = profiles.flatMap((profile) => (profile.id ? [profile.id] : []));
  if (new Set(ids).size !== ids.length) {
    throw new ValidationError('Duplicate desktop profile identifier');
  }
  return profiles;
}

export function canOpenClaudeMacProfile(profile: ClaudeDesktopProfile): boolean {
  return Boolean(
    profile.id &&
      profile.mac?.sshHost &&
      profile.mac.launcherPath?.startsWith('/') &&
      profile.mac.launcherPath.endsWith('.app')
  );
}

export const CLAUDE_WINDOWS_PROFILE_IDS = new Set(['platyr', 'gmail', 'party', 'me']);

export function canOpenClaudeWindowsProfile(profile: ClaudeDesktopProfile): boolean {
  return Boolean(
    profile.id && CLAUDE_WINDOWS_PROFILE_IDS.has(profile.id) && profile.windows?.sshHost
  );
}

function publicLauncher(launcher: ClaudeDesktopLauncher): Omit<ClaudeDesktopLauncher, 'sshHost'> {
  const result: Omit<ClaudeDesktopLauncher, 'sshHost'> = { launcherName: launcher.launcherName };
  for (const field of ['launcherPath', 'profilePath', 'isDefault'] as const) {
    const value = launcher[field];
    if (value !== undefined) Object.assign(result, { [field]: value });
  }
  return result;
}

/** Browser metadata never includes the private SSH transport configuration. */
export async function listClaudeDesktopProfileMetadata() {
  return (await listClaudeDesktopProfiles()).map((profile) => ({
    ...(profile.id ? { id: profile.id } : {}),
    email: profile.email,
    ...(profile.mac
      ? {
          mac: {
            ...publicLauncher(profile.mac),
            ...(profile.id ? { canOpen: canOpenClaudeMacProfile(profile) } : {}),
          },
        }
      : {}),
    ...(profile.windows
      ? {
          windows: {
            ...publicLauncher(profile.windows),
            ...(profile.id ? { canOpen: canOpenClaudeWindowsProfile(profile) } : {}),
            ...(profile.windows.startMenuPath
              ? { startMenuPath: profile.windows.startMenuPath }
              : {}),
            ...(profile.id ? { launchUri: `ccs-claude://launch/${profile.id}` } : {}),
          },
        }
      : {}),
  }));
}
