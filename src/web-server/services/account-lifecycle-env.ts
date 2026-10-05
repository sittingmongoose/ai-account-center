import { getCcsDir } from '../../utils/config-manager';
import { broadcastDashboardEvent } from '../dashboard-events';
import { getAccountConfirmations, type AccountConfirmationStore } from './account-confirmations';
import {
  getAccountDashboard,
  invalidateAccountDashboard,
  replaceAccountDashboardRow,
} from './account-dashboard-service';
import type {
  AccountDashboard,
  DashboardAccount,
  DashboardProvider,
} from './account-dashboard-types';
import { keyStoreFor, type KeyStoreBackend, type KeyStoreLocation } from './account-key-store';
import {
  readAdditionalEntries,
  resolveAccount,
  type ResolvedAccount,
} from './account-lifecycle-accounts';
import {
  auditLifecycle,
  getAntigravityLifecycle,
  getClaudeLifecycle,
  getCodexLifecycle,
  getMuseLifecycle,
  getSignInJobRunner,
  lifecycleProviderFacts,
  setAccountsChangedHandler,
} from './account-lifecycle-runtime';
import { probeAdditionalSource, refreshAdditionalAccount } from './additional-account-service';
import type { AdditionalUsageSource } from './additional-usage-transport';
import type { AntigravityAccountLifecycle } from '../../antigravity/account-lifecycle';
import type { ClaudeAccountLifecycle } from './claude-account-lifecycle';
import { runClaudeHostOverSsh } from './claude-host-transport';
import type { CodexAccountLifecycle } from './codex-account-lifecycle';
import type { MuseAccountLifecycle } from './muse-account-lifecycle';
import { providerSignInState, type ProviderRegistryFacts } from './dashboard-provider-registry';
import type { SignInFlowSpec, SignInJobRunner } from './signin-jobs';

/**
 * Everything the lifecycle actions touch, injectable for tests: stores, the
 * job runner, the Codex and Claude flows, the collector probe, the key store,
 * the dashboard cache and the audit log.
 */
export interface LifecycleEnv {
  ccsDir: () => string;
  runner: () => SignInJobRunner;
  codex: () => CodexAccountLifecycle;
  claude: () => ClaudeAccountLifecycle;
  /** Antigravity saved profiles; without it Antigravity actions answer not_implemented. */
  antigravity?: () => AntigravityAccountLifecycle;
  /**
   * Builds the supervised Antigravity sign-in job (the driver flow). Injectable
   * so route tests need no bubblewrap; the default is the real driver flow.
   */
  antigravityJobFlow?: (profileName: string, mode: 'add' | 'signin-again') => SignInFlowSpec;
  /** Muse Sign in again; without it (or while its flag is off) it answers not_implemented. */
  muse?: () => MuseAccountLifecycle;
  confirmations: () => AccountConfirmationStore;
  providerFacts: (context: { secureTransport?: boolean }) => ProviderRegistryFacts;
  getDashboard: (context: { secureTransport?: boolean }) => Promise<AccountDashboard>;
  probe: (source: AdditionalUsageSource) => Promise<DashboardAccount>;
  refreshAdditional: (id: string) => Promise<DashboardAccount | null>;
  replaceRow: (account: DashboardAccount) => void;
  keyStore: (location: KeyStoreLocation) => KeyStoreBackend | null;
  /** Open Cursor in the Mac console session over the account's ssh alias. */
  openCursor: (sshHost: string) => Promise<void>;
  /** After any saved change: drop the dashboard cache and hint the /ws clients. */
  onChanged: () => void;
  audit: (event: string, data: Record<string, unknown>) => void;
  now: () => number;
}

/** Per request: the transport and the session the confirmation tokens bind to. */
export interface LifecycleContext {
  secure: boolean;
  sessionKey: string;
}

export interface LifecycleResult {
  status: number;
  body: unknown;
  headers?: Record<string, string>;
}

const OPEN_CURSOR_COMMAND = [
  "console_user=$(/usr/bin/stat -f '%Su' /dev/console)",
  'test "$console_user" != root && test "$console_user" != loginwindow || exit 1',
  'console_uid=$(/usr/bin/id -u "$console_user") || exit 1',
  'current_uid=$(/usr/bin/id -u) || exit 1',
  'test "$console_uid" = "$current_uid" || exit 1',
  'exec /usr/bin/open -a Cursor',
].join('\n');

function accountsChanged(): void {
  invalidateAccountDashboard();
  broadcastDashboardEvent({ type: 'accounts-changed' });
}

// A finished sign-in job (Codex Add) changes the accounts after its route answered.
setAccountsChangedHandler(accountsChanged);

export function defaultLifecycleEnv(): LifecycleEnv {
  return {
    ccsDir: getCcsDir,
    runner: getSignInJobRunner,
    codex: getCodexLifecycle,
    claude: getClaudeLifecycle,
    antigravity: getAntigravityLifecycle,
    muse: getMuseLifecycle,
    confirmations: getAccountConfirmations,
    providerFacts: (context) => lifecycleProviderFacts(context),
    getDashboard: (context) => getAccountDashboard('mac', false, context),
    probe: (source) => probeAdditionalSource(source),
    refreshAdditional: refreshAdditionalAccount,
    replaceRow: replaceAccountDashboardRow,
    keyStore: (location) => keyStoreFor(location, { ccsDir: getCcsDir() }),
    openCursor: async (sshHost) => {
      await runClaudeHostOverSsh(sshHost, OPEN_CURSOR_COMMAND, '');
    },
    onChanged: accountsChanged,
    audit: auditLifecycle,
    now: Date.now,
  };
}

export function resolveIn(env: LifecycleEnv, id: string): Promise<ResolvedAccount> {
  return resolveAccount(id, {
    ccsDir: env.ccsDir(),
    codexHasProfile: (name) => {
      try {
        return env.codex().registry().hasProfile(name);
      } catch {
        return false;
      }
    },
    findClaude: (claudeId) =>
      env
        .claude()
        .findProfile(claudeId)
        .catch(() => null),
    ...(env.antigravity
      ? {
          antigravityHasProfile: (profileId: string) => {
            try {
              return env.antigravity?.().hasProfile(profileId) === true;
            } catch {
              return false;
            }
          },
        }
      : {}),
  });
}

/** Accounts of one provider in its own store (hidden ones included). */
export async function providerAccountCount(
  env: LifecycleEnv,
  provider: DashboardProvider
): Promise<number> {
  if (provider === 'codex') return env.codex().registry().listProfiles().length;
  if (provider === 'claude') return env.claude().count();
  if (provider === 'antigravity') return env.antigravity?.().profileCount() ?? 0;
  const { entries } = await readAdditionalEntries(env.ccsDir());
  return entries.filter((entry) => entry.provider === provider).length;
}

export function signInState(env: LifecycleEnv, provider: DashboardProvider, secure: boolean) {
  return providerSignInState(provider, env.providerFacts({ secureTransport: secure }));
}

/** One in-process queue per name, so two key writes for one provider never interleave. */
export { keyQueue, serialized } from './account-lifecycle-queue';
