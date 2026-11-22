import path from 'path';
import { withTsPrivateFileLeases } from '../../shared/private-files';
import { ConfigError } from '../../errors/error-types';
import type { DashboardProvider } from './account-dashboard-types';
import { discoveredOpencodeGoParentId } from './additional-usage-transport';
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
 * changes the other. Hidden accounts keep being collected.
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
export type AccountVisibilityDelta = Partial<
  Record<keyof AccountVisibility, { hide: string[]; show: string[] }>
>;

export function parseVisibilityDelta(value: unknown): AccountVisibilityDelta | null {
  if (!record(value) || Object.keys(value).length === 0) return null;
  const delta: AccountVisibilityDelta = {};
  for (const [key, operation] of Object.entries(value)) {
    if (!(VISIBILITY_KEYS as readonly string[]).includes(key) || !record(operation)) return null;
    if (
      Object.keys(operation).length === 0 ||
      !Object.keys(operation).every((name) => name === 'hide' || name === 'show')
    )
      return null;
    const parse = key.endsWith('Providers') ? parseProviderList : parseIdList;
    const hide = parse('hide' in operation ? operation.hide : []);
    const show = parse('show' in operation ? operation.show : []);
    if (!hide || !show || hide.some((id) => show.includes(id))) return null;
    delta[key as keyof AccountVisibility] = { hide, show };
  }
  return delta;
}

type VisibilityForward = (
  legacy: () => Promise<AccountVisibilityUpdateResult>
) => Promise<AccountVisibilityUpdateResult>;

function withVisibilityWriter<T>(ccsDir: string, task: () => Promise<T>): Promise<T> {
  return withPrivateFileLock(accountVisibilityPath(ccsDir), () =>
    withTsPrivateFileLeases(ccsDir, ['account-visibility'], task)
  );
}

function forwardVisibility(
  ccsDir: string,
  legacy: () => Promise<AccountVisibilityUpdateResult>,
  forward?: VisibilityForward
): Promise<AccountVisibilityUpdateResult> {
  return forward
    ? withPrivateFileLock(accountVisibilityPath(ccsDir), () => forward(legacy))
    : withVisibilityWriter(ccsDir, legacy);
}

export async function patchAccountVisibility(
  ccsDir: string,
  delta: AccountVisibilityDelta,
  forward?: VisibilityForward
): Promise<AccountVisibilityUpdateResult> {
  const file = accountVisibilityPath(ccsDir);
  return forwardVisibility(
    ccsDir,
    async () => {
      const current = await readAccountVisibility(ccsDir);
      if (current.state !== 'ok') return { state: 'unavailable' };
      const next = { ...current.visibility };
      for (const key of VISIBILITY_KEYS) {
        const operation = delta[key];
        if (operation) {
          const values = new Set<string>(next[key]);
          const isIdList = key === 'hiddenAccountIds' || key === 'trayHiddenAccountIds';
          const hideSet = new Set(operation.hide);
          operation.show.forEach((id) => {
            values.delete(id);
            if (isIdList) {
              const parent = discoveredOpencodeGoParentId(id);
              if (parent && !hideSet.has(parent)) values.delete(parent);
            }
          });
          operation.hide.forEach((id) => values.add(id));
          Object.assign(next, { [key]: [...values] });
        }
      }
      const saved = parseVisibilityBody(next);
      if (!saved) throw new ConfigError('Visibility limits exceeded');
      await writePrivateJsonFile(file, { version: 1, ...next });
      return { state: 'saved', visibility: next };
    },
    forward
  );
}

/**
 * Replace matching entries in an account-id list in place, preserving order and dropping
 * duplicates. Returns `changed: false` when no entry matched `replacements`.
 */
export function replaceVisibilityAccountIds(
  list: readonly string[],
  replacements: ReadonlyMap<string, string>
): { list: string[]; changed: boolean } {
  if (list.length === 0 || replacements.size === 0) return { list: [...list], changed: false };
  let changed = false;
  const seen = new Set<string>();
  const next: string[] = [];
  for (const id of list) {
    const replacement = replacements.get(id);
    if (replacement !== undefined) changed = true;
    const mapped = replacement ?? id;
    if (seen.has(mapped)) continue;
    seen.add(mapped);
    next.push(mapped);
  }
  return { list: next, changed };
}

/**
 * Persist unambiguous bare-to-digest account ID migrations in place under the visibility file
 * mutex and registry lease. Updates `hiddenAccountIds` and `trayHiddenAccountIds` independently,
 * only for bare IDs still present when the lock is held. Writes nothing when no bare ID remains
 * or when the file is unavailable.
 */
export async function migrateAccountVisibilityIds(
  ccsDir: string,
  replacements: ReadonlyMap<string, string>,
  forward?: VisibilityForward
): Promise<AccountVisibilityUpdateResult> {
  if (replacements.size === 0) {
    const current = await readAccountVisibility(ccsDir);
    return current.state === 'ok'
      ? { state: 'saved', visibility: current.visibility }
      : { state: 'unavailable' };
  }
  return forwardVisibility(
    ccsDir,
    async () => {
      const current = await readAccountVisibility(ccsDir);
      if (current.state !== 'ok') return { state: 'unavailable' };
      const home = replaceVisibilityAccountIds(current.visibility.hiddenAccountIds, replacements);
      const tray = replaceVisibilityAccountIds(
        current.visibility.trayHiddenAccountIds,
        replacements
      );
      if (!home.changed && !tray.changed) {
        return { state: 'saved', visibility: current.visibility };
      }
      const next: AccountVisibility = {
        hiddenProviders: current.visibility.hiddenProviders,
        hiddenAccountIds: home.changed ? home.list : current.visibility.hiddenAccountIds,
        trayHiddenProviders: current.visibility.trayHiddenProviders,
        trayHiddenAccountIds: tray.changed ? tray.list : current.visibility.trayHiddenAccountIds,
      };
      if (!parseVisibilityBody(next)) throw new ConfigError('Visibility limits exceeded');
      const visibility = await saveVisibility(ccsDir, next);
      return { state: 'saved', visibility };
    },
    forward
  );
}

export type AccountVisibilityRead =
  { state: 'ok'; visibility: AccountVisibility } | { state: 'unavailable' };

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
  return withVisibilityWriter(ccsDir, () => saveVisibility(ccsDir, visibility));
}

async function saveVisibility(
  ccsDir: string,
  visibility: AccountVisibility
): Promise<AccountVisibility> {
  const saved = {
    hiddenProviders: [...visibility.hiddenProviders],
    hiddenAccountIds: [...visibility.hiddenAccountIds],
    trayHiddenProviders: [...visibility.trayHiddenProviders],
    trayHiddenAccountIds: [...visibility.trayHiddenAccountIds],
  };
  await writePrivateJsonFile(accountVisibilityPath(ccsDir), { version: 1, ...saved });
  return saved;
}

export type AccountVisibilityUpdateResult =
  { state: 'saved'; visibility: AccountVisibility } | { state: 'unavailable' };

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
  update: AccountVisibilityUpdate,
  forward?: VisibilityForward
): Promise<AccountVisibilityUpdateResult> {
  return forwardVisibility(
    ccsDir,
    async () => {
      const { hiddenProviders, hiddenAccountIds, trayHiddenProviders, trayHiddenAccountIds } =
        update;
      if (hiddenProviders && hiddenAccountIds && trayHiddenProviders && trayHiddenAccountIds) {
        const visibility = await saveVisibility(ccsDir, {
          hiddenProviders,
          hiddenAccountIds,
          trayHiddenProviders,
          trayHiddenAccountIds,
        });
        return { state: 'saved', visibility };
      }

      const current = await readAccountVisibility(ccsDir);
      if (current.state !== 'ok') return { state: 'unavailable' };
      const visibility = await saveVisibility(ccsDir, {
        hiddenProviders: hiddenProviders ?? current.visibility.hiddenProviders,
        hiddenAccountIds: hiddenAccountIds ?? current.visibility.hiddenAccountIds,
        trayHiddenProviders: trayHiddenProviders ?? current.visibility.trayHiddenProviders,
        trayHiddenAccountIds: trayHiddenAccountIds ?? current.visibility.trayHiddenAccountIds,
      });
      return { state: 'saved', visibility };
    },
    forward
  );
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
