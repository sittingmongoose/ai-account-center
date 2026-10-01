import { createHash } from 'crypto';
import fs from 'fs/promises';
import path from 'path';
import { ConfigError } from '../../errors/error-types';
import { getCcsDir } from '../../utils/config-manager';
import type { DashboardAccount, DashboardAccountWindow } from './account-dashboard-types';
import {
  ADDITIONAL_PROVIDERS,
  AdditionalUsageTransportError,
  isAdditionalProvider,
  isSafeUsageSshAlias,
  runAdditionalUsageSource,
  type AdditionalProvider,
  type AdditionalUsageSource,
} from './additional-usage-transport';

const CACHE_TTL_MS = 120_000;
const REFRESH_DEBOUNCE_MS = 5_000;
const FAILURE_BACKOFF_MS = 30_000;
const MAX_MANIFEST_BYTES = 32 * 1024;
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

export interface AdditionalAccountDeps {
  ccsDir?: string;
  readManifest?: () => Promise<string | null>;
  runSource?: (source: AdditionalUsageSource) => Promise<string>;
  now?: () => number;
}

interface Manifest {
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
    id: `${source.provider}:usage`,
    provider: source.provider,
    providerLabel: LABELS[source.provider],
    label: LABELS[source.provider],
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
function normalize(source: AdditionalUsageSource, contents: string): DashboardAccount {
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
  account.label = account.email ?? LABELS[source.provider];
  const plan = displayText(result.plan, 48);
  account.plan = plan && /^[A-Za-z][A-Za-z0-9 ._+-]*$/.test(plan) ? plan : null;
  account.fetchedAt = timestamp(result.fetchedAt);
  account.sampledAt = timestamp(result.sampledAt);
  if (status === 'ok' || status === 'cached') {
    account.windows = Array.isArray(result.windows)
      ? result.windows
          .slice(0, MAX_USAGE_WINDOWS)
          .map(usageWindow)
          .filter((window): window is DashboardAccountWindow => window !== null)
      : [];
    account.message = null;
    if (account.windows.length === 0) {
      account.status = 'error';
      account.message = 'Account usage is temporarily unavailable.';
    }
  }
  return account;
}

function copy(account: DashboardAccount, cached = false): DashboardAccount {
  return {
    ...account,
    status: cached && account.status === 'ok' ? 'cached' : account.status,
    windows: account.windows.map((window) => ({ ...window })),
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}

async function readManifestFile(ccsDir: string): Promise<string | null> {
  let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
  try {
    handle = await fs.open(path.join(ccsDir, 'account-usage-sources.json'), 'r');
    const buffer = Buffer.alloc(MAX_MANIFEST_BYTES + 1);
    const { bytesRead } = await handle.read(buffer, 0, buffer.length, 0);
    if (bytesRead > MAX_MANIFEST_BYTES) throw new ConfigError('Invalid source configuration.');
    return buffer.subarray(0, bytesRead).toString('utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw new ConfigError('Invalid source configuration.');
  } finally {
    await handle?.close();
  }
}

function manifest(contents: string | null): Manifest {
  const defaults = ADDITIONAL_PROVIDERS.map((provider) => ({
    provider,
    platform: 'ubuntu' as const,
  }));
  const fingerprint = createHash('sha256')
    .update(contents ?? 'local-ubuntu-defaults')
    .digest('hex');
  if (contents === null) return { fingerprint, valid: true, sources: defaults };
  try {
    if (Buffer.byteLength(contents, 'utf8') > MAX_MANIFEST_BYTES) {
      throw new ConfigError('Invalid source configuration.');
    }
    const config: unknown = JSON.parse(contents);
    if (
      !record(config) ||
      config.version !== 1 ||
      Object.keys(config).some((key) => !['version', 'sources'].includes(key)) ||
      !Array.isArray(config.sources) ||
      config.sources.length > ADDITIONAL_PROVIDERS.length
    ) {
      throw new ConfigError('Invalid source configuration.');
    }
    const configured = new Map<AdditionalProvider, AdditionalUsageSource>();
    for (const entry of config.sources) {
      if (
        !record(entry) ||
        !isAdditionalProvider(entry.provider) ||
        !['ubuntu', 'mac', 'windows'].includes(entry.platform as string) ||
        Object.keys(entry).some((key) => !['provider', 'platform', 'sshHost'].includes(key)) ||
        (entry.sshHost !== undefined && !isSafeUsageSshAlias(entry.sshHost)) ||
        configured.has(entry.provider)
      ) {
        throw new ConfigError('Invalid source configuration.');
      }
      configured.set(entry.provider, {
        provider: entry.provider,
        platform: entry.platform as AdditionalUsageSource['platform'],
        ...(entry.sshHost === undefined ? {} : { sshHost: entry.sshHost }),
      });
    }
    return {
      fingerprint,
      valid: true,
      sources: defaults.map((source) => configured.get(source.provider) ?? source),
    };
  } catch {
    return { fingerprint, valid: false, sources: defaults };
  }
}

/** Per-CCS-scope cache. Changed source manifests never reuse another host's quota samples. */
export class AdditionalAccountService {
  private readonly ccsDir: string;
  private fingerprint = '';
  private caches = new Map<AdditionalProvider, SourceCache>();
  private manifestPending: Promise<Manifest> | null = null;

  constructor(private readonly deps: AdditionalAccountDeps = {}) {
    this.ccsDir = path.resolve(deps.ccsDir ?? getCcsDir());
  }

  private loadManifest(): Promise<Manifest> {
    this.manifestPending ??= (this.deps.readManifest ?? (() => readManifestFile(this.ccsDir)))()
      .then(manifest)
      .catch(() => ({
        fingerprint: 'invalid-source-config',
        valid: false,
        sources: manifest(null).sources,
      }))
      .finally(() => {
        this.manifestPending = null;
      });
    return this.manifestPending;
  }

  private async collect(
    source: AdditionalUsageSource,
    cache: SourceCache
  ): Promise<DashboardAccount> {
    try {
      const contents = await (this.deps.runSource ?? runAdditionalUsageSource)(source);
      cache.result = normalize(source, contents);
    } catch (error) {
      cache.result = unavailable(
        source,
        'error',
        error instanceof AdditionalUsageTransportError && error.timedOut
          ? 'Account usage request timed out.'
          : 'Account usage is temporarily unavailable.'
      );
    }
    cache.fetchedAt = (this.deps.now ?? Date.now)();
    cache.failed = cache.result.status !== 'ok' && cache.result.status !== 'cached';
    if (!cache.failed) {
      cache.lastGood = copy(cache.result);
    } else if (cache.result.status === 'error' && cache.lastGood) {
      cache.result = {
        ...copy(cache.lastGood),
        status: 'cached',
        message: 'Live account usage is temporarily unavailable; showing the last saved sample.',
      };
    }
    return cache.result;
  }

  private async getSource(
    source: AdditionalUsageSource,
    refresh: boolean
  ): Promise<DashboardAccount> {
    let cache = this.caches.get(source.provider);
    if (!cache) {
      cache = {
        result: null,
        lastGood: null,
        fetchedAt: -Infinity,
        refreshedAt: -Infinity,
        failed: false,
        pending: null,
      };
      this.caches.set(source.provider, cache);
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

  async get(opts: { refresh?: boolean } = {}): Promise<DashboardAccount[]> {
    const config = await this.loadManifest();
    if (config.fingerprint !== this.fingerprint) {
      this.fingerprint = config.fingerprint;
      this.caches = new Map();
    }
    if (!config.valid) {
      return config.sources.map((source) =>
        unavailable(source, 'error', 'Account usage source configuration is invalid.')
      );
    }
    return Promise.all(
      config.sources.map((source) => this.getSource(source, opts.refresh === true))
    );
  }
}

const services = new Map<string, AdditionalAccountService>();

export function getAdditionalDashboardAccounts(
  opts: { refresh?: boolean } = {}
): Promise<DashboardAccount[]> {
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
  return service.get(opts);
}
