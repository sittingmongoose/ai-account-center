import { AntigravityError } from './errors';
import type {
  AntigravityDashboardAccount,
  AntigravityPoolWindow,
  AntigravityInventory,
  AntigravityUsageProfile,
  AntigravityUsageSample,
} from './usage-contract';
import {
  IDENTITY_UNBOUND_MESSAGE,
  displayText,
  email,
  poolWindow,
  publicProfile,
  safeId,
  timestamp,
} from './usage-normalization';

export interface AntigravityUsageDependencies {
  readProfiles(): Promise<AntigravityUsageProfile[]>;
  collectQuota(profileId: string): Promise<AntigravityUsageSample>;
  now?: () => number;
}

interface Cache {
  revision: string;
  result: AntigravityDashboardAccount | null;
  lastGood: AntigravityDashboardAccount | null;
  checkedAt: number;
  refreshedAt: number;
  retryAt: number;
  failed: boolean;
  pending: Promise<AntigravityDashboardAccount> | null;
}

const TTL_MS = 120_000;
const FAILURE_BACKOFF_MS = 30_000;
const REFRESH_DEBOUNCE_MS = 5_000;
const MAX_PROFILES = 16;

function clone(account: AntigravityDashboardAccount): AntigravityDashboardAccount {
  return {
    ...account,
    windows: account.windows.map((window) => ({
      ...window,
      ...(window.modelIds ? { modelIds: [...window.modelIds] } : {}),
    })),
    capabilities: {
      ...account.capabilities,
      claudePlatforms: [],
      antigravityHostIds: [...account.capabilities.antigravityHostIds],
    },
  };
}

function fallback(profile: AntigravityUsageProfile): AntigravityDashboardAccount {
  const publicRow = publicProfile(profile);
  return {
    id: `antigravity:profile:${publicRow.id}`,
    provider: 'antigravity',
    providerLabel: 'Antigravity',
    label: publicRow.email,
    email: publicRow.email,
    plan: publicRow.plan,
    platform: 'ubuntu',
    source: 'Antigravity saved login on Ubuntu',
    status: publicRow.available ? 'unavailable' : 'needs_sign_in',
    message: publicRow.available
      ? 'Antigravity usage is temporarily unavailable.'
      : 'This Antigravity profile needs a verified sign-in.',
    fetchedAt: null,
    sampledAt: null,
    isActive: publicRow.selected,
    windows: [],
    capabilities: {
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
      antigravityProfileId: publicRow.id,
      antigravityHostIds: publicRow.available ? ['ubuntu'] : [],
    },
  };
}

/** Account-specific, revision-bound cache; inactive sampling never switches live auth. */
export class AntigravityUsageService {
  private readonly caches = new Map<string, Cache>();
  constructor(private readonly deps: AntigravityUsageDependencies) {}

  private async profiles(): Promise<AntigravityUsageProfile[]> {
    const profiles = await this.deps.readProfiles();
    if (!Array.isArray(profiles) || profiles.length > MAX_PROFILES)
      throw new AntigravityError('Invalid Antigravity inventory.');
    const ids = new Set<string>();
    const identities = new Set<string>();
    let selectedCount = 0;
    for (const profile of profiles) {
      publicProfile(profile);
      if (
        !safeId(profile.id) ||
        ids.has(profile.id) ||
        typeof profile.identityKey !== 'string' ||
        !profile.identityKey ||
        profile.identityKey.length > 512 ||
        typeof profile.credentialRevision !== 'string' ||
        !profile.credentialRevision ||
        profile.credentialRevision.length > 128 ||
        [
          profile.identityVerified,
          profile.available,
          profile.selected,
          profile.runtimeVerified,
        ].some((value) => typeof value !== 'boolean') ||
        (profile.identityVerified && identities.has(profile.identityKey))
      )
        throw new AntigravityError('Ambiguous Antigravity inventory.');
      ids.add(profile.id);
      if (profile.identityVerified) identities.add(profile.identityKey);
      if (profile.identityVerified && profile.selected) selectedCount += 1;
    }
    if (selectedCount > 1) throw new AntigravityError('Ambiguous active Antigravity account.');
    for (const id of this.caches.keys()) if (!ids.has(id)) this.caches.delete(id);
    return profiles;
  }

  async getInventory(): Promise<AntigravityInventory> {
    return {
      schemaVersion: 1,
      hostId: 'ubuntu',
      profiles: (await this.profiles()).map(publicProfile),
    };
  }

  private async collect(
    profile: AntigravityUsageProfile,
    cache: Cache
  ): Promise<AntigravityDashboardAccount> {
    const result = fallback(profile);
    const now = (this.deps.now ?? Date.now)();
    let invalidateLastGood = false;
    let retryAfter = FAILURE_BACKOFF_MS;
    let reportedBackoff = false;
    try {
      const sample = await this.deps.collectQuota(profile.id);
      const sampleEmail = email(sample.email);
      const successful =
        sample.status === 'ok' || sample.status === 'fresh' || sample.status === 'cached';
      if (
        sample.profileId !== profile.id ||
        sample.identityKey !== profile.identityKey ||
        sample.credentialRevision !== profile.credentialRevision ||
        (sample.email !== null && sampleEmail?.toLowerCase() !== profile.email.toLowerCase()) ||
        (successful && sampleEmail === null)
      ) {
        invalidateLastGood = true;
        result.status = 'error';
        // The one failure that says this saved account may not be the account
        // it claims: clients stop offering it as a switch target.
        result.statusReason = 'identity_unbound';
        result.message = IDENTITY_UNBOUND_MESSAGE;
      } else {
        result.plan = displayText(sample.plan, 80) ?? result.plan;
        if (
          Number.isInteger(sample.retryAfterSeconds) &&
          (sample.retryAfterSeconds as number) > 0
        ) {
          retryAfter = Math.min(86_400, sample.retryAfterSeconds as number) * 1000;
          reportedBackoff = true;
        }
        result.status =
          sample.status === 'fresh' || sample.status === 'ok'
            ? 'ok'
            : sample.status === 'cached' ||
                sample.status === 'needs_sign_in' ||
                sample.status === 'unavailable'
              ? sample.status
              : 'error';
        result.fetchedAt = timestamp(sample.fetchedAt);
        result.sampledAt = timestamp(sample.sampledAt);
        if (
          (result.status === 'ok' || result.status === 'cached') &&
          result.sampledAt &&
          Date.parse(result.sampledAt) <= now + 30_000
        ) {
          result.windows = (Array.isArray(sample.windows) ? sample.windows : [])
            .slice(0, 256)
            .map(poolWindow)
            .filter((window): window is AntigravityPoolWindow => window !== null);
          if (!result.windows.length) result.status = 'error';
        } else if (result.status === 'ok' || result.status === 'cached') result.status = 'error';
        if (result.status === 'needs_sign_in') invalidateLastGood = true;
        result.message =
          result.status === 'ok'
            ? null
            : result.status === 'cached'
              ? 'Showing the last successful Antigravity usage reading.'
              : result.status === 'needs_sign_in'
                ? 'This Antigravity profile needs to sign in again.'
                : 'Antigravity usage is temporarily unavailable.';
      }
    } catch {
      result.status = 'error';
      result.message = 'Antigravity usage is temporarily unavailable.';
    }
    if (invalidateLastGood) cache.lastGood = null;
    const valid = result.status === 'ok' || result.status === 'cached';
    if (valid) {
      if (
        cache.lastGood &&
        Date.parse(result.sampledAt as string) < Date.parse(cache.lastGood.sampledAt as string)
      ) {
        cache.result = {
          ...clone(cache.lastGood),
          isActive: profile.selected,
          status: 'cached',
          message: 'Showing the last successful Antigravity usage reading.',
        };
      } else cache.result = result;
      cache.lastGood = clone(cache.result);
      cache.retryAt = result.status === 'cached' && reportedBackoff ? now + retryAfter : now;
    } else {
      cache.result = cache.lastGood
        ? {
            ...clone(cache.lastGood),
            isActive: profile.selected,
            status: 'cached',
            message:
              'Live Antigravity usage is temporarily unavailable; showing the last saved sample.',
          }
        : result;
      cache.retryAt = now + retryAfter;
    }
    cache.failed = !valid;
    cache.checkedAt = now;
    return clone(cache.result);
  }

  private async getProfile(
    profile: AntigravityUsageProfile,
    refresh: boolean
  ): Promise<AntigravityDashboardAccount> {
    if (!profile.identityVerified || !profile.available) {
      this.caches.delete(profile.id);
      return fallback(profile);
    }
    const revision = JSON.stringify([profile.identityKey, profile.credentialRevision]);
    let cache = this.caches.get(profile.id);
    if (!cache || cache.revision !== revision) {
      cache = {
        revision,
        result: null,
        lastGood: null,
        checkedAt: -Infinity,
        refreshedAt: -Infinity,
        retryAt: -Infinity,
        failed: false,
        pending: null,
      };
      this.caches.set(profile.id, cache);
    }
    const now = (this.deps.now ?? Date.now)();
    const forced = refresh && now - cache.refreshedAt >= REFRESH_DEBOUNCE_MS;
    if (forced) cache.refreshedAt = now;
    const expired = now - cache.checkedAt >= (cache.failed ? FAILURE_BACKOFF_MS : TTL_MS);
    if (!cache.pending && now >= cache.retryAt && (forced || !cache.result || expired)) {
      const current = cache;
      current.pending = this.collect(profile, current).finally(() => {
        current.pending = null;
      });
    }
    const result = cache.pending ? await cache.pending : (cache.result ?? fallback(profile));
    return { ...clone(result), isActive: profile.selected };
  }

  async getAccounts(options: { refresh?: boolean } = {}): Promise<AntigravityDashboardAccount[]> {
    const initial = await this.profiles();
    const accounts = await Promise.all(
      initial.map((profile) => this.getProfile(profile, options.refresh === true))
    );
    // A manual activation/import can race an in-flight quota read. Recheck the
    // private registry before publishing its sample; do not reuse old identity.
    const current = await this.profiles();
    return current.map((profile) => {
      const previous = initial.find((candidate) => candidate.id === profile.id);
      const sampled = accounts.find(
        (candidate) => candidate.capabilities.antigravityProfileId === profile.id
      );
      if (
        !previous ||
        !sampled ||
        previous.identityKey !== profile.identityKey ||
        previous.credentialRevision !== profile.credentialRevision ||
        !profile.identityVerified ||
        !profile.available
      ) {
        this.caches.delete(profile.id);
        return fallback(profile);
      }
      return { ...clone(sampled), isActive: profile.selected };
    });
  }

  invalidate(): void {
    this.caches.clear();
  }

  /** Synchronous bounded-response fallback; never starts a helper/provider call. */
  cachedAccounts(profiles: AntigravityUsageProfile[]): AntigravityDashboardAccount[] {
    return profiles.map((profile) => {
      const cache = this.caches.get(profile.id);
      const revision = JSON.stringify([profile.identityKey, profile.credentialRevision]);
      if (
        !profile.identityVerified ||
        !profile.available ||
        !cache?.result ||
        cache.revision !== revision
      )
        return fallback(profile);
      return { ...clone(cache.result), isActive: profile.selected };
    });
  }
}

/** Registered profile rows replace the legacy one-source Antigravity inventory. */
export function mergeAntigravityAccounts<T extends { id: string; provider: string }>(
  existing: T[],
  profiles: AntigravityDashboardAccount[]
): Array<T | AntigravityDashboardAccount> {
  if (!profiles.length) return [...existing];
  return [...existing.filter((account) => account.provider !== 'antigravity'), ...profiles];
}
