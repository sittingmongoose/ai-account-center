import fs from 'fs/promises';
import path from 'path';
import { ConfigError } from '../../errors/error-types';
import {
  readPrivateJsonFile,
  withPrivateFileLock,
  writePrivateJsonFile,
} from './private-json-store';

/**
 * Claude profile stores owned by the lifecycle routes
 * (CONTRACT-registry-lifecycle section 3.4):
 * - `accounts/claude-pending.json`: a new profile has no email until its first
 *   reading, and the version 1 inventory requires one, so it waits here;
 * - `accounts/trash.json`: removed profiles, kept 30 days. Host trash folder
 *   names and launcher paths never leave the server;
 * - the version 1 inventory `claude-desktop-profiles.json` is edited only by
 *   removing or appending one whole entry; every other entry and key is kept.
 * Lock order is trash, then pending, then inventory, so nesting never deadlocks.
 */
export type ClaudeHost = 'mac' | 'windows';
export const CLAUDE_HOSTS: readonly ClaudeHost[] = ['mac', 'windows'];
export const CLAUDE_PROFILE_ID = /^[a-z][a-z0-9-]{1,31}$/;
export const TRASH_ID = /^tr_[a-f0-9]{16}$/;
const TRASH_NAME = /^[a-z][a-z0-9-]{1,31}-\d{8}T\d{6}Z$/;
const SSH_ALIAS = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const TIMESTAMP = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{3})?Z$/;
const MAX_STORE_BYTES = 256 * 1024;
const MAX_INVENTORY_BYTES = 1024 * 1024;
const MAX_ENTRIES = 64;

export interface ClaudeHostLauncher {
  launcherName: string;
  launcherPath?: string;
  profilePath: string;
  startMenuPath?: string;
  sshHost: string;
}

export interface PendingClaudeProfile {
  id: string;
  label: string | null;
  mac: ClaudeHostLauncher;
  windows: ClaudeHostLauncher;
  createdAt: string;
  /** The account the user said they would sign in as; verified on first reading. */
  expectedEmail: string | null;
}

export interface ClaudeTrashHost {
  trashName: string;
  launcher: ClaudeHostLauncher;
}

export interface ClaudeTrashEntry {
  trashId: string;
  provider: 'claude';
  accountId: string;
  label: string;
  source: 'inventory' | 'pending';
  hosts: Partial<Record<ClaudeHost, ClaudeTrashHost>>;
  /** The removed inventory or pending entry, restored as it was. */
  entry: Record<string, unknown>;
  trashedAt: string;
  purgeAfter: string;
  state: 'trashed' | 'deleting';
}

export function accountsDir(ccsDir: string): string {
  return path.join(ccsDir, 'accounts');
}
export function pendingFile(ccsDir: string): string {
  return path.join(accountsDir(ccsDir), 'claude-pending.json');
}
export function trashFile(ccsDir: string): string {
  return path.join(accountsDir(ccsDir), 'trash.json');
}
export function inventoryFile(ccsDir: string): string {
  return path.join(ccsDir, 'claude-desktop-profiles.json');
}

function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

function text(value: unknown, max: number): value is string {
  return (
    typeof value === 'string' &&
    value.trim().length > 0 &&
    value.length <= max &&
    !/[\u0000-\u001f\u007f]/.test(value)
  );
}

export function parseLauncher(value: unknown): ClaudeHostLauncher | null {
  if (!record(value)) return null;
  const allowed = ['launcherName', 'launcherPath', 'profilePath', 'startMenuPath', 'sshHost'];
  if (Object.keys(value).some((key) => !allowed.includes(key))) return null;
  if (!text(value.launcherName, 255) || !text(value.profilePath, 4096)) return null;
  if (typeof value.sshHost !== 'string' || !SSH_ALIAS.test(value.sshHost)) return null;
  for (const optional of ['launcherPath', 'startMenuPath'] as const) {
    if (value[optional] !== undefined && !text(value[optional], 4096)) return null;
  }
  return { ...(value as unknown as ClaudeHostLauncher) };
}

function parsePending(value: unknown): PendingClaudeProfile[] | null {
  if (!record(value) || value.version !== 1 || !Array.isArray(value.profiles)) return null;
  if (value.profiles.length > MAX_ENTRIES) return null;
  const profiles: PendingClaudeProfile[] = [];
  for (const item of value.profiles) {
    if (!record(item) || typeof item.id !== 'string' || !CLAUDE_PROFILE_ID.test(item.id)) {
      return null;
    }
    const mac = parseLauncher(item.mac);
    const windows = parseLauncher(item.windows);
    // Entries written before the email assertion carry no expectedEmail.
    const expectedEmail =
      item.expectedEmail === undefined || item.expectedEmail === null
        ? null
        : text(item.expectedEmail, 254)
          ? (item.expectedEmail as string)
          : undefined;
    if (
      !mac ||
      !windows ||
      (item.label !== null && !text(item.label, 96)) ||
      typeof item.createdAt !== 'string' ||
      !TIMESTAMP.test(item.createdAt) ||
      expectedEmail === undefined ||
      profiles.some((profile) => profile.id === item.id)
    ) {
      return null;
    }
    profiles.push({
      id: item.id,
      label: item.label as string | null,
      mac,
      windows,
      createdAt: item.createdAt,
      expectedEmail,
    });
  }
  return profiles;
}

function parseTrash(value: unknown): ClaudeTrashEntry[] | null {
  if (!record(value) || value.version !== 1 || !Array.isArray(value.entries)) return null;
  if (value.entries.length > MAX_ENTRIES) return null;
  const entries: ClaudeTrashEntry[] = [];
  for (const item of value.entries) {
    if (
      !record(item) ||
      typeof item.trashId !== 'string' ||
      !TRASH_ID.test(item.trashId) ||
      item.provider !== 'claude' ||
      typeof item.accountId !== 'string' ||
      !/^claude:[a-z][a-z0-9-]{1,31}$/.test(item.accountId) ||
      !text(item.label, 254) ||
      (item.source !== 'inventory' && item.source !== 'pending') ||
      !record(item.hosts) ||
      !record(item.entry) ||
      typeof item.trashedAt !== 'string' ||
      !TIMESTAMP.test(item.trashedAt) ||
      typeof item.purgeAfter !== 'string' ||
      !TIMESTAMP.test(item.purgeAfter) ||
      (item.state !== 'trashed' && item.state !== 'deleting')
    ) {
      return null;
    }
    const hosts: Partial<Record<ClaudeHost, ClaudeTrashHost>> = {};
    for (const [host, detail] of Object.entries(item.hosts)) {
      if (host !== 'mac' && host !== 'windows') return null;
      const launcher = record(detail) ? parseLauncher(detail.launcher) : null;
      if (!launcher || !record(detail) || typeof detail.trashName !== 'string') return null;
      if (!TRASH_NAME.test(detail.trashName)) return null;
      hosts[host] = { trashName: detail.trashName, launcher };
    }
    entries.push({ ...(item as unknown as ClaudeTrashEntry), hosts });
  }
  return entries;
}

/** Pending profiles; an unreadable file is an error, never an empty list. */
export async function readPendingProfiles(ccsDir: string): Promise<PendingClaudeProfile[]> {
  const read = await readPrivateJsonFile(pendingFile(ccsDir), MAX_STORE_BYTES);
  if (read.state === 'absent') return [];
  const profiles = read.state === 'ok' ? parsePending(read.value) : null;
  if (!profiles) throw new ConfigError('Pending Claude profiles could not be read safely.');
  return profiles;
}

export async function readTrash(ccsDir: string): Promise<ClaudeTrashEntry[]> {
  const read = await readPrivateJsonFile(trashFile(ccsDir), MAX_STORE_BYTES);
  if (read.state === 'absent') return [];
  const entries = read.state === 'ok' ? parseTrash(read.value) : null;
  if (!entries) throw new ConfigError('The account trash could not be read safely.');
  return entries;
}

export function updatePendingProfiles<T>(
  ccsDir: string,
  mutate: (profiles: PendingClaudeProfile[]) => Promise<{ next: PendingClaudeProfile[]; result: T }>
): Promise<T> {
  const file = pendingFile(ccsDir);
  return withPrivateFileLock(file, async () => {
    const { next, result } = await mutate(await readPendingProfiles(ccsDir));
    if (!parsePending({ version: 1, profiles: next })) {
      throw new ConfigError('Pending Claude profiles could not be saved safely.');
    }
    await writePrivateJsonFile(file, { version: 1, profiles: next });
    return result;
  });
}

export function updateTrash<T>(
  ccsDir: string,
  mutate: (entries: ClaudeTrashEntry[]) => Promise<{ next: ClaudeTrashEntry[]; result: T }>
): Promise<T> {
  const file = trashFile(ccsDir);
  return withPrivateFileLock(file, async () => {
    const { next, result } = await mutate(await readTrash(ccsDir));
    if (!parseTrash({ version: 1, entries: next })) {
      throw new ConfigError('The account trash could not be saved safely.');
    }
    await writePrivateJsonFile(file, { version: 1, entries: next });
    return result;
  });
}

/** The raw inventory document; absent is an empty version 1 document. */
async function readInventoryDocument(ccsDir: string): Promise<Record<string, unknown>> {
  let contents: string;
  try {
    const stat = await fs.lstat(inventoryFile(ccsDir));
    if (!stat.isFile() || stat.size > MAX_INVENTORY_BYTES) throw new ConfigError('invalid');
    contents = await fs.readFile(inventoryFile(ccsDir), 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return { version: 1, profiles: [] };
    throw new ConfigError('The Claude profile list could not be read safely.');
  }
  let document: unknown;
  try {
    document = JSON.parse(contents);
  } catch {
    throw new ConfigError('The Claude profile list could not be read safely.');
  }
  if (!record(document) || document.version !== 1 || !Array.isArray(document.profiles)) {
    throw new ConfigError('The Claude profile list could not be read safely.');
  }
  return document;
}

/**
 * Change the inventory's profile list under its lock. The callback gets the raw
 * entries and returns the next list; everything else in the file is kept.
 */
export function updateInventory<T>(
  ccsDir: string,
  mutate: (profiles: Record<string, unknown>[]) => { next: Record<string, unknown>[]; result: T }
): Promise<T> {
  const file = inventoryFile(ccsDir);
  return withPrivateFileLock(file, async () => {
    const document = await readInventoryDocument(ccsDir);
    const profiles = (document.profiles as unknown[]).filter(record);
    if (profiles.length !== (document.profiles as unknown[]).length) {
      throw new ConfigError('The Claude profile list could not be read safely.');
    }
    const { next, result } = mutate(profiles.map((profile) => ({ ...profile })));
    await writePrivateJsonFile(file, { ...document, profiles: next });
    return result;
  });
}

/** Raw inventory entries (ids and launchers), for id checks and trash records. */
export async function readInventoryEntries(ccsDir: string): Promise<Record<string, unknown>[]> {
  const document = await readInventoryDocument(ccsDir);
  return (document.profiles as unknown[]).filter(record);
}
