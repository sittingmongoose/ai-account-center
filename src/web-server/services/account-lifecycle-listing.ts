import type { DashboardProvider } from './account-dashboard-types';
import {
  accountView,
  readAdditionalEntries,
  type RegistryAccountView,
} from './account-lifecycle-accounts';
import {
  signInState,
  type LifecycleContext,
  type LifecycleEnv,
  type LifecycleResult,
} from './account-lifecycle-env';
import type { RegistryAccount } from './account-registry-v2';
import { entryView, jobBody, keyInfo } from './account-lifecycle-helpers';

/** GET /api/accounts/registry (CONTRACT-registry-lifecycle 6.1). */
export async function registryListing(
  env: LifecycleEnv,
  context: LifecycleContext
): Promise<LifecycleResult> {
  const dashboard = await env.getDashboard({ secureTransport: context.secure });
  const facts = env.providerFacts({ secureTransport: context.secure });
  const providers = Array.isArray(dashboard.providers) ? dashboard.providers : [];
  const canReplace = (provider: DashboardProvider) =>
    providers.find((entry) => entry.id === provider)?.capabilities.replaceKey === true;
  let entries: RegistryAccount[] = [];
  try {
    entries = (await readAdditionalEntries(env.ccsDir())).entries;
  } catch {
    entries = [];
  }
  const codex = env.codex();
  const claude = env.claude();
  const runner = env.runner();
  const accounts: RegistryAccountView[] = [];
  for (const row of Array.isArray(dashboard.accounts) ? dashboard.accounts : []) {
    const running = runner.runningForAccount(row.id) !== null;
    if (row.provider === 'codex' && row.capabilities.codexProfile) {
      const name = row.capabilities.codexProfile;
      const refusal = await codex.removeRefusal(name, running).catch(() => null);
      const available = signInState(env, 'codex', context.secure).unavailableReason === null;
      accounts.push(
        accountView(
          row,
          null,
          {
            signInAgain: available && refusal !== 'account_active',
            replaceKey: false,
            remove: true,
            open: [],
            recheck: false,
          },
          refusal
        )
      );
      continue;
    }
    if (row.provider === 'claude') {
      const id = row.capabilities.claudeProfileId ?? '';
      const profile = id ? await claude.findProfile(id).catch(() => null) : null;
      accounts.push(
        accountView(
          row,
          null,
          {
            signInAgain: true,
            replaceKey: false,
            remove: facts.remove?.claude !== false && profile !== null,
            open: [...row.capabilities.claudePlatforms],
            recheck: false,
          },
          profile?.isDefault ? 'account_protected' : null
        )
      );
      continue;
    }
    const profileId = row.capabilities.antigravityProfileId;
    if (row.provider === 'antigravity' && typeof profileId === 'string' && env.antigravity) {
      const agy = env.antigravity();
      // The page listing asks the registry and the row's own live flag; Remove
      // itself asks the live native login again at both calls.
      const refusal = await agy
        .removeRefusal(profileId, { signinRunning: running, fresh: false, liveHint: row.isActive })
        .catch(() => null);
      const reason = signInState(env, 'antigravity', context.secure).unavailableReason;
      accounts.push(
        accountView(
          row,
          null,
          {
            // Sign in again answers with the terminal command (preflight_failed).
            signInAgain:
              reason !== 'tool_missing' && reason !== 'not_implemented' && refusal === null,
            replaceKey: false,
            remove: facts.remove?.antigravity !== false,
            open: [],
            recheck: false,
          },
          refusal
        )
      );
      continue;
    }
    const entry = entries.find((candidate) => candidate.id === row.id);
    if (entry) {
      const view = entryView(entry, await keyInfo(env, entry), canReplace(entry.provider), row);
      if (entry.provider === 'muse') view.actions.signInAgain = false;
      view.removeRefusal = running ? 'signin_running' : null;
      accounts.push(view);
      continue;
    }
    // Console wallets: Remove deletes the stored opt-in source (no provider change).
    // Antigravity without its lifecycle has no action.
    const isWallet = row.id.startsWith('plan-opencode-go-console-');
    accounts.push(
      accountView(
        row,
        null,
        {
          signInAgain: isWallet,
          replaceKey: false,
          remove: isWallet && facts.remove?.['opencode-go'] !== false,
          open: [],
          recheck: false,
        },
        isWallet && running ? 'signin_running' : null
      )
    );
  }
  const trash = await claude.listTrash().catch(() => []);
  return {
    status: 200,
    body: {
      providers,
      accounts,
      jobs: runner.list().map((job) => jobBody(job, context)),
      trash: trash.map(({ trashId, provider, label: name, trashedAt, purgeAfter, state }) => ({
        trashId,
        provider,
        label: name,
        trashedAt,
        purgeAfter,
        state,
      })),
    },
  };
}

/** POST /api/accounts/add */
