import path from 'path';
import type { DashboardProvider } from './account-dashboard-types';
import { isDashboardProviderId } from './dashboard-provider-table';
import {
  readPrivateJsonFile,
  withPrivateFileLock,
  writePrivateJsonFile,
} from './private-json-store';

/**
 * Display-only visibility (CONTRACT-registry-lifecycle sections 3.3 and 4),
 * `~/.ccs/account-visibility.json`, 0600, at most 32 KB. `hiddenProviders` and
 * `hiddenAccountIds` hide providers and accounts on the dashboard;
 * `trayHiddenProviders` and `trayHiddenAccountIds` hide them in the trays.
 * The dashboard lists and the tray lists are independent: changing one never
 * changes the other. Hidden accounts keep being collected and stay
 * auto-switch candidates.
 */
export const ACCOUNT_VISIBILITY_FILE = 'account-visibility.json';
/** Room for both 128-id lists at their longest, plus both provider lists. */
export const MAX_VISIBILITY_BYTES = 32 * 1024;
export const MAX_HIDDEN_ACCOUNT_IDS = 128;
/** Bound on the raw provider list before duplicates are dropped. */
const MAX_HIDDEN_PROVIDER_ENTRIES = 32;

/** Public account id shapes (section 1, rule 8), plus the OpenCode console wallet. */
const ACCOUNT_ID = /^[a-z0-9][a-z0-9-]{0,31}(?::[A-Za-z0-9@._+-]{1,128}){1,2}$/;
const WALLET_ACCOUNT_ID = /^plan-opencode-go-console-mac-[a-f0-9]{12}$/;

export interface AccountVisibility {
  hiddenProviders: DashboardProvider[];
  hiddenAccountIds: string[];
  /** Providers the trays do not show; independent of hiddenProviders. */
  trayHiddenProviders: DashboardProvider[];
  /** Accounts the trays do not show; independent of hiddenAccountIds. */
  trayHiddenAccountIds: string[];
}

/** The PUT body: any non-empty subset of the four stored lists. */
export type AccountVisibilityUpdate = Partial<AccountVisibility>;

export type AccountVisibilityRead =
  | { state: 'ok'; visibility: AccountVisibility }
  | { state: 'unavailable' };

export function isPublicAccountId(value: unknown): value is string {
  return typeof value === 'string' && (ACCOUNT_ID.test(value) || WALLET_ACCOUNT_ID.test(value));
}

function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

function unique<T>(values: T[]): T[] {
  return [...new Set(values)];
}

/** The PUT body keys; a key the body leaves out keeps its stored list. */
const VISIBILITY_KEYS = [
  'hiddenProviders',
  'hiddenAccountIds',
  'trayHiddenProviders',
  'trayHiddenAccountIds',
] as const;
/** The optional stored keys: a file written before them keeps reading. */
const OPTIONAL_FILE_KEYS = ['trayHiddenProviders', 'trayHiddenAccountIds'] as const;

/** The bound and the known-provider check shared by both provider lists. */
function parseProviderList(value: unknown): DashboardProvider[] | null {
  if (
    !Array.isArray(value) ||
    value.length > MAX_HIDDEN_PROVIDER_ENTRIES ||
    !value.every(isDashboardProviderId)
  ) {
    return null;
  }
  return unique(value as DashboardProvider[]);
}

/** The bound and the id shapes shared by both account id lists. */
function parseIdList(value: unknown): string[] | null {
  if (
    !Array.isArray(value) ||
    value.length > MAX_HIDDEN_ACCOUNT_IDS ||
    !value.every(isPublicAccountId)
  ) {
    return null;
  }
  return unique(value as string[]);
}

/**
 * The PUT body: a non-empty subset of `{hiddenProviders, hiddenAccountIds,
 * trayHiddenProviders, trayHiddenAccountIds}`. Unknown keys, unknown
 * providers, malformed ids, more than 32 provider entries and more than 128
 * ids in either id list are rejected. Duplicates are dropped; absent account
 * ids are kept.
 */
export function parseVisibilityBody(value: unknown): AccountVisibilityUpdate | null {
  if (
    !record(value) ||
    Object.keys(value).length === 0 ||
    !Object.keys(value).every((key) => (VISIBILITY_KEYS as readonly string[]).includes(key))
  ) {
    return null;
  }
  const update: AccountVisibilityUpdate = {};
  if ('hiddenProviders' in value) {
    const providers = parseProviderList(value.hiddenProviders);
    if (providers === null) return null;
    update.hiddenProviders = providers;
  }
  if ('trayHiddenProviders' in value) {
    const providers = parseProviderList(value.trayHiddenProviders);
    if (providers === null) return null;
    update.trayHiddenProviders = providers;
  }
  if ('hiddenAccountIds' in value) {
    const ids = parseIdList(value.hiddenAccountIds);
    if (ids === null) return null;
    update.hiddenAccountIds = ids;
  }
  if ('trayHiddenAccountIds' in value) {
    const ids = parseIdList(value.trayHiddenAccountIds);
    if (ids === null) return null;
    update.trayHiddenAccountIds = ids;
  }
  return update;
}

/**
 * The stored file. `trayHiddenProviders` and `trayHiddenAccountIds` are
 * optional so a file written before the tray lists existed keeps reading. A
 * provider that has left the table since it was hidden is dropped quietly;
 * an unknown key or anything else malformed makes the file unavailable.
 */
function parseVisibilityFile(value: unknown): AccountVisibility | null {
  if (!record(value) || value.version !== 1) return null;
  const allowed = new Set<string>(['version', 'hiddenProviders', 'hiddenAccountIds']);
  for (const key of OPTIONAL_FILE_KEYS) allowed.add(key);
  const keys = Object.keys(value);
  if (!keys.every((key) => allowed.has(key))) return null;
  if (!('hiddenProviders' in value) || !('hiddenAccountIds' in value)) return null;
  const hasTrayProviders = 'trayHiddenProviders' in value;
  const hasTrayIds = 'trayHiddenAccountIds' in value;
  const providerKeys = hasTrayProviders
    ? (['hiddenProviders', 'trayHiddenProviders'] as const)
    : (['hiddenProviders'] as const);
  for (const key of providerKeys) {
    const list = value[key];
    if (!Array.isArray(list) || !list.every((id) => typeof id === 'string')) return null;
  }
  const parsed = parseVisibilityBody({
    hiddenProviders: (value.hiddenProviders as string[]).filter(isDashboardProviderId),
    hiddenAccountIds: value.hiddenAccountIds,
    ...(hasTrayProviders
      ? {
          trayHiddenProviders: (value.trayHiddenProviders as string[]).filter(
            isDashboardProviderId
          ),
        }
      : {}),
    ...(hasTrayIds ? { trayHiddenAccountIds: value.trayHiddenAccountIds } : {}),
  });
  if (!parsed) return null;
  return {
    hiddenProviders: parsed.hiddenProviders ?? [],
    hiddenAccountIds: parsed.hiddenAccountIds ?? [],
    trayHiddenProviders: parsed.trayHiddenProviders ?? [],
    trayHiddenAccountIds: parsed.trayHiddenAccountIds ?? [],
  };
}

export function accountVisibilityPath(ccsDir: string): string {
  return path.join(ccsDir, ACCOUNT_VISIBILITY_FILE);
}

/** Absent means nothing is hidden; an unsafe or malformed file is unavailable, never empty. */
export async function readAccountVisibility(ccsDir: string): Promise<AccountVisibilityRead> {
  const read = await readPrivateJsonFile(accountVisibilityPath(ccsDir), MAX_VISIBILITY_BYTES);
  if (read.state === 'absent') {
    return {
      state: 'ok',
      visibility: {
        hiddenProviders: [],
        hiddenAccountIds: [],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [],
      },
    };
  }
  if (read.state === 'invalid') return { state: 'unavailable' };
  const visibility = parseVisibilityFile(read.value);
  return visibility ? { state: 'ok', visibility } : { state: 'unavailable' };
}

/** Full replacement under the file mutex; also repairs an unreadable file. */
export function writeAccountVisibility(
  ccsDir: string,
  visibility: AccountVisibility
): Promise<AccountVisibility> {
  const file = accountVisibilityPath(ccsDir);
  return withPrivateFileLock(file, async () => {
    const saved = {
      hiddenProviders: [...visibility.hiddenProviders],
      hiddenAccountIds: [...visibility.hiddenAccountIds],
      trayHiddenProviders: [...visibility.trayHiddenProviders],
      trayHiddenAccountIds: [...visibility.trayHiddenAccountIds],
    };
    await writePrivateJsonFile(file, { version: 1, ...saved });
    return saved;
  });
}

export type AccountVisibilityUpdateResult =
  | { state: 'saved'; visibility: AccountVisibility }
  | { state: 'unavailable' };

/**
 * The PUT semantics: every list the update names replaces the stored one; a
 * list it leaves out is unchanged, so a dashboard change never touches the
 * tray lists and a tray change never touches the dashboard lists. The merge reads under the same file mutex,
 * so two concurrent PUTs cannot clobber each other's lists. A partial update
 * against a file that cannot be read safely writes nothing; a full one
 * replaces and repairs the file, exactly as before.
 */
export async function updateAccountVisibility(
  ccsDir: string,
  update: AccountVisibilityUpdate
): Promise<AccountVisibilityUpdateResult> {
  if (
    update.hiddenProviders !== undefined &&
    update.hiddenAccountIds !== undefined &&
    update.trayHiddenProviders !== undefined &&
    update.trayHiddenAccountIds !== undefined
  ) {
    const visibility = await writeAccountVisibility(ccsDir, {
      hiddenProviders: update.hiddenProviders,
      hiddenAccountIds: update.hiddenAccountIds,
      trayHiddenProviders: update.trayHiddenProviders,
      trayHiddenAccountIds: update.trayHiddenAccountIds,
    });
    return { state: 'saved', visibility };
  }
  const file = accountVisibilityPath(ccsDir);
  return withPrivateFileLock(file, async (): Promise<AccountVisibilityUpdateResult> => {
    const current = await readAccountVisibility(ccsDir);
    if (current.state !== 'ok') return { state: 'unavailable' };
    const saved: AccountVisibility = {
      hiddenProviders: [...(update.hiddenProviders ?? current.visibility.hiddenProviders)],
      hiddenAccountIds: [...(update.hiddenAccountIds ?? current.visibility.hiddenAccountIds)],
      trayHiddenProviders: [
        ...(update.trayHiddenProviders ?? current.visibility.trayHiddenProviders),
      ],
      trayHiddenAccountIds: [
        ...(update.trayHiddenAccountIds ?? current.visibility.trayHiddenAccountIds),
      ],
    };
    await writePrivateJsonFile(file, { version: 1, ...saved });
    return { state: 'saved', visibility: saved };
  });
}

/** What the dashboard shows: the hidden lists, and whether they come from a good read. */
export interface DashboardVisibility extends AccountVisibility {
  available: boolean;
}

/**
 * The dashboard's memory of the visibility file, per CCS scope. An unreadable
 * file is never read as empty (CONTRACT-registry-lifecycle section 1, rule 6):
 * the last good lists stay in force, marked unavailable, so one bad read
 * (a wrong mode, a partial hand edit) never shows every hidden row again.
 * Only a scope that has never had a good read shows nothing hidden.
 */
export class VisibilityMemory {
  private readonly scopes = new Map<
    string,
    { good: AccountVisibility | null; available: boolean }
  >();

  constructor(private readonly maxScopes = 16) {}

  private view(good: AccountVisibility | null, available: boolean): DashboardVisibility {
    return {
      hiddenProviders: [...(good?.hiddenProviders ?? [])],
      hiddenAccountIds: [...(good?.hiddenAccountIds ?? [])],
      trayHiddenProviders: [...(good?.trayHiddenProviders ?? [])],
      trayHiddenAccountIds: [...(good?.trayHiddenAccountIds ?? [])],
      available,
    };
  }

  /** Record a finished read of this scope and return what to show. */
  record(scope: string, read: AccountVisibilityRead): DashboardVisibility {
    const previous = this.scopes.get(scope);
    const entry =
      read.state === 'ok'
        ? {
            good: {
              hiddenProviders: [...read.visibility.hiddenProviders],
              hiddenAccountIds: [...read.visibility.hiddenAccountIds],
              trayHiddenProviders: [...read.visibility.trayHiddenProviders],
              trayHiddenAccountIds: [...read.visibility.trayHiddenAccountIds],
            },
            available: true,
          }
        : { good: previous?.good ?? null, available: false };
    this.scopes.delete(scope);
    this.scopes.set(scope, entry);
    while (this.scopes.size > this.maxScopes) {
      const oldest = this.scopes.keys().next().value;
      if (oldest === undefined) break;
      this.scopes.delete(oldest);
    }
    return this.view(entry.good, entry.available);
  }

  /** While a read is slow: the last result for this scope, or nothing hidden and unavailable. */
  current(scope: string): DashboardVisibility {
    const entry = this.scopes.get(scope);
    return this.view(entry?.good ?? null, entry?.available ?? false);
  }
}
