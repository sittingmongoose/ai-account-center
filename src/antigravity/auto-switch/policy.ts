import type {
  AntigravityAutoObservation,
  AntigravityAutoProfile,
  AntigravityAutoSwitchOutcome,
  AntigravityAutoSwitchStoredState,
} from './types';

/** Avoid switching to another account that is itself almost at the threshold. */
export const MIN_TARGET_HEADROOM_PERCENT = 5;
export const MAX_HOST_CENSUS_AGE_MS = 10_000;

export interface AntigravityAutoSwitchDecision {
  outcome: AntigravityAutoSwitchOutcome;
  active?: AntigravityAutoProfile;
  target?: AntigravityAutoProfile;
  activeRemainingPercent?: number;
  targetRemainingPercent?: number;
}

function freshTimestamp(value: string, now: number, maximumAge: number): boolean {
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) && timestamp <= now && now - timestamp <= maximumAge;
}

export function matchingPoolRemaining(
  profile: AntigravityAutoProfile,
  observation: AntigravityAutoObservation,
  state: AntigravityAutoSwitchStoredState,
  now: number
): number | null {
  const matches = observation.quotas.filter(
    (quota) => quota.profileId === profile.id && quota.hostId === profile.hostId
  );
  if (matches.length !== 1) return null;
  const quota = matches[0];
  if (
    quota.status !== 'fresh' ||
    quota.source !== 'native-consumer' ||
    quota.identityKey !== profile.identityKey ||
    quota.credentialRevision !== profile.credentialRevision ||
    !freshTimestamp(quota.sampledAt, now, state.settings.maxQuotaAgeSeconds * 1000)
  )
    return null;
  const pools = quota.pools.filter((pool) => pool.id === state.settings.requestedPoolId);
  if (
    pools.length !== 1 ||
    pools[0].complete !== true ||
    !['reported-quota', 'provider-entitlement'].includes(pools[0].eligibility) ||
    !['provider-id', 'provider-bucket-membership'].includes(pools[0].idSource)
  )
    return null;
  const rateWindows = pools[0].windows.filter((window) => window.kind === 'rate_limit');
  if (
    !rateWindows.length ||
    new Set(rateWindows.map((window) => window.key)).size !== rateWindows.length
  )
    return null;
  const values: number[] = [];
  for (const window of rateWindows) {
    // A missing, expired or malformed reset cannot support a fresh switching decision.
    const reset = window.resetAt === null ? NaN : Date.parse(window.resetAt);
    if (
      (window.enabled !== undefined && typeof window.enabled !== 'boolean') ||
      (window.unlimited !== undefined && typeof window.unlimited !== 'boolean') ||
      window.enabled === false ||
      window.unlimited === true ||
      !Number.isFinite(reset) ||
      reset <= now ||
      typeof window.remainingPercent !== 'number' ||
      !Number.isFinite(window.remainingPercent) ||
      window.remainingPercent < 0 ||
      window.remainingPercent > 100
    )
      return null;
    values.push(window.remainingPercent);
  }
  return Math.min(...values);
}

/** Pure conservative policy. Extra credits and unrelated model pools never enter the ranking. */
export function decideAntigravityAutoSwitch(
  state: AntigravityAutoSwitchStoredState,
  observation: AntigravityAutoObservation,
  now: number
): AntigravityAutoSwitchDecision {
  if (!state.settings.enabled) return { outcome: 'disabled' };
  if (!state.settings.requestedPoolId) return { outcome: 'setup_required' };
  const hostProfiles = observation.profiles.filter((profile) => profile.hostId === 'ubuntu');
  if (hostProfiles.some((profile) => typeof profile.isActive !== 'boolean'))
    return { outcome: 'setup_required' };
  const eligible = hostProfiles.filter(
    (profile) =>
      profile.authValid === true &&
      profile.nativeConsumerCompatible === true &&
      typeof profile.identityKey === 'string' &&
      typeof profile.credentialRevision === 'string' &&
      profile.identityKey.length > 0 &&
      profile.credentialRevision.length > 0
  );
  if (
    eligible.length < 2 ||
    new Set(eligible.map((profile) => profile.id)).size !== eligible.length ||
    new Set(eligible.map((profile) => profile.identityKey)).size !== eligible.length
  )
    return { outcome: 'setup_required' };
  const activeProfiles = hostProfiles.filter((profile) => profile.isActive === true);
  if (activeProfiles.length !== 1 || !eligible.includes(activeProfiles[0]))
    return { outcome: 'setup_required' };
  const active = activeProfiles[0];
  if (state.lastSwitch) {
    const switchedAt = Date.parse(state.lastSwitch.at);
    // Future/corrupt state fails closed rather than shortening the persisted cooldown.
    if (!Number.isFinite(switchedAt) || switchedAt > now) return { outcome: 'error' };
    if (now - switchedAt < state.settings.cooldownSeconds * 1000) return { outcome: 'cooldown' };
  }
  for (const hostId of state.settings.selectedHostIds) {
    const hosts = observation.hosts.filter((host) => host.hostId === hostId);
    if (
      hosts.length !== 1 ||
      hosts[0].available !== true ||
      hosts[0].complete !== true ||
      hosts[0].busy !== false ||
      hosts[0].manualActivationInProgress !== false ||
      !freshTimestamp(hosts[0].sampledAt, now, MAX_HOST_CENSUS_AGE_MS)
    ) {
      return { outcome: 'waiting_idle' };
    }
  }
  const activeRemaining = matchingPoolRemaining(active, observation, state, now);
  if (activeRemaining === null) return { outcome: 'no_fresh_quota' };
  const thresholdRemaining = 100 - state.settings.thresholdUsedPercent;
  if (activeRemaining > thresholdRemaining)
    return {
      outcome: 'healthy',
      active,
      activeRemainingPercent: activeRemaining,
    };
  const minimumTargetRemaining = Math.max(
    thresholdRemaining + MIN_TARGET_HEADROOM_PERCENT,
    activeRemaining + MIN_TARGET_HEADROOM_PERCENT
  );
  const candidates = eligible
    .filter((profile) => !profile.isActive)
    .map((profile) => ({
      profile,
      remaining: matchingPoolRemaining(profile, observation, state, now),
    }))
    .filter(
      (item): item is typeof item & { remaining: number } =>
        item.remaining !== null &&
        item.remaining > thresholdRemaining &&
        item.remaining >= minimumTargetRemaining
    )
    .sort(
      (a, b) =>
        b.remaining - a.remaining ||
        (a.profile.id < b.profile.id ? -1 : a.profile.id > b.profile.id ? 1 : 0)
    );
  if (!candidates[0])
    return {
      outcome: 'no_candidate',
      active,
      activeRemainingPercent: activeRemaining,
    };
  return {
    outcome: 'scheduled',
    active,
    target: candidates[0].profile,
    activeRemainingPercent: activeRemaining,
    targetRemainingPercent: candidates[0].remaining,
  };
}

export function sameAntigravityDecision(
  earlier: AntigravityAutoSwitchDecision,
  latest: AntigravityAutoSwitchDecision
): boolean {
  return (
    latest.outcome === 'scheduled' &&
    !!earlier.active &&
    !!earlier.target &&
    !!latest.active &&
    !!latest.target &&
    earlier.active.id === latest.active.id &&
    earlier.target.id === latest.target.id &&
    earlier.active.hostId === latest.active.hostId &&
    earlier.target.hostId === latest.target.hostId &&
    earlier.active.identityKey === latest.active.identityKey &&
    earlier.target.identityKey === latest.target.identityKey &&
    earlier.active.credentialRevision === latest.active.credentialRevision &&
    earlier.target.credentialRevision === latest.target.credentialRevision
  );
}
