import {
  AntigravityProfileRegistry,
  identityKey as registeredIdentityKey,
} from '../../antigravity/registry';
import { IDENTITY_UNBOUND_MESSAGE } from '../../antigravity/usage-normalization';
import type { DashboardAccount } from './account-dashboard-types';
import { HEX_DIGEST, usageEmail, type UsageFailoverTarget } from './usage-failover-contract';
import { getUsageFailoverService, type UsageFailoverService } from './usage-failover-service';
import { localUsageHost } from './usage-failover-transport';

/** Private lookup data; identity and credential hashes never enter a dashboard row. */
export interface AntigravityUsageBinding {
  email: string;
  identityKey: string;
  credentialRevision: string;
}

export interface AntigravityUsageFailoverDeps {
  readBinding?: (profileId: string, scope: string) => AntigravityUsageBinding | null;
  failover?: Pick<UsageFailoverService, 'recover' | 'peek'>;
}

function readBinding(profileId: string, scope: string): AntigravityUsageBinding | null {
  try {
    const registry = AntigravityProfileRegistry.openExisting(scope);
    const profile = registry?.listProfiles().find((row) => row.id === profileId);
    if (!registry || !profile) return null;
    const saved = registry.readCredential(profileId, 'ubuntu');
    if (
      registeredIdentityKey(saved.identity) !== profile.identityKey ||
      saved.identity.email.toLowerCase() !== profile.email.toLowerCase()
    )
      return null;
    return {
      email: profile.email,
      identityKey: profile.identityKey,
      credentialRevision: saved.credentialRevision,
    };
  } catch {
    return null;
  }
}

function unavailable(account: DashboardAccount, mismatch = false): DashboardAccount {
  const result: DashboardAccount = {
    ...account,
    status: mismatch ? 'error' : 'unavailable',
    windows: [],
    fetchedAt: null,
    sampledAt: null,
    message: mismatch
      ? IDENTITY_UNBOUND_MESSAGE
      : 'The saved Antigravity account could not be verified.',
    capabilities: { ...account.capabilities, antigravityCanActivate: false },
  };
  if (mismatch) result.statusReason = 'identity_unbound';
  else delete result.statusReason;
  return result;
}

function bindingFor(
  account: DashboardAccount,
  scope: string,
  deps: AntigravityUsageFailoverDeps
): { target: UsageFailoverTarget; binding: AntigravityUsageBinding } | null {
  const profileId = account.capabilities.antigravityProfileId;
  if (
    account.provider !== 'antigravity' ||
    typeof profileId !== 'string' ||
    !/^[a-z][a-z0-9_-]{0,47}$/.test(profileId) ||
    account.id !== `antigravity:profile:${profileId}`
  )
    return null;
  let binding: AntigravityUsageBinding | null;
  try {
    binding = (deps.readBinding ?? readBinding)(profileId, scope);
  } catch {
    return null;
  }
  if (
    !binding ||
    !HEX_DIGEST.test(binding.identityKey) ||
    !HEX_DIGEST.test(binding.credentialRevision) ||
    !usageEmail(binding.email) ||
    binding.email.toLowerCase() !== account.email?.toLowerCase()
  )
    return null;
  return {
    binding,
    target: {
      accountId: account.id,
      provider: 'antigravity',
      ccsDir: scope,
      binding: JSON.stringify([profileId, binding.identityKey, binding.credentialRevision]),
      primarySources: [{ ...localUsageHost(), selector: `antigravity-profile:${profileId}` }],
      expectedEmail: binding.email,
      expectedIdentity: { kind: 'antigravity-subject', key: binding.identityKey },
    },
  };
}

function project(account: DashboardAccount, reading: DashboardAccount): DashboardAccount {
  if (reading === account) return account;
  if (
    reading.id !== account.id ||
    reading.provider !== 'antigravity' ||
    reading.email?.toLowerCase() !== account.email?.toLowerCase()
  )
    return unavailable(account, true);
  const result: DashboardAccount = {
    ...account,
    plan: reading.plan,
    status: reading.status,
    message: reading.message,
    source: reading.source,
    fetchedAt: reading.fetchedAt,
    sampledAt: reading.sampledAt,
    windows: structuredClone(reading.windows),
    // Usage may come from any host; activation still addresses the registered
    // Ubuntu runtime. A display fallback cannot supply a runtime identity proof.
    platform: 'ubuntu',
    capabilities: {
      ...account.capabilities,
      antigravityCanActivate:
        account.capabilities.antigravityCanActivate === true &&
        reading.capabilities.antigravityCanActivate === true,
    },
  };
  delete result.host;
  delete result.antigravityPlan;
  if (reading.antigravityPlan) result.antigravityPlan = structuredClone(reading.antigravityPlan);
  return result;
}

/** Apply after the native public projection. Never used by the switching monitor. */
export async function recoverAntigravityUsage(
  accounts: DashboardAccount[],
  scope: string,
  deps: AntigravityUsageFailoverDeps = {}
): Promise<DashboardAccount[]> {
  return Promise.all(
    accounts.map(async (account) => {
      if (account.provider !== 'antigravity' || account.statusReason === 'identity_unbound')
        return account;
      const before = bindingFor(account, scope, deps);
      if (!before) return unavailable(account);
      const service = deps.failover ?? getUsageFailoverService();
      const reading = await service.recover(before.target, account).catch(() => account);
      // Removing or re-importing a profile can race a helper. Never publish a
      // sample for the previous immutable saved credential after that change.
      const after = bindingFor(account, scope, deps);
      if (!after || before.target.binding !== after.target.binding) return unavailable(account);
      return project(account, reading);
    })
  );
}

/** Synchronous cache projection validates the current saved credential on every response. */
export function peekAntigravityUsage(
  account: DashboardAccount,
  scope: string,
  deps: AntigravityUsageFailoverDeps = {}
): DashboardAccount {
  if (account.provider !== 'antigravity' || account.statusReason === 'identity_unbound')
    return account;
  const current = bindingFor(account, scope, deps);
  if (!current) return unavailable(account);
  try {
    return project(
      account,
      (deps.failover ?? getUsageFailoverService()).peek(current.target, account)
    );
  } catch {
    return account;
  }
}
