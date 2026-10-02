import path from 'path';
import { ConfigError, ValidationError } from '../../errors/error-types';
import type { DashboardPlatform } from './account-dashboard-types';
import { parseSourceManifest, readSourceManifestFile } from './account-usage-manifest';
import {
  ADDITIONAL_PROVIDERS,
  isAdditionalAccountId,
  isAdditionalProvider,
  isCollectorCredential,
  isSafeUsageSshAlias,
  type AdditionalProvider,
  type AdditionalUsageSource,
  type CollectorCredential,
} from './additional-usage-transport';
import {
  readPrivateJsonFile,
  withPrivateFileLock,
  writePrivateJsonFile,
} from './private-json-store';

/**
 * Account registry v2 for the seven additional providers
 * (CONTRACT-registry-lifecycle section 3.1). A new file, so an older package
 * keeps reading the untouched version 1 `account-usage-sources.json`.
 */
export const ACCOUNT_REGISTRY_FILE = 'account-usage-accounts.json';
export const MAX_REGISTRY_BYTES = 64 * 1024;
export const MAX_REGISTRY_ACCOUNTS = 64;
export const MAX_ACCOUNTS_PER_PROVIDER = 16;

/** Which credential kinds each provider's accounts may use. */
export const PROVIDER_CREDENTIAL_KINDS: Readonly<
  Record<AdditionalProvider, readonly CollectorCredential['kind'][]>
> = Object.freeze({
  antigravity: ['discover', 'antigravity-profile'],
  muse: ['discover', 'config-home'],
  cursor: ['discover'],
  'kimi-code': ['discover', 'aac-key', 'config-home'],
  qwen: ['discover', 'browser-capsule'],
  zai: ['discover', 'aac-key'],
  'opencode-go': ['discover', 'aac-key'],
});

export interface RegistryAccount {
  id: string;
  provider: AdditionalProvider;
  platform: DashboardPlatform;
  sshHost: string | null;
  label: string | null;
  credential: CollectorCredential;
  createdAt: string | null;
  createdBy: 'migration' | 'dashboard' | null;
}

export interface AccountRegistryV2 {
  version: 2;
  accounts: RegistryAccount[];
}

export type AccountRegistryRead =
  | { state: 'absent' }
  | { state: 'invalid' }
  | { state: 'ok'; registry: AccountRegistryV2 };

const ENTRY_KEYS = new Set([
  'id',
  'provider',
  'platform',
  'sshHost',
  'label',
  'credential',
  'createdAt',
  'createdBy',
]);
const TIMESTAMP = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/;
/** C0, DEL, C1, line separators and bidirectional overrides. */
const UNPRINTABLE = /[\u0000-\u001f\u007f-\u009f\u2028\u2029\u202a-\u202e\u2066-\u2069]/;

function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

/** 1-48 printable characters, not only spaces (Z.ai has no email; the label names it). */
export function isAccountLabel(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    [...value].length >= 1 &&
    [...value].length <= 48 &&
    value.trim().length > 0 &&
    !UNPRINTABLE.test(value)
  );
}

function credentialReference(credential: CollectorCredential): string | null {
  switch (credential.kind) {
    case 'aac-key':
      return `key:${credential.keyId}`;
    case 'browser-capsule':
      return `capsule:${credential.capsuleId}`;
    case 'config-home':
      return `home:${credential.homeId}`;
    case 'antigravity-profile':
      return `profile:${credential.profileId}`;
    default:
      return null;
  }
}

function parseEntry(entry: unknown): RegistryAccount | null {
  if (!record(entry) || Object.keys(entry).some((key) => !ENTRY_KEYS.has(key))) return null;
  const { id, provider, platform, sshHost, label, credential, createdAt, createdBy } = entry;
  if (
    !isAdditionalProvider(provider) ||
    !isAdditionalAccountId(provider, id) ||
    (platform !== 'ubuntu' && platform !== 'mac' && platform !== 'windows') ||
    (sshHost !== undefined && sshHost !== null && !isSafeUsageSshAlias(sshHost)) ||
    (label !== undefined && label !== null && !isAccountLabel(label)) ||
    !isCollectorCredential(credential) ||
    !PROVIDER_CREDENTIAL_KINDS[provider].includes(credential.kind) ||
    // `discover` is today's first-working-credential lookup: only the migrated row.
    (credential.kind === 'discover' && id !== `${provider}:usage`) ||
    (createdAt !== undefined &&
      createdAt !== null &&
      (typeof createdAt !== 'string' ||
        !TIMESTAMP.test(createdAt) ||
        !Number.isFinite(Date.parse(createdAt)))) ||
    (createdBy !== undefined &&
      createdBy !== null &&
      createdBy !== 'migration' &&
      createdBy !== 'dashboard')
  ) {
    return null;
  }
  return {
    id: id as string,
    provider,
    platform,
    sshHost: (sshHost as string | null | undefined) ?? null,
    label: (label as string | null | undefined) ?? null,
    credential: { ...credential },
    createdAt: (createdAt as string | null | undefined) ?? null,
    createdBy: (createdBy as RegistryAccount['createdBy'] | undefined) ?? null,
  };
}

/** Strict: any unknown key, wrong type, duplicate or limit breach rejects the whole file. */
export function parseAccountRegistry(value: unknown): AccountRegistryV2 | null {
  if (
    !record(value) ||
    Object.keys(value).some((key) => key !== 'version' && key !== 'accounts') ||
    value.version !== 2 ||
    !Array.isArray(value.accounts) ||
    value.accounts.length > MAX_REGISTRY_ACCOUNTS
  ) {
    return null;
  }
  const accounts: RegistryAccount[] = [];
  const ids = new Set<string>();
  const references = new Set<string>();
  const perProvider = new Map<AdditionalProvider, number>();
  for (const candidate of value.accounts) {
    const entry = parseEntry(candidate);
    if (!entry || ids.has(entry.id)) return null;
    const reference = credentialReference(entry.credential);
    if (reference && references.has(`${entry.provider}/${reference}`)) return null;
    const count = (perProvider.get(entry.provider) ?? 0) + 1;
    if (count > MAX_ACCOUNTS_PER_PROVIDER) return null;
    perProvider.set(entry.provider, count);
    ids.add(entry.id);
    if (reference) references.add(`${entry.provider}/${reference}`);
    accounts.push(entry);
  }
  return { version: 2, accounts };
}

export function accountRegistryPath(ccsDir: string): string {
  return path.join(ccsDir, ACCOUNT_REGISTRY_FILE);
}

/** Absent, invalid (never read as empty) or the validated registry. */
export async function readAccountRegistry(ccsDir: string): Promise<AccountRegistryRead> {
  const read = await readPrivateJsonFile(accountRegistryPath(ccsDir), MAX_REGISTRY_BYTES);
  if (read.state !== 'ok') return read;
  const registry = parseAccountRegistry(read.value);
  return registry ? { state: 'ok', registry } : { state: 'invalid' };
}

/** The collector source of one registry account. */
export function registrySource(entry: RegistryAccount): AdditionalUsageSource {
  return {
    provider: entry.provider,
    platform: entry.platform,
    ...(entry.sshHost === null ? {} : { sshHost: entry.sshHost }),
    account: { id: entry.id, label: entry.label, credential: { ...entry.credential } },
  };
}

/**
 * The registry an upgrade starts from: one `discover` entry per provider with
 * the effective version 1 platform and ssh alias, ids `<provider>:usage`.
 */
export function registryFromSourceManifest(
  sources: AdditionalUsageSource[],
  createdAt: string
): AccountRegistryV2 {
  return {
    version: 2,
    accounts: ADDITIONAL_PROVIDERS.map((provider) => {
      const source = sources.find((candidate) => candidate.provider === provider) ?? {
        provider,
        platform: 'ubuntu' as const,
      };
      return {
        id: `${provider}:usage`,
        provider,
        platform: source.platform,
        sshHost: source.sshHost ?? null,
        label: null,
        credential: { kind: 'discover' as const },
        createdAt,
        createdBy: 'migration' as const,
      };
    }),
  };
}

export interface AccountRegistryUpdateDeps {
  now?: () => number;
  readSourceManifest?: () => Promise<string | null>;
}

/**
 * Change the registry under its file mutex (for lifecycle writes). The first
 * write migrates the effective version 1 sources (CONTRACT-registry-lifecycle
 * section 8.1); the version 1 file is only read, never modified. An invalid
 * registry or an invalid version 1 manifest refuses the write, so a bad file
 * is never silently replaced with defaults.
 */
export function updateAccountRegistry(
  ccsDir: string,
  mutate: (registry: AccountRegistryV2) => AccountRegistryV2,
  deps: AccountRegistryUpdateDeps = {}
): Promise<AccountRegistryV2> {
  const file = accountRegistryPath(ccsDir);
  return withPrivateFileLock(file, async () => {
    const current = await readAccountRegistry(ccsDir);
    if (current.state === 'invalid') {
      throw new ConfigError('Account list could not be read safely.');
    }
    let base: AccountRegistryV2;
    if (current.state === 'ok') {
      base = current.registry;
    } else {
      const contents = await (deps.readSourceManifest ?? (() => readSourceManifestFile(ccsDir)))();
      const manifest = parseSourceManifest(contents);
      if (!manifest.valid) throw new ConfigError('Account list could not be read safely.');
      base = registryFromSourceManifest(
        manifest.sources,
        new Date((deps.now ?? Date.now)()).toISOString()
      );
    }
    const next = parseAccountRegistry(
      mutate({ version: 2, accounts: base.accounts.map((entry) => ({ ...entry })) })
    );
    if (!next) throw new ValidationError('The account list change is not valid.');
    await writePrivateJsonFile(file, next);
    return next;
  });
}
