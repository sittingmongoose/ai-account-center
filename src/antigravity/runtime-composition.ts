import { AntigravityError } from './errors';
import path from 'path';
import { AntigravityProfileRegistry, identityKey } from './registry';
import { AntigravitySwitchService } from './switch-service';
import type {
  ActivateRequest,
  AntigravitySwitchDriver,
  NativeCredential,
  VerifiedIdentity,
} from './types';
import { AntigravityUsageService } from './usage-service';
import type {
  AntigravityApiDependencies,
  AntigravityDashboardAccount,
  AntigravityUsageProfile,
  AntigravityUsageSample,
} from './usage-contract';
import { DEFAULT_ANTIGRAVITY_AUTO_SWITCH_SETTINGS } from './auto-switch/settings';
import {
  AntigravityAutoSwitchService,
  ANTIGRAVITY_AUTO_SWITCH_MESSAGES,
} from './auto-switch/monitor';
import { AntigravityAutoSwitchFileStore } from './auto-switch/store';
import type {
  AntigravityAutoHostCensus,
  AntigravityAutoObservation,
  AntigravityAutoQuotaPool,
  AntigravityAutoQuotaSnapshot,
  AntigravityAutoSwitchStatus,
  AntigravityAutoSwitchStore,
} from './auto-switch/types';

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
  pools: AntigravityAutoQuotaPool[];
}

export interface AntigravityRuntimeDependencies {
  registry: AntigravityProfileRegistry;
  driver: AntigravitySwitchDriver;
  store: AntigravityAutoSwitchStore;
  collectQuota(request: SavedQuotaRequest): Promise<SavedQuotaSnapshot>;
  /** Must report current native process ownership/completeness and external transaction locks. */
  observeHost(): Promise<AntigravityAutoHostCensus>;
  now?: () => number;
  setTimer?: (callback: () => void, milliseconds: number) => unknown;
  clearTimer?: (timer: unknown) => void;
}

export interface AntigravityRuntime extends AntigravityApiDependencies {
  hasProfiles(): boolean;
  cachedAccounts(): AntigravityDashboardAccount[];
  readSelectedProfileId(): Promise<string | null>;
  getAutoSwitchStatus(): AntigravityAutoSwitchStatus;
  start(): void;
  stop(): void;
}

function copyAccount(account: AntigravityDashboardAccount): AntigravityDashboardAccount {
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
    pools: [],
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
  let manualActivations = 0;
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
    let snapshot: SavedQuotaSnapshot;
    try {
      snapshot = await deps.collectQuota(request);
    } catch {
      return unavailableQuota(request);
    }
    // The response must still refer to the original immutable registered bundle.
    try {
      const current = deps.registry.readCredential(profileId, 'ubuntu');
      if (
        current.credentialRevision !== request.credentialRevision ||
        identityKey(current.identity) !== request.identityKey ||
        snapshot.profileId !== profileId ||
        snapshot.identityKey !== request.identityKey ||
        snapshot.credentialRevision !== request.credentialRevision ||
        snapshot.source !== 'native-consumer' ||
        snapshot.email?.toLowerCase() !== request.email.toLowerCase()
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
  const capability = async (): Promise<boolean> => {
    try {
      return (await deps.driver.canProveRuntimeIdentity()) === true;
    } catch {
      return false;
    }
  };
  const observe = async (
    quiescedHost?: AntigravityAutoHostCensus
  ): Promise<AntigravityAutoObservation> => {
    const profiles = await readProfiles();
    const [snapshots, host, supported] = await Promise.all([
      Promise.all(
        profiles
          .filter((profile) => profile.identityVerified && profile.available)
          .map((profile) => collect(profile.id))
      ),
      quiescedHost ? Promise.resolve(quiescedHost) : deps.observeHost(),
      quiescedHost ? Promise.resolve(quiescedHost.available === true) : capability(),
    ]);
    const census: AntigravityAutoHostCensus = {
      hostId: 'ubuntu',
      available: supported && host.hostId === 'ubuntu' && host.available === true,
      complete:
        host.complete === true &&
        typeof host.busy === 'boolean' &&
        typeof host.manualActivationInProgress === 'boolean',
      busy: host.busy !== false,
      manualActivationInProgress:
        manualActivations > 0 || host.manualActivationInProgress !== false,
      sampledAt: host.sampledAt,
    };
    return {
      profiles: profiles.map((profile) => ({
        id: profile.id,
        hostId: 'ubuntu',
        identityKey: profile.identityKey,
        credentialRevision: profile.credentialRevision,
        authValid: profile.identityVerified && profile.available,
        nativeConsumerCompatible: profile.identityVerified && profile.available,
        isActive: profile.selected,
      })),
      quotas: snapshots.map(
        (snapshot): AntigravityAutoQuotaSnapshot => ({
          profileId: snapshot.profileId,
          hostId: 'ubuntu',
          identityKey: snapshot.identityKey,
          credentialRevision: snapshot.credentialRevision,
          source: 'native-consumer',
          status:
            snapshot.status === 'fresh'
              ? 'fresh'
              : snapshot.status === 'cached' || snapshot.status === 'rate_limited'
                ? snapshot.status
                : snapshot.status === 'error'
                  ? 'error'
                  : 'unavailable',
          sampledAt: snapshot.sampledAt ?? '',
          pools:
            snapshot.identityVerified === true &&
            snapshot.identityValidation === 'verified' &&
            Array.isArray(snapshot.pools)
              ? snapshot.pools
              : [],
        })
      ),
      hosts: [census],
    };
  };
  const monitor = new AntigravityAutoSwitchService({
    store: deps.store,
    observe,
    now: deps.now,
    setTimer: deps.setTimer,
    clearTimer: deps.clearTimer,
    activate: (request) =>
      switcher.activate({
        ...request,
        revalidateAutomatic: (context) =>
          request.revalidateAutomatic({
            hostId: context.hostId,
            currentIdentityKey: context.currentIdentityKey,
            targetIdentityKey: context.targetIdentityKey,
            phase: context.phase,
            quiescedHost: context.quiescedHost,
          }),
      }),
  });

  const activate = async (request: ActivateRequest) => {
    if (request.mode !== 'manual' || request.hostId !== 'ubuntu')
      throw new AntigravityError('Invalid manual activation request.');
    manualActivations += 1;
    try {
      return await switcher.activate(request);
    } finally {
      manualActivations -= 1;
    }
  };
  const recover = async () => {
    manualActivations += 1;
    try {
      return await switcher.recover();
    } finally {
      manualActivations -= 1;
    }
  };
  return {
    hasProfiles: () => deps.registry.listProfiles().length > 0,
    getInventory: async () => ({
      ...(await usage.getInventory()),
      activationSupported: await capability(),
    }),
    getAccounts: async (options) => {
      const accounts = await usage.getAccounts(options);
      const supported = await capability();
      const result = accounts.map((account) => ({
        ...account,
        capabilities: {
          ...account.capabilities,
          antigravityCanActivate:
            supported && account.capabilities.antigravityHostIds.includes('ubuntu'),
        },
      }));
      return result.map(copyAccount);
    },
    cachedAccounts: () =>
      usage.cachedAccounts(mapProfiles(deps.registry.listProfiles())).map((account) => ({
        ...copyAccount(account),
        capabilities: { ...account.capabilities, antigravityCanActivate: false },
      })),
    readSelectedProfileId: async () =>
      (await readProfiles()).find((profile) => profile.selected)?.id ?? null,
    activate,
    recover,
    getAutoSwitchStatus: () => monitor.getStatus(),
    updateAutoSwitchSettings: (patch) => monitor.updateSettings(patch),
    invalidateUsage: () => usage.invalidate(),
    start: () => monitor.start(),
    stop: () => monitor.stop(),
  };
}

/** Persistent construction is opt-in and happens only after a verified driver is supplied. */
export function createPersistentAntigravityRuntime(
  ccsDirectory: string,
  deps: Omit<AntigravityRuntimeDependencies, 'registry' | 'store'>
): AntigravityRuntime {
  if (
    process.platform !== 'linux' ||
    !path.isAbsolute(ccsDirectory) ||
    deps.driver.hostId !== 'ubuntu'
  )
    throw new AntigravityError('Antigravity switching is managed on Ubuntu.');
  return createAntigravityRuntime({
    ...deps,
    registry: new AntigravityProfileRegistry(ccsDirectory),
    store: new AntigravityAutoSwitchFileStore(path.join(ccsDirectory, 'antigravity-switching')),
  });
}

export function disabledAntigravityAutoSwitchStatus(): AntigravityAutoSwitchStatus {
  return {
    ...DEFAULT_ANTIGRAVITY_AUTO_SWITCH_SETTINGS,
    selectedHostIds: ['ubuntu'],
    outcome: 'disabled',
    message: ANTIGRAVITY_AUTO_SWITCH_MESSAGES.disabled,
    activationInProgress: false,
  };
}
