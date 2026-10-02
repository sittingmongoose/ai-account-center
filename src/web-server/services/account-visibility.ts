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
 * `~/.ccs/account-visibility.json`, 0600, at most 16 KB. Hidden accounts keep
 * being collected and stay auto-switch candidates.
 */
export const ACCOUNT_VISIBILITY_FILE = 'account-visibility.json';
export const MAX_VISIBILITY_BYTES = 16 * 1024;
export const MAX_HIDDEN_ACCOUNT_IDS = 128;
/** Bound on the raw provider list before duplicates are dropped. */
const MAX_HIDDEN_PROVIDER_ENTRIES = 32;

/** Public account id shapes (section 1, rule 8), plus the OpenCode console wallet. */
const ACCOUNT_ID = /^[a-z0-9][a-z0-9-]{0,31}(?::[A-Za-z0-9@._+-]{1,128}){1,2}$/;
const WALLET_ACCOUNT_ID = /^plan-opencode-go-console-mac-[a-f0-9]{12}$/;

export interface AccountVisibility {
  hiddenProviders: DashboardProvider[];
  hiddenAccountIds: string[];
}

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

/**
 * The PUT body: exactly `{hiddenProviders, hiddenAccountIds}`. Unknown keys,
 * unknown providers, malformed ids and more than 128 ids are rejected.
 * Duplicates are dropped; absent account ids are kept.
 */
export function parseVisibilityBody(value: unknown): AccountVisibility | null {
  if (
    !record(value) ||
    Object.keys(value).length !== 2 ||
    !Array.isArray(value.hiddenProviders) ||
    !Array.isArray(value.hiddenAccountIds) ||
    value.hiddenProviders.length > MAX_HIDDEN_PROVIDER_ENTRIES ||
    value.hiddenAccountIds.length > MAX_HIDDEN_ACCOUNT_IDS ||
    !value.hiddenProviders.every(isDashboardProviderId) ||
    !value.hiddenAccountIds.every(isPublicAccountId)
  ) {
    return null;
  }
  return {
    hiddenProviders: unique(value.hiddenProviders as DashboardProvider[]),
    hiddenAccountIds: unique(value.hiddenAccountIds as string[]),
  };
}

/**
 * The stored file. A provider that has left the table since it was hidden is
 * dropped quietly; anything else malformed makes the file unavailable.
 */
function parseVisibilityFile(value: unknown): AccountVisibility | null {
  if (
    !record(value) ||
    value.version !== 1 ||
    Object.keys(value).length !== 3 ||
    !Array.isArray(value.hiddenProviders) ||
    !value.hiddenProviders.every((id) => typeof id === 'string')
  ) {
    return null;
  }
  return parseVisibilityBody({
    hiddenProviders: (value.hiddenProviders as string[]).filter(isDashboardProviderId),
    hiddenAccountIds: value.hiddenAccountIds,
  });
}

export function accountVisibilityPath(ccsDir: string): string {
  return path.join(ccsDir, ACCOUNT_VISIBILITY_FILE);
}

/** Absent means nothing is hidden; an unsafe or malformed file is unavailable, never empty. */
export async function readAccountVisibility(ccsDir: string): Promise<AccountVisibilityRead> {
  const read = await readPrivateJsonFile(accountVisibilityPath(ccsDir), MAX_VISIBILITY_BYTES);
  if (read.state === 'absent') {
    return { state: 'ok', visibility: { hiddenProviders: [], hiddenAccountIds: [] } };
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
    };
    await writePrivateJsonFile(file, { version: 1, ...saved });
    return saved;
  });
}
