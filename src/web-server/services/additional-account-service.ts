import path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import type { DashboardAccount, DashboardAccountWindow } from './account-dashboard-types';
import { parseSourceManifest, readSourceManifestFile } from './account-usage-manifest';
import {
  readAccountRegistry,
  registrySource,
  type AccountRegistryRead,
} from './account-registry-v2';
import {
  AdditionalUsageTransportError,
  isCollectableSource,
  runAdditionalUsageSource,
  type AdditionalProvider,
  type AdditionalUsageSource,
} from './additional-usage-transport';

const CACHE_TTL_MS = 120_000;
const REFRESH_DEBOUNCE_MS = 5_000;
const FAILURE_BACKOFF_MS = 30_000;
const MAX_RESULT_BYTES = 64 * 1024;
const MAX_USAGE_WINDOWS = 256;
const MAX_SCOPES = 16;
const LABELS: Record<AdditionalProvider, string> = {
  antigravity: 'Antigravity',
  muse: 'Muse Code',
  cursor: 'Cursor',
  'kimi-code': 'Kimi Code',
  qwen: 'Qwen token plan',
  zai: 'Z.ai coding plan',
  'opencode-go': 'OpenCode Go',
};
const PLATFORM_LABELS = { ubuntu: 'Ubuntu', mac: 'Mac', windows: 'Windows' };
const MUSE_CACHED_MESSAGES = new Set([
  'Muse is limiting requests; showing the last successful usage reading. Refresh resumes automatically.',
  'Showing the last successful Muse usage reading. Usage refreshes automatically.',
]);
const MUSE_TRANSIENT_FAILURES = new Set(['rate_limited', 'provider_error', 'network_error']);

export interface AdditionalAccountDeps {
  ccsDir?: string;
  /** Version 1 manifest contents (`account-usage-sources.json`), read when registry v2 is absent. */
  readManifest?: () => Promise<string | null>;
  /** Registry v2 (`account-usage-accounts.json`); absent, invalid or valid. */
  readRegistry?: () => Promise<AccountRegistryRead>;
  runSource?: (source: AdditionalUsageSource) => Promise<string>;
  now?: () => number;
}

/** Which store listed the accounts: v1 manifest, valid v2 registry, or an unreadable v2 file. */
export type AdditionalRegistryMode = 'v1' | 'v2' | 'v2-invalid';

export interface AdditionalAccountsSnapshot {
  registry: AdditionalRegistryMode;
  accounts: DashboardAccount[];
}

interface Manifest {
  mode: AdditionalRegistryMode;
  fingerprint: string;
  valid: boolean;
  sources: AdditionalUsageSource[];
}

interface SourceCache {
  result: DashboardAccount | null;
  lastGood: DashboardAccount | null;
  fetchedAt: number;
  refreshedAt: number;
  failed: boolean;
  pending: Promise<DashboardAccount> | null;
}

function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

function nonnegative(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

function percent(value: unknown): number | null {
  const parsed = nonnegative(value);
  return parsed !== null && parsed <= 100 ? parsed : null;
}

function timestamp(value: unknown): string | null {
  if (
    typeof value !== 'string' ||
    value.length > 40 ||
    !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)
  ) {
    return null;
  }
  const parsed = new Date(value);
  const year = parsed.getUTCFullYear();
  return Number.isFinite(parsed.getTime()) && year >= 2000 && year <= 2200
    ? parsed.toISOString()
    : null;
}

function displayText(value: unknown, maximum: number): string | null {
  if (
    typeof value !== 'string' ||
    value.length > maximum ||
    /[\u0000-\u001f\u007f]/.test(value) ||
    /(?:bearer\s|sk[-_]|eyJ[A-Za-z0-9_-]{8}|https?:\/\/|[{}]|secret|access_token|refresh_token)/i.test(
      value
    )
  ) {
    return null;
  }
  return value.trim() || null;
}

function email(value: unknown): string | null {
  const parsed = displayText(value, 254);
  return parsed && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(parsed) ? parsed : null;
}

function usageWindow(value: unknown, index: number): DashboardAccountWindow | null {
  if (!record(value)) return null;
  const usedPercent = nonnegative(value.usedPercent);
  const remainingPercent = percent(value.remainingPercent);
  const used = nonnegative(value.used);
  const limit = nonnegative(value.limit);
  const remaining =
    typeof value.remaining === 'number' && Number.isFinite(value.remaining)
      ? value.remaining
      : null;
  const resetAt = timestamp(value.resetAt);
  const expiresAt = timestamp(value.expiresAt);
  const kind =
    value.kind === 'rate_limit' ||
    value.kind === 'balance' ||
    value.kind === 'spend' ||
    value.kind === 'extra_usage'
      ? value.kind
      : undefined;
  if (
    usedPercent === null &&
    remainingPercent === null &&
    used === null &&
    limit === null &&
    remaining === null &&
    resetAt === null &&
    expiresAt === null &&
    typeof value.unlimited !== 'boolean' &&
    typeof value.enabled !== 'boolean'
  ) {
    return null;
  }
  const candidateKey = displayText(value.key, 64);
  const key =
    candidateKey && /^[A-Za-z0-9_-]{1,64}$/.test(candidateKey)
      ? candidateKey
      : `usage-${index + 1}`;
  return {
    key,
    label: displayText(value.label, 80) ?? 'Usage',
    usedPercent,
    remainingPercent:
      remainingPercent ?? (usedPercent === null ? null : Math.max(0, 100 - usedPercent)),
    resetAt,
    windowMinutes: nonnegative(value.windowMinutes),
    used,
    limit,
    unit: displayText(value.unit, 24),
    ...(kind ? { kind } : {}),
    ...(Object.prototype.hasOwnProperty.call(value, 'remaining') ? { remaining } : {}),
    ...(Object.prototype.hasOwnProperty.call(value, 'expiresAt') ? { expiresAt } : {}),
    ...(typeof value.unlimited === 'boolean' ? { unlimited: value.unlimited } : {}),
    ...(typeof value.enabled === 'boolean' ? { enabled: value.enabled } : {}),
  };
}

function unavailable(
  source: AdditionalUsageSource,
  status: DashboardAccount['status'] = 'unavailable',
  message = 'Saved account usage is unavailable on this computer.'
): DashboardAccount {
  return {
    id: source.account?.id ?? `${source.provider}:usage`,
    provider: source.provider,
    providerLabel: LABELS[source.provider],
    label: source.account?.label ?? LABELS[source.provider],
    email: null,
    plan: null,
    platform: source.platform,
    source: `Account on ${PLATFORM_LABELS[source.platform]}`,
    status,
    message,
    fetchedAt: null,
    sampledAt: null,
    isActive: false,
    windows: [],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}

/** Rebuild the public DTO; helper extras, command output and upstream errors are discarded. */
function normalize(
  source: AdditionalUsageSource,
  contents: string
): { account: DashboardAccount; invalidIdentity: boolean; museTransientFailure: boolean } {
  if (Buffer.byteLength(contents, 'utf8') > MAX_RESULT_BYTES) {
    throw new AdditionalUsageTransportError();
  }
  const result: unknown = JSON.parse(contents);
  if (!record(result)) throw new AdditionalUsageTransportError();
  const status: DashboardAccount['status'] =
    result.status === 'ok' ||
    result.status === 'cached' ||
    result.status === 'needs_sign_in' ||
    result.status === 'error' ||
    result.status === 'unavailable'
      ? result.status
      : 'error';
  const account = unavailable(
    source,
    status,
    status === 'needs_sign_in'
      ? 'The saved account needs to sign in again.'
      : status === 'error'
        ? 'Account usage is temporarily unavailable.'
        : 'Saved account usage is unavailable on this computer.'
  );
  account.email = email(result.email);
  let invalidIdentity =
    result.email !== null && result.email !== undefined && account.email === null;
  account.label = source.account?.label ?? account.email ?? LABELS[source.provider];
  const plan = displayText(result.plan, 48);
  account.plan = plan && /^[A-Za-z][A-Za-z0-9 ._+-]*$/.test(plan) ? plan : null;
  if (source.provider === 'muse' && (!account.email || !account.plan)) invalidIdentity = true;
  account.fetchedAt = timestamp(result.fetchedAt);
  account.sampledAt = timestamp(result.sampledAt);
  if (status === 'ok' || status === 'cached') {
    account.windows = Array.isArray(result.windows)
      ? result.windows
          .slice(0, MAX_USAGE_WINDOWS)
          .map(usageWindow)
          .filter((window): window is DashboardAccountWindow => window !== null)
      : [];
    account.message =
      source.provider === 'muse' && status === 'cached'
        ? typeof result.message === 'string' && MUSE_CACHED_MESSAGES.has(result.message)
          ? result.message
          : 'Showing the last saved Muse usage sample.'
        : null;
    if (account.windows.length === 0) {
      account.status = 'error';
      account.message = 'Account usage is temporarily unavailable.';
    }
  }
  if (source.provider === 'muse' && invalidIdentity && (status === 'ok' || status === 'cached')) {
    account.status = 'error';
    account.message = 'The saved Muse account identity could not be verified.';
    account.windows = [];
  }
  return {
    account,
    invalidIdentity,
    museTransientFailure:
      source.provider === 'muse' &&
      typeof result.failureCode === 'string' &&
      MUSE_TRANSIENT_FAILURES.has(result.failureCode),
  };
}

function copy(account: DashboardAccount, cached = false): DashboardAccount {
  return {
    ...account,
    status: cached && account.status === 'ok' ? 'cached' : account.status,
    windows: account.windows.map((window) => ({ ...window })),
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}

function matchesMuseIdentity(current: DashboardAccount, previous: DashboardAccount): boolean {
  return (
    current.email !== null &&
    previous.email !== null &&
    current.email.toLowerCase() === previous.email.toLowerCase() &&
    current.plan !== null &&
    current.plan === previous.plan
  );
}

function observationTime(account: DashboardAccount): number {
  return Date.parse(account.sampledAt ?? account.fetchedAt ?? '');
}

/** One cache per account and source: a changed host, credential or id never inherits a sample. */
function sourceKey(source: AdditionalUsageSource): string {
  return JSON.stringify([
    source.account?.id ?? `${source.provider}:usage`,
    source.provider,
    source.platform,
    source.sshHost ?? null,
    source.account?.credential ?? null,
  ]);
}

const DEFAULT_SOURCES = parseSourceManifest(null).sources;

/**
 * Per-CCS-scope cache, one entry per account (CONTRACT-registry-lifecycle 3.1).
 * Read order: a valid registry v2 is the only source; an invalid v2 file shows
 * every provider as unavailable and never falls back to version 1; without v2
 * the version 1 manifest is read exactly as before. Changed version 1
 * manifests never reuse another host's quota samples.
 */
export class AdditionalAccountService {
  private readonly ccsDir: string;
  private fingerprint = '';
  private caches = new Map<string, SourceCache>();
  private manifestPending: Promise<Manifest> | null = null;
  private lastManifest: Manifest | null = null;

  constructor(private readonly deps: AdditionalAccountDeps = {}) {
    this.ccsDir = path.resolve(deps.ccsDir ?? getCcsDir());
  }

  private async readManifest(): Promise<Manifest> {
    // An injected version 1 reader without a registry reader is a version 1 fixture.
    const readRegistry =
      this.deps.readRegistry ??
      (this.deps.readManifest
        ? async (): Promise<AccountRegistryRead> => ({ state: 'absent' })
        : () => readAccountRegistry(this.ccsDir));
    const registry = await readRegistry();
    if (registry.state === 'ok') {
      return {
        mode: 'v2',
        // Accounts own their caches, so editing the list keeps unchanged accounts' samples.
        fingerprint: 'account-registry-v2',
        valid: true,
        sources: registry.registry.accounts.map(registrySource),
      };
    }
    if (registry.state === 'invalid') {
      return {
        mode: 'v2-invalid',
        fingerprint: 'invalid-account-registry',
        valid: false,
        sources: DEFAULT_SOURCES,
      };
    }
    try {
      const contents = await (
        this.deps.readManifest ?? (() => readSourceManifestFile(this.ccsDir))
      )();
      return { mode: 'v1', ...parseSourceManifest(contents) };
    } catch {
      return {
        mode: 'v1',
        fingerprint: 'invalid-source-config',
        valid: false,
        sources: DEFAULT_SOURCES,
      };
    }
  }

  private loadManifest(): Promise<Manifest> {
    this.manifestPending ??= this.readManifest()
      .catch(
        (): Manifest => ({
          mode: 'v2-invalid',
          fingerprint: 'invalid-account-registry',
          valid: false,
          sources: DEFAULT_SOURCES,
        })
      )
      .then((loaded) => {
        this.lastManifest = loaded;
        return loaded;
      })
      .finally(() => {
        this.manifestPending = null;
      });
    return this.manifestPending;
  }

  private async collect(
    source: AdditionalUsageSource,
    cache: SourceCache
  ): Promise<DashboardAccount> {
    let invalidIdentity = false;
    let museTransientFailure = false;
    try {
      const contents = await (this.deps.runSource ?? runAdditionalUsageSource)(source);
      const normalized = normalize(source, contents);
      cache.result = normalized.account;
      invalidIdentity = normalized.invalidIdentity;
      museTransientFailure = normalized.museTransientFailure;
    } catch (error) {
      cache.result =
        error instanceof AdditionalUsageTransportError && error.helperOutdated
          ? unavailable(
              source,
              'unavailable',
              `Update the usage helper on ${PLATFORM_LABELS[source.platform]}.`
            )
          : unavailable(
              source,
              'error',
              error instanceof AdditionalUsageTransportError && error.timedOut
                ? 'Account usage request timed out.'
                : 'Account usage is temporarily unavailable.'
            );
    }
    if (
      source.provider === 'muse' &&
      (invalidIdentity ||
        cache.result.status === 'needs_sign_in' ||
        (cache.lastGood && !matchesMuseIdentity(cache.result, cache.lastGood)) ||
        (!['ok', 'cached'].includes(cache.result.status) && !museTransientFailure))
    ) {
      // Only explicitly classified transient failures can retain Muse quota.
      // Unknown failures and rejected bindings also clear later retry state.
      cache.lastGood = null;
    }
    cache.fetchedAt = (this.deps.now ?? Date.now)();
    cache.failed = cache.result.status !== 'ok' && cache.result.status !== 'cached';
    if (!cache.failed) {
      if (
        source.provider === 'muse' &&
        cache.result.status === 'cached' &&
        cache.lastGood &&
        matchesMuseIdentity(cache.result, cache.lastGood) &&
        observationTime(cache.result) < observationTime(cache.lastGood)
      ) {
        cache.result = {
          ...copy(cache.lastGood),
          status: 'cached',
          message: cache.result.message,
        };
      }
      cache.lastGood = copy(cache.result);
    } else if (
      cache.lastGood &&
      ((source.provider !== 'muse' && cache.result.status === 'error') ||
        (source.provider === 'muse' &&
          (cache.result.status === 'unavailable' || cache.result.status === 'error') &&
          museTransientFailure &&
          matchesMuseIdentity(cache.result, cache.lastGood)))
    ) {
      cache.result = {
        ...copy(cache.lastGood),
        status: 'cached',
        message:
          source.provider === 'muse'
            ? 'Showing the last successful Muse usage reading. Usage refreshes automatically.'
            : 'Live account usage is temporarily unavailable; showing the last saved sample.',
      };
    }
    return cache.result;
  }

  private async getSource(
    source: AdditionalUsageSource,
    refresh: boolean
  ): Promise<DashboardAccount> {
    if (!isCollectableSource(source)) {
      return unavailable(
        source,
        'unavailable',
        'This account type is not read by the usage helper yet.'
      );
    }
    const key = sourceKey(source);
    let cache = this.caches.get(key);
    if (!cache) {
      cache = {
        result: null,
        lastGood: null,
        fetchedAt: -Infinity,
        refreshedAt: -Infinity,
        failed: false,
        pending: null,
      };
      this.caches.set(key, cache);
    }
    const now = (this.deps.now ?? Date.now)();
    const force = refresh && now - cache.refreshedAt >= REFRESH_DEBOUNCE_MS;
    const backoff = cache.failed && now - cache.fetchedAt < FAILURE_BACKOFF_MS;
    const expired = now - cache.fetchedAt >= (cache.failed ? FAILURE_BACKOFF_MS : CACHE_TTL_MS);
    if (force) cache.refreshedAt = now;
    if (!cache.pending && !backoff && (force || expired || !cache.result)) {
      const current = cache;
      current.pending = this.collect(source, current).finally(() => {
        current.pending = null;
      });
    }
    if (cache.pending) return copy(await cache.pending);
    return copy(cache.result ?? unavailable(source), true);
  }

  async snapshot(
    opts: { refresh?: boolean; excludeAntigravity?: boolean } = {}
  ): Promise<AdditionalAccountsSnapshot> {
    const config = await this.loadManifest();
    if (config.fingerprint !== this.fingerprint) {
      this.fingerprint = config.fingerprint;
      this.caches = new Map();
    }
    if (config.mode === 'v2-invalid') {
      return {
        registry: config.mode,
        accounts: config.sources.map((source) =>
          unavailable(source, 'unavailable', 'Account list could not be read safely.')
        ),
      };
    }
    if (!config.valid) {
      return {
        registry: config.mode,
        accounts: config.sources.map((source) =>
          unavailable(source, 'error', 'Account usage source configuration is invalid.')
        ),
      };
    }
    const current = new Set(config.sources.map(sourceKey));
    for (const key of this.caches.keys()) if (!current.has(key)) this.caches.delete(key);
    return {
      registry: config.mode,
      accounts: await Promise.all(
        config.sources
          .filter(
            (source) => !(opts.excludeAntigravity === true && source.provider === 'antigravity')
          )
          .map((source) => this.getSource(source, opts.refresh === true))
      ),
    };
  }

  async get(
    opts: { refresh?: boolean; excludeAntigravity?: boolean } = {}
  ): Promise<DashboardAccount[]> {
    return (await this.snapshot(opts)).accounts;
  }

  /**
   * Placeholder rows for the accounts the last loaded list names, for a
   * response that cannot wait for collection; null before the first load.
   */
  configured(opts: { excludeAntigravity?: boolean } = {}): AdditionalAccountsSnapshot | null {
    const config = this.lastManifest;
    if (!config) return null;
    return {
      registry: config.mode,
      accounts: config.sources
        .filter(
          (source) => !(opts.excludeAntigravity === true && source.provider === 'antigravity')
        )
        .map((source) =>
          unavailable(
            source,
            'unavailable',
            config.mode === 'v2-invalid'
              ? 'Account list could not be read safely.'
              : 'Account usage is temporarily unavailable.'
          )
        ),
    };
  }
}

const services = new Map<string, AdditionalAccountService>();

function scopedService(): AdditionalAccountService {
  const scope = path.resolve(getCcsDir());
  let service = services.get(scope);
  if (!service) {
    service = new AdditionalAccountService({ ccsDir: scope });
    services.set(scope, service);
    while (services.size > MAX_SCOPES) {
      const oldest = services.keys().next().value;
      if (oldest === undefined) break;
      services.delete(oldest);
    }
  }
  return service;
}

export function getAdditionalDashboardAccounts(
  opts: { refresh?: boolean; excludeAntigravity?: boolean } = {}
): Promise<DashboardAccount[]> {
  return scopedService().get(opts);
}

export function getAdditionalDashboardSnapshot(
  opts: { refresh?: boolean; excludeAntigravity?: boolean } = {}
): Promise<AdditionalAccountsSnapshot> {
  return scopedService().snapshot(opts);
}

export function getConfiguredAdditionalAccounts(
  opts: { excludeAntigravity?: boolean } = {}
): AdditionalAccountsSnapshot | null {
  return services.get(path.resolve(getCcsDir()))?.configured(opts) ?? null;
}
