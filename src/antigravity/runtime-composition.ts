import { AntigravityError } from './errors';
import { cloneAntigravityPlan } from './plan';
import path from 'path';
import { AntigravityProfileRegistry, identityKey } from './registry';
import { AntigravitySwitchService, type AntigravityRecoveryResult } from './switch-service';
import type { AntigravitySwitchDriver, NativeCredential, VerifiedIdentity } from './types';
import { AntigravityUsageService } from './usage-service';
import type {
  AntigravityApiDependencies,
  AntigravityDashboardAccount,
  AntigravityRuntimeServiceProblem,
  AntigravityUsageProfile,
  AntigravityUsageSample,
} from './usage-contract';
import { publicRuntimeServiceProblem } from './runtime-health';
import {
  holdIdentityUsageCooldown,
  readIdentityUsageCooldown,
} from '../web-server/services/usage-failover-cooldown';

/** Private worker request; bytes, subjects, identity keys and revisions never leave adapters. */
export interface SavedQuotaRequest {
  profileId: string;
  email: string;
  identityKey: string;
  credentialRevision: string;
  credential: NativeCredential;
  identity: VerifiedIdentity;
}

export interface SavedQuotaSnapshot extends AntigravityUsageSample {
  source: 'native-consumer';
  identityVerified: boolean;
  identityValidation: 'verified' | 'mismatch' | 'needs_sign_in' | 'unavailable';
}

export interface AntigravityRuntimeDependencies {
  registry: AntigravityProfileRegistry;
  /** Shared private usage scope, supplied by persistent production construction. */
  ccsDirectory?: string;
  driver: AntigravitySwitchDriver;
  collectQuota(request: SavedQuotaRequest): Promise<SavedQuotaSnapshot>;
  now?: () => number;
  /**
   * Why the installed runtime service is not running, when a read-only check
   * found a cause. Absent in fixtures; production passes runtime-health.ts.
   */
  readRuntimeServiceProblem?: () => Promise<AntigravityRuntimeServiceProblem | null>;
}

export interface AntigravityRuntime extends AntigravityApiDependencies {
  hasProfiles(): boolean;
  cachedAccounts(): AntigravityDashboardAccount[];
  readSelectedProfileId(): Promise<string | null>;
  /** Finish or undo a switch an older version or a managed update left unfinished (CLI `recover`). */
  recover(): Promise<AntigravityRecoveryResult>;
}

function copyAccount(account: AntigravityDashboardAccount): AntigravityDashboardAccount {
  return {
    ...account,
    ...(account.antigravityPlan
      ? {
          antigravityPlan: cloneAntigravityPlan(account.antigravityPlan),
        }
      : {}),
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

function unavailableQuota(request: SavedQuotaRequest): SavedQuotaSnapshot {
  return {
    profileId: request.profileId,
    identityKey: request.identityKey,
    credentialRevision: request.credentialRevision,
    email: request.email,
    plan: request.identity.plan,
    status: 'unavailable',
    fetchedAt: null,
    sampledAt: null,
    windows: [],
    source: 'native-consumer',
    identityVerified: false,
    identityValidation: 'unavailable',
  };
}

/** Explicit construction only. Importing this module does not touch files or start native programs. */
export function createAntigravityRuntime(deps: AntigravityRuntimeDependencies): AntigravityRuntime {
  if (deps.driver.hostId !== 'ubuntu')
    throw new AntigravityError('Antigravity switching targets the Ubuntu CLI.');
  const switcher = new AntigravitySwitchService({
    registry: deps.registry,
    driver: deps.driver,
    now: deps.now,
  });
  const mapProfiles = (
    profiles: ReturnType<AntigravityProfileRegistry['listProfiles']>
  ): AntigravityUsageProfile[] => {
    return profiles.map((profile) => {
      const host = profile.hosts.find((candidate) => candidate.hostId === 'ubuntu');
      try {
        const saved = deps.registry.readCredential(profile.id, 'ubuntu');
        if (
          identityKey(saved.identity) !== profile.identityKey ||
          saved.identity.email.toLowerCase() !== profile.email.toLowerCase()
        )
          throw new AntigravityError('Saved account identity changed.');
        return {
          id: profile.id,
          email: profile.email,
          plan: profile.plan,
          identityKey: profile.identityKey,
          credentialRevision: saved.credentialRevision,
          identityVerified: true,
          available: host?.available === true,
          selected: host?.selected === true,
          runtimeVerified: host?.active === true && host.verification === 'runtime',
          verifiedAt: host?.verifiedAt ?? saved.identity.verifiedAt,
        };
      } catch {
        return {
          id: profile.id,
          email: profile.email,
          plan: profile.plan,
          identityKey: profile.identityKey,
          credentialRevision: 'unavailable',
          identityVerified: false,
          available: false,
          selected: false,
          runtimeVerified: false,
          verifiedAt: null,
        };
      }
    });
  };
  const readProfiles = async (): Promise<AntigravityUsageProfile[]> =>
    mapProfiles(await switcher.readInventory());

  const collect = async (profileId: string): Promise<SavedQuotaSnapshot> => {
    const profile = deps.registry.listProfiles().find((candidate) => candidate.id === profileId);
    if (!profile) throw new AntigravityError('Saved Antigravity profile is unavailable.');
    const saved = deps.registry.readCredential(profileId, 'ubuntu');
    if (identityKey(saved.identity) !== profile.identityKey)
      throw new AntigravityError('Saved account identity changed.');
    const request: SavedQuotaRequest = {
      profileId,
      email: profile.email,
      identityKey: profile.identityKey,
      credentialRevision: saved.credentialRevision,
      credential: saved.credential,
      identity: saved.identity,
    };
    const identity = { kind: 'antigravity-subject' as const, key: profile.identityKey };
    if (deps.ccsDirectory) {
      const now = (deps.now ?? Date.now)();
      const until = readIdentityUsageCooldown(deps.ccsDirectory, 'antigravity', identity, now);
      if (until > now)
        return {
          ...unavailableQuota(request),
          status: 'rate_limited',
          retryAfterSeconds: Math.ceil((until - now) / 1000),
        };
    }
    let snapshot: SavedQuotaSnapshot;
    try {
      snapshot = await deps.collectQuota(request);
    } catch {
      return unavailableQuota(request);
    }
    const matchesRequest =
      snapshot.profileId === profileId &&
      snapshot.identityKey === request.identityKey &&
      snapshot.credentialRevision === request.credentialRevision &&
      snapshot.source === 'native-consumer' &&
      snapshot.email?.toLowerCase() === request.email.toLowerCase();
    // A provider throttled this immutable request even if its local token was
    // replaced while it was in flight. Quota publication still rechecks below.
    // A fresh snapshot may also report a throttle that hit only its optional
    // per-model Gemini read: the summary windows are published, and the same
    // identity-bound hold stops the next collections from retrying early.
    const freshThrottled =
      snapshot.status === 'fresh' &&
      Number.isInteger(snapshot.retryAfterSeconds) &&
      (snapshot.retryAfterSeconds as number) > 0;
    if (
      matchesRequest &&
      (snapshot.status === 'rate_limited' || freshThrottled) &&
      snapshot.identityValidation !== 'mismatch' &&
      snapshot.identityValidation !== 'needs_sign_in' &&
      deps.ccsDirectory
    ) {
      const now = (deps.now ?? Date.now)();
      const retry = snapshot.retryAfterSeconds;
      const seconds = Math.max(
        60,
        Math.min(86400, typeof retry === 'number' && Number.isFinite(retry) ? retry : 60)
      );
      holdIdentityUsageCooldown(
        deps.ccsDirectory,
        'antigravity',
        identity,
        now + seconds * 1000,
        now
      );
    }
    // The response must still refer to the original immutable registered bundle.
    try {
      const current = deps.registry.readCredential(profileId, 'ubuntu');
      if (
        current.credentialRevision !== request.credentialRevision ||
        identityKey(current.identity) !== request.identityKey ||
        !matchesRequest
      ) {
        return {
          ...unavailableQuota(request),
          status: 'needs_sign_in',
          identityValidation: 'mismatch',
        };
      }
    } catch {
      return {
        ...unavailableQuota(request),
        status: 'needs_sign_in',
        identityValidation: 'mismatch',
      };
    }
    if (
      snapshot.identityValidation === 'mismatch' ||
      snapshot.identityValidation === 'needs_sign_in'
    )
      return {
        ...unavailableQuota(request),
        status: 'needs_sign_in',
        identityValidation: snapshot.identityValidation,
      };
    if (
      (snapshot.status === 'fresh' || snapshot.status === 'ok') &&
      (snapshot.identityVerified !== true || snapshot.identityValidation !== 'verified')
    )
      return unavailableQuota(request);
    return snapshot;
  };

  const usage = new AntigravityUsageService({ readProfiles, collectQuota: collect, now: deps.now });
  return {
    hasProfiles: () => deps.registry.listProfiles().length > 0,
    getInventory: async () => {
      const inventory = await usage.getInventory();
      const serviceProblem = publicRuntimeServiceProblem(
        await deps.readRuntimeServiceProblem?.().catch(() => null)
      );
      return {
        ...inventory,
        ...(serviceProblem ? { runtimeServiceProblem: serviceProblem } : {}),
      };
    },
    getAccounts: async (options) => (await usage.getAccounts(options)).map(copyAccount),
    cachedAccounts: () =>
      usage.cachedAccounts(mapProfiles(deps.registry.listProfiles())).map(copyAccount),
    readSelectedProfileId: async () =>
      (await readProfiles()).find((profile) => profile.selected)?.id ?? null,
    recover: () => switcher.recover(),
    invalidateUsage: () => usage.invalidate(),
  };
}

/** Persistent construction is opt-in and happens only after a verified driver is supplied. */
export function createPersistentAntigravityRuntime(
  ccsDirectory: string,
  deps: Omit<AntigravityRuntimeDependencies, 'registry'>
): AntigravityRuntime {
  if (
    process.platform !== 'linux' ||
    !path.isAbsolute(ccsDirectory) ||
    deps.driver.hostId !== 'ubuntu'
  )
    throw new AntigravityError('Antigravity switching is managed on Ubuntu.');
  return createAntigravityRuntime({
    ...deps,
    ccsDirectory,
    registry: new AntigravityProfileRegistry(ccsDirectory),
  });
}
