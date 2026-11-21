import path from 'path';
import { CursorQuotaReader } from './cursor-account-reader';
import { getCcsDir } from '../../utils/config-manager';
import { LOCAL_FILE_DEADLINE_MS, withDeadline } from '../../utils/settle-deadline';
import type {
  DashboardAccount,
  DashboardAccountWindow,
  DashboardPlatform,
} from './account-dashboard-types';
import {
  antigravityPlanDisplay,
  cloneAntigravityPlan,
  type AntigravityReportedPlan,
} from '../../antigravity/plan';
import { parseSourceManifest, readSourceManifestFile } from './account-usage-manifest';
import {
  readAccountRegistry,
  registrySource,
  type AccountRegistryRead,
} from './account-registry-v2';
import {
  classifyAccountRegistry,
  isUnreadableAccountRegistry,
  type AdditionalRegistryMode,
} from './account-registry-mode';
import {
  AdditionalUsageTransportError,
  isCollectableSource,
  runAdditionalUsageSource,
  type AdditionalProvider,
  type AdditionalUsageSource,
} from './additional-usage-transport';
import { HOST_LABELS, sourceComputer, type DashboardHost } from './dashboard-hosts';
import { getUsageFailoverService, type UsageFailoverService } from './usage-failover-service';
import {
  sameUsageIdentity,
  type UsageFailoverTarget,
  type UsageIdentity,
} from './usage-failover-contract';

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
const MUSE_CACHED_MESSAGES = new Set([
  'Muse is limiting requests; showing the last successful usage reading. Refresh resumes automatically.',
]);
/** Sent by helpers synced before the home stopped showing it; the cached status and sample time already say so. */
const MUSE_LEGACY_CACHED_MESSAGE =
  'Showing the last successful Muse usage reading. Usage refreshes automatically.';
const MUSE_TRANSIENT_FAILURES = new Set(['rate_limited', 'provider_error', 'network_error']);

export interface AdditionalAccountDeps {
  ccsDir?: string;
  cursorQuota?: CursorQuotaReader | null;
  /** Version 1 manifest contents (`account-usage-sources.json`), read when registry v2 is absent. */
  readManifest?: () => Promise<string | null>;
  /** Registry v2 (`account-usage-accounts.json`); absent, invalid or valid. */
  readRegistry?: () => Promise<AccountRegistryRead>;
  runSource?: (source: AdditionalUsageSource) => Promise<string>;
  /** Shared identity-bound source selection; null keeps the legacy collector path. */
  failover?:
    | (Pick<UsageFailoverService, 'collect' | 'peek'> &
        Partial<Pick<UsageFailoverService, 'readingIdentity' | 'discoveredIdentities'>>)
    | null;
  now?: () => number;
  /** Overrides LOCAL_FILE_DEADLINE_MS for the account list read, so tests need not wait. */
  manifestDeadlineMs?: number;
}

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

/**
 * The computer a source runs on. Nas1, the second Ubuntu computer, is stored as
 * platform 'ubuntu' plus its fixed ssh alias and never as a platform of its
 * own, so registry files and older packages read it unchanged. Sources and
 * registry entries both fit, so the account removal wording shares this.
 */
export function sourceHost(source: {
  platform: DashboardPlatform;
  sshHost?: string | null;
}): DashboardHost {
  return sourceComputer(source);
}

function unavailable(
  source: AdditionalUsageSource,
  status: DashboardAccount['status'] = 'unavailable',
  message = 'Saved account usage is unavailable on this computer.'
): DashboardAccount {
  const host = sourceHost(source);
  return {
    id: source.account?.id ?? `${source.provider}:usage`,
    provider: source.provider,
    providerLabel: LABELS[source.provider],
    label: source.account?.label ?? LABELS[source.provider],
    email: null,
    plan: null,
    platform: source.platform,
    ...(host !== source.platform ? { host } : {}),
    source: `Account on ${HOST_LABELS[host]}`,
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
  if (status === 'cached') {
    account.cacheReason =
      result.failureCode === 'rate_limited' ? 'rate_limited' : 'outcome_unknown';
  }
  if (source.provider === 'antigravity')
    Object.assign(
      account,
      antigravityPlanDisplay(
        displayText(result.plan, 80),
        account.sampledAt ?? account.fetchedAt,
        record(result.reportedPlan) ? (result.reportedPlan as AntigravityReportedPlan) : undefined
      )
    );
  if (status === 'ok' || status === 'cached') {
    account.windows = Array.isArray(result.windows)
      ? result.windows
          .slice(0, MAX_USAGE_WINDOWS)
          .map((window, index) =>
            usageWindow(
              source.provider === 'antigravity' &&
                record(window) &&
                window.key === 'google-ai-credits'
                ? { ...window, label: 'AI credits (overage)' }
                : window,
              index
            )
          )
          .filter((window): window is DashboardAccountWindow => window !== null)
      : [];
    account.message =
      source.provider === 'muse' && status === 'cached'
        ? typeof result.message === 'string' && result.message !== MUSE_LEGACY_CACHED_MESSAGE
          ? MUSE_CACHED_MESSAGES.has(result.message)
            ? result.message
            : 'Showing the last saved Muse usage sample.'
          : null
        : null;
    if (account.windows.length === 0 && account.antigravityPlan?.quotaPolicy !== 'none') {
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

function copy(account: DashboardAccount): DashboardAccount {
  return {
    ...account,
    ...(account.antigravityPlan
      ? {
          antigravityPlan: cloneAntigravityPlan(account.antigravityPlan),
        }
      : {}),
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

/** Providers whose discover entry reads every stored key, one row each (owner, card OCG1). */
const EXPANDS_DISCOVERY: ReadonlySet<string> = new Set(['opencode-go']);

/**
 * Per-CCS-scope cache, one entry per account (CONTRACT-registry-lifecycle 3.1).
 * Read order: a valid registry v2 is the only source; an invalid v2 file shows
 * every provider as unavailable and never falls back to version 1; without v2
 * the version 1 manifest is read exactly as before. Changed version 1
 * manifests never reuse another host's quota samples.
 */
export class AdditionalAccountService {
  private readonly ccsDir: string;
  private readonly failover:
    | (Pick<UsageFailoverService, 'collect' | 'peek'> &
        Partial<Pick<UsageFailoverService, 'readingIdentity' | 'discoveredIdentities'>>)
    | null;
  private readonly cursorQuota: CursorQuotaReader | null;
  private fingerprint = '';
  private caches = new Map<string, SourceCache>();
  private manifestPending: Promise<Manifest> | null = null;
  private lastManifest: Manifest | null = null;
  /** Sources a discover entry expanded into, pinned to one stored key's identity. */
  private pinned = new WeakMap<AdditionalUsageSource, UsageIdentity>();
  /** The last snapshot's sources after discovery expanded, for placeholders and Re-check. */
  private lastSources: { fingerprint: string; sources: AdditionalUsageSource[] } | null = null;

  constructor(private readonly deps: AdditionalAccountDeps = {}) {
    this.ccsDir = path.resolve(deps.ccsDir ?? getCcsDir());
    this.cursorQuota =
      deps.cursorQuota === undefined
        ? deps.runSource || deps.failover !== undefined
          ? null
          : new CursorQuotaReader({ ccsDir: this.ccsDir, now: deps.now })
        : deps.cursorQuota;
    // Existing injected transports remain isolated fixtures. Production uses
    // the shared coordinator so every account keeps one verified identity.
    this.failover =
      deps.failover === undefined
        ? deps.runSource
          ? null
          : getUsageFailoverService()
        : deps.failover;
  }

  private failoverTarget(source: AdditionalUsageSource): UsageFailoverTarget {
    const credential = source.account?.credential;
    const selector =
      !credential || credential.kind === 'discover'
        ? 'discover'
        : credential.kind === 'aac-key'
          ? `aac-key:${credential.keyId}`
          : credential.kind === 'browser-capsule'
            ? `browser-capsule:${credential.capsuleId}`
            : credential.kind === 'config-home'
              ? `config-home:${credential.homeId}`
              : `antigravity-profile:${credential.profileId}`;
    return {
      accountId: source.account?.id ?? `${source.provider}:usage`,
      provider: source.provider,
      ccsDir: this.ccsDir,
      // The registry row names the account and its owned credential. Labels
      // do not establish identity and changing a label does not rebind it.
      binding: sourceKey(source),
      primarySources: [
        {
          host: sourceHost(source),
          platform: source.platform,
          sshHost: source.sshHost ?? null,
          selector,
        },
      ],
      ...(this.pinned.has(source) ? { expectedIdentity: this.pinned.get(source) } : {}),
    };
  }

  /**
   * Owner, 2026-11-21 (card OCG1): every OpenCode Go key stored on the computer is read, once.
   * A `discover` entry stands for each stored key that no other entry of the provider already
   * names (`aac-key:<id>`): one row per such key, pinned to that key's identity, with an id and
   * label from the identity's digest (never the key). When every stored key is already named,
   * the discover entry adds no row and no read. An unreadable store keeps the entry as it was.
   */
  private async expandDiscovery(
    sources: AdditionalUsageSource[]
  ): Promise<AdditionalUsageSource[]> {
    const discover = this.failover?.discoveredIdentities?.bind(this.failover);
    if (!discover) return sources;
    const expanded = await Promise.all(
      sources.map(async (source): Promise<AdditionalUsageSource[]> => {
        const account = source.account;
        if (
          !EXPANDS_DISCOVERY.has(source.provider) ||
          !account ||
          account.credential.kind !== 'discover'
        )
          return [source];
        const bound = sources
          .filter(
            (other) =>
              other !== source &&
              other.provider === source.provider &&
              sourceHost(other) === sourceHost(source) &&
              (other.sshHost ?? null) === (source.sshHost ?? null)
          )
          .flatMap((other) => {
            const credential = other.account?.credential;
            return credential?.kind === 'aac-key' ? [`aac-key:${credential.keyId}`] : [];
          });
        const found = await discover(this.failoverTarget(source), bound).catch(() => null);
        if (!found || found.length === 0) return [source];
        const unbound = found.filter((item) => !item.bound);
        return unbound.map(({ identity }) => {
          const digest = identity.key.slice(0, 8);
          const pinned: AdditionalUsageSource = {
            ...source,
            account: {
              ...account,
              id: `${account.id}:${digest}`,
              label: `${account.label ?? LABELS[source.provider]} ${digest}`,
            },
          };
          this.pinned.set(pinned, identity);
          return pinned;
        });
      })
    );
    return expanded.flat();
  }

  private async readManifest(): Promise<Manifest> {
    // An injected version 1 reader without a registry reader is a version 1 fixture.
    const readRegistry =
      this.deps.readRegistry ??
      (this.deps.readManifest
        ? async (): Promise<AccountRegistryRead> => ({ state: 'absent' })
        : () => readAccountRegistry(this.ccsDir));
    const active = classifyAccountRegistry(await readRegistry());
    if (active.mode === 'v2') {
      return {
        mode: active.mode,
        // Accounts own their caches, so editing the list keeps unchanged accounts' samples.
        fingerprint: 'account-registry-v2',
        valid: true,
        sources: active.registry.accounts.map(registrySource),
      };
    }
    if (isUnreadableAccountRegistry(active.mode)) {
      return {
        mode: active.mode,
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
    const unreadable = (): Manifest => ({
      mode: 'v2-invalid',
      fingerprint: 'invalid-account-registry',
      valid: false,
      sources: DEFAULT_SOURCES,
    });
    // One read at a time: a hung disk holds one, and later callers share it.
    this.manifestPending ??= this.readManifest()
      .catch(unreadable)
      .then((loaded) => {
        this.lastManifest = loaded;
        return loaded;
      })
      .finally(() => {
        this.manifestPending = null;
      });
    // A read past its deadline keeps the last list in force rather than holding every row.
    return withDeadline(
      this.manifestPending,
      this.deps.manifestDeadlineMs ?? LOCAL_FILE_DEADLINE_MS,
      undefined,
      'Account list read'
    ).catch(() => this.lastManifest ?? unreadable());
  }

  private async collect(
    source: AdditionalUsageSource,
    cache: SourceCache,
    force = false
  ): Promise<DashboardAccount> {
    if (source.provider === 'cursor' && this.cursorQuota) {
      const quota = await this.cursorQuota.read(unavailable(source), force);
      if (quota) {
        cache.result = quota;
        cache.lastGood = null;
        cache.fetchedAt = (this.deps.now ?? Date.now)();
        cache.failed = !['ok', 'cached'].includes(quota.status);
        return quota;
      }
    }
    if (this.failover) {
      try {
        cache.result = await this.failover.collect(
          this.failoverTarget(source),
          unavailable(source)
        );
      } catch {
        cache.result = unavailable(source, 'error', 'Account usage is temporarily unavailable.');
      }
      // The coordinator owns identity checks, cached observations and expiry.
      // Reapplying the old per-host lastGood cache could undo its invalidation.
      cache.lastGood = null;
      cache.fetchedAt = (this.deps.now ?? Date.now)();
      cache.failed = !['ok', 'cached'].includes(cache.result.status);
      return cache.result;
    }
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
              `Update the usage helper on ${HOST_LABELS[sourceHost(source)]}.`
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
        cacheReason: 'read_failed',
        message:
          source.provider === 'muse'
            ? null
            : 'Live account usage is temporarily unavailable; showing the last saved sample.',
      };
    }
    return cache.result;
  }

  private async getSource(
    source: AdditionalUsageSource,
    refresh: boolean
  ): Promise<DashboardAccount> {
    if (!this.failover && !isCollectableSource(source)) {
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
      current.pending = this.collect(source, current, force).finally(() => {
        current.pending = null;
      });
    }
    if (cache.pending) return copy(await cache.pending);
    return copy(cache.result ?? unavailable(source));
  }

  async snapshot(
    opts: { refresh?: boolean; excludeAntigravity?: boolean } = {}
  ): Promise<AdditionalAccountsSnapshot> {
    const config = await this.loadManifest();
    if (config.fingerprint !== this.fingerprint) {
      this.fingerprint = config.fingerprint;
      this.caches = new Map();
    }
    if (isUnreadableAccountRegistry(config.mode)) {
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
    const all = await this.expandDiscovery(config.sources);
    this.lastSources = { fingerprint: config.fingerprint, sources: all };
    const current = new Set(all.map(sourceKey));
    for (const key of this.caches.keys()) if (!current.has(key)) this.caches.delete(key);
    const sources = all.filter(
      (source) => !(opts.excludeAntigravity === true && source.provider === 'antigravity')
    );
    return {
      registry: config.mode,
      accounts: this.oneRowPerIdentity(
        sources,
        await Promise.all(sources.map((source) => this.getSource(source, opts.refresh === true)))
      ),
    };
  }

  /**
   * Two registry entries whose readings came from the same credential identity are one
   * account: show it once (never two rows of the same account). The row bound to its own
   * credential stays over a `discover` row, then the first in list order. Rows without a
   * known identity are never merged. Nothing is rebound and no provider is asked.
   */
  private oneRowPerIdentity(
    sources: AdditionalUsageSource[],
    rows: DashboardAccount[]
  ): DashboardAccount[] {
    const readingIdentity = this.failover?.readingIdentity?.bind(this.failover);
    if (!readingIdentity) return rows;
    const identities = sources.map((source) => readingIdentity(this.failoverTarget(source)));
    const discovered = (index: number) => {
      const credential = sources[index].account?.credential;
      return !credential || credential.kind === 'discover';
    };
    const dropped = new Set<number>();
    for (let index = 0; index < rows.length; index++) {
      if (dropped.has(index) || !identities[index]) continue;
      for (let other = index + 1; other < rows.length; other++) {
        if (
          dropped.has(other) ||
          sources[other].provider !== sources[index].provider ||
          !sameUsageIdentity(identities[index], identities[other])
        )
          continue;
        if (discovered(index) && !discovered(other)) {
          dropped.add(index);
          break;
        }
        dropped.add(other);
      }
    }
    return rows.filter((_, index) => !dropped.has(index));
  }

  async get(
    opts: { refresh?: boolean; excludeAntigravity?: boolean } = {}
  ): Promise<DashboardAccount[]> {
    return (await this.snapshot(opts)).accounts;
  }

  /**
   * Re-check one account now (CONTRACT-registry-lifecycle 6.5): the cache TTL
   * is skipped, the failure backoff and the refresh debounce are not. Null when
   * the current list does not name the reviewed account and source.
   */
  async refreshAccount(
    id: string,
    reviewed?: AdditionalUsageSource
  ): Promise<DashboardAccount | null> {
    const config = await this.loadManifest();
    if (!config.valid) return null;
    const known =
      this.lastSources?.fingerprint === config.fingerprint ? this.lastSources.sources : [];
    const source = [...config.sources, ...known].find(
      (candidate) => (candidate.account?.id ?? `${candidate.provider}:usage`) === id
    );
    if (!source || (reviewed && sourceKey(reviewed) !== sourceKey(source))) return null;
    return this.getSource(source, true);
  }

  /**
   * Placeholder rows for the accounts the last loaded list names, for a
   * response that cannot wait for collection; null before the first load.
   */
  configured(opts: { excludeAntigravity?: boolean } = {}): AdditionalAccountsSnapshot | null {
    const config = this.lastManifest;
    if (!config) return null;
    const expanded =
      this.lastSources?.fingerprint === config.fingerprint ? this.lastSources.sources : null;
    const sources = (expanded ?? config.sources).filter(
      (source) => !(opts.excludeAntigravity === true && source.provider === 'antigravity')
    );
    const rows = sources.map((source) => {
      const placeholder = unavailable(
        source,
        'unavailable',
        isUnreadableAccountRegistry(config.mode)
          ? 'Account list could not be read safely.'
          : 'Account usage is temporarily unavailable.'
      );
      const quota =
        config.valid && source.provider === 'cursor' ? this.cursorQuota?.peek(placeholder) : null;
      if (quota) return quota;
      return config.valid && this.failover
        ? this.failover.peek(this.failoverTarget(source), placeholder)
        : placeholder;
    });
    return {
      registry: config.mode,
      accounts: config.valid ? this.oneRowPerIdentity(sources, rows) : rows,
    };
  }
}

/**
 * Run one source once, outside every cache (a new key's check, 6.2 and 6.4).
 * A transport failure reads as an `error` row; nothing is retained.
 */
export async function probeAdditionalSource(
  source: AdditionalUsageSource,
  runSource: (source: AdditionalUsageSource) => Promise<string> = runAdditionalUsageSource
): Promise<DashboardAccount> {
  if (!isCollectableSource(source)) return unavailable(source);
  try {
    return normalize(source, await runSource(source)).account;
  } catch {
    return unavailable(source, 'error', 'Account usage is temporarily unavailable.');
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

export function refreshAdditionalAccount(
  id: string,
  reviewed: AdditionalUsageSource
): Promise<DashboardAccount | null> {
  return scopedService().refreshAccount(id, reviewed);
}
