import type { DashboardAccount, DashboardPlatform } from './account-dashboard-types';
import { parseSourceManifest, readSourceManifestFile } from './account-usage-manifest';
import {
  readAccountRegistry,
  registryFromSourceManifest,
  type RegistryAccount,
} from './account-registry-v2';
import { isPublicAccountId } from './account-visibility';
import {
  ADDITIONAL_PROVIDERS,
  isAdditionalAccountId,
  type AdditionalProvider,
} from './additional-usage-transport';
import type { ClaudeProfileRecord } from './claude-account-lifecycle';
import type { StoredKeyInfo } from './account-key-store';

/**
 * Account lookup and the public account view of the Accounts & Settings page
 * (CONTRACT-registry-lifecycle 6.1). Ids keep today's public shapes; anything
 * else is 400 `invalid_account`, and an id no store lists is 404.
 */
export class LifecycleHttpError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    readonly extra: Record<string, unknown> = {}
  ) {
    super(code);
    this.name = 'LifecycleHttpError';
  }
}

/** Fixed public sentences; the code is what clients key their own copy on. */
export const LIFECYCLE_MESSAGES: Readonly<Record<string, string>> = Object.freeze({
  invalid_account: 'That account id is not valid.',
  unknown_account: 'That account was not found.',
  not_implemented: 'This action is not available on this server yet.',
  not_configured: 'That app has no launcher on that computer.',
  not_removable: 'This account cannot be removed from the dashboard.',
  single_account_provider: 'This provider supports one account in this version.',
  too_many_accounts: 'This provider already has the most accounts allowed.',
  id_in_use: 'That name is already used by another account.',
  duplicate_key: 'That key is already saved for another account.',
  key_rejected: 'The provider rejected that key. Nothing was saved.',
  not_aac_owned: 'This account reads a key saved by another app. Add a key here instead.',
  use_replace_key: 'Use Replace key for this account.',
  account_active: 'Activate another account first, then try again.',
  account_default: 'This is the default account. Make another account the default first.',
  account_protected: "This is this computer's default Claude profile. It can't be removed here.",
  last_account: 'This is the last account of this provider.',
  activation_running: 'An account switch is running. Wait for it to finish.',
  signin_running: 'A sign-in for this account is running.',
  app_running: 'Quit the app for this profile on that computer first.',
  app_state_unknown: 'Could not check whether the app is running. Try again later.',
  confirmation_stale: 'The account changed. Review it again.',
  job_running: 'A sign-in for this provider is already running.',
  too_many_jobs: 'Too many sign-ins are running. Finish one first.',
  host_unreachable: 'A computer could not be reached. Nothing was changed.',
  trash_cross_volume: 'The trash is on another disk on that computer. Nothing was moved.',
  remove_failed: 'The account could not be removed safely. Nothing was changed.',
  restore_failed: 'The account could not be restored safely.',
  purge_failed: 'The profile could not be deleted safely. It stays in the trash.',
  write_failed: 'The change could not be saved safely.',
  registry_unavailable: 'Account list could not be read safely.',
  key_store_unavailable: 'The key could not be stored safely.',
  rate_limited: 'This account was checked a moment ago. Try again shortly.',
  unknown_job: 'That sign-in has ended and is no longer listed.',
  code_not_expected: 'This sign-in is not waiting for a code.',
  unknown_trash: 'That trash entry was not found.',
  tool_missing: 'The sign-in tool is not installed on the dashboard computer.',
  preflight_failed: 'Sign in to this provider from a terminal on Ubuntu with the command shown.',
  isolation_unproven: 'A second account for this provider is not supported yet.',
  extension_update_required: 'This needs an update of the browser extension.',
  secure_transport_required:
    'Use the secure dashboard address (HTTPS or a tunnel) for keys and sign-in codes.',
});

export type ResolvedAccount =
  | { kind: 'codex'; id: string; provider: 'codex'; name: string }
  | { kind: 'claude'; id: string; provider: 'claude'; profile: ClaudeProfileRecord }
  | {
      kind: 'additional';
      id: string;
      provider: AdditionalProvider;
      entry: RegistryAccount;
      mode: 'v1' | 'v2';
    }
  | { kind: 'antigravity'; id: string; provider: 'antigravity'; profileId: string }
  | { kind: 'wallet'; id: string; provider: 'opencode-go' };

export interface AdditionalEntries {
  mode: 'v1' | 'v2';
  entries: RegistryAccount[];
}

/** The additional-provider accounts the collectors read now: registry v2, else the v1 sources. */
export async function readAdditionalEntries(ccsDir: string): Promise<AdditionalEntries> {
  const read = await readAccountRegistry(ccsDir).catch(() => ({ state: 'invalid' as const }));
  if (read.state === 'invalid') throw new LifecycleHttpError(500, 'registry_unavailable');
  if (read.state === 'ok') return { mode: 'v2', entries: read.registry.accounts };
  const manifest = parseSourceManifest(await readSourceManifestFile(ccsDir).catch(() => null));
  if (!manifest.valid) throw new LifecycleHttpError(500, 'registry_unavailable');
  return {
    mode: 'v1',
    entries: registryFromSourceManifest(manifest.sources, new Date(0).toISOString()).accounts,
  };
}

export interface AccountLookup {
  ccsDir: string;
  codexHasProfile: (name: string) => boolean;
  findClaude: (id: string) => Promise<ClaudeProfileRecord | null>;
  /** When given, an Antigravity id must name a saved profile (else 404). */
  antigravityHasProfile?: (profileId: string) => boolean;
}

export async function resolveAccount(id: string, lookup: AccountLookup): Promise<ResolvedAccount> {
  if (!isPublicAccountId(id)) throw new LifecycleHttpError(400, 'invalid_account');
  if (id.startsWith('plan-opencode-go-console-')) {
    return { kind: 'wallet', id, provider: 'opencode-go' };
  }
  const [provider, ...rest] = id.split(':');
  const local = rest.join(':');
  if (provider === 'codex' && rest.length === 1 && lookup.codexHasProfile(local)) {
    return { kind: 'codex', id, provider, name: local };
  }
  if (provider === 'claude' && rest.length === 1) {
    const profile = await lookup.findClaude(local);
    if (profile) return { kind: 'claude', id, provider, profile };
  }
  if (provider === 'antigravity' && rest[0] === 'profile' && rest.length === 2) {
    if (!lookup.antigravityHasProfile || lookup.antigravityHasProfile(rest[1])) {
      return { kind: 'antigravity', id, provider, profileId: rest[1] };
    }
    throw new LifecycleHttpError(404, 'unknown_account');
  }
  if ((ADDITIONAL_PROVIDERS as readonly string[]).includes(provider)) {
    const additional = provider as AdditionalProvider;
    if (isAdditionalAccountId(additional, id)) {
      const { mode, entries } = await readAdditionalEntries(lookup.ccsDir);
      const entry = entries.find((candidate) => candidate.id === id);
      if (entry) return { kind: 'additional', id, provider: additional, entry, mode };
    }
  }
  throw new LifecycleHttpError(404, 'unknown_account');
}

export type CredentialView =
  | {
      kind: 'aac-key';
      last4: string | null;
      fingerprint: string | null;
      storedOn: DashboardPlatform;
    }
  | { kind: 'discover' | 'browser-capsule' | 'config-home' | 'antigravity-profile' };

export interface AccountActions {
  signInAgain: boolean;
  replaceKey: boolean;
  remove: boolean;
  open: Array<'mac' | 'windows'>;
  recheck: boolean;
}

export interface RegistryAccountView {
  id: string;
  provider: DashboardAccount['provider'];
  label: string;
  email: string | null;
  platform: DashboardPlatform;
  credential: CredentialView | null;
  /** Hidden on the dashboard (the provider or this account). */
  hidden: boolean;
  /** Hidden in the trays (the provider or this account); independent of `hidden`. */
  trayHidden: boolean;
  lifecycle: NonNullable<DashboardAccount['lifecycle']>;
  actions: AccountActions;
  removeRefusal: string | null;
}

export function credentialView(entry: RegistryAccount, key: StoredKeyInfo | null): CredentialView {
  if (entry.credential.kind === 'aac-key') {
    return {
      kind: 'aac-key',
      last4: key?.last4 ?? null,
      fingerprint: key?.fingerprint ?? null,
      storedOn: key?.storedOn ?? entry.platform,
    };
  }
  return { kind: entry.credential.kind };
}

export function accountView(
  row: Pick<DashboardAccount, 'id' | 'provider' | 'label' | 'email' | 'platform'> & {
    hidden?: boolean;
    trayHidden?: boolean;
    lifecycle?: DashboardAccount['lifecycle'];
  },
  credential: CredentialView | null,
  actions: AccountActions,
  removeRefusal: string | null
): RegistryAccountView {
  return {
    id: row.id,
    provider: row.provider,
    label: row.label,
    email: row.email,
    platform: row.platform,
    credential,
    hidden: row.hidden === true,
    trayHidden: row.trayHidden === true,
    lifecycle: row.lifecycle ?? { state: 'ready', jobId: null },
    actions,
    removeRefusal,
  };
}
