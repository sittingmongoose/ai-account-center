import type {
  DashboardAccount,
  DashboardProvider,
  DashboardProviderEntry,
} from './account-dashboard-types';
import type { StoredKeyInfo } from './account-key-store';
import type { PublicTrashEntry } from './claude-account-lifecycle';
import type { SignInJob } from './signin-jobs';
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

/** One dashboard row of the listing and the owner facts its view needs (read once, in TS). */
export type RegistryRowFacts =
  | { kind: 'codex'; row: DashboardAccount; refusal: string | null; available: boolean }
  | {
      kind: 'claude';
      row: DashboardAccount;
      profile: { isDefault: boolean; email: string | null } | null;
      removeAllowed: boolean;
    }
  | {
      kind: 'antigravity';
      row: DashboardAccount;
      refusal: string | null;
      reason: string | null;
      removeAllowed: boolean;
    }
  | {
      kind: 'entry';
      row: DashboardAccount;
      entry: RegistryAccount;
      key: StoredKeyInfo | null;
      /** Muse only: whether its sign-in can start here. */
      museSignIn: boolean | null;
      running: boolean;
    }
  | { kind: 'other'; row: DashboardAccount; running: boolean; walletRemove: boolean };

export interface RegistryListingFacts {
  providers: DashboardProviderEntry[];
  rows: RegistryRowFacts[];
  jobs: SignInJob[];
  trash: Array<
    Pick<PublicTrashEntry, 'trashId' | 'provider' | 'label' | 'trashedAt' | 'purgeAfter' | 'state'>
  >;
}

/**
 * Every owner read of GET /api/accounts/registry, in the listing's order: the dashboard, the
 * registry entries, each row's removal/sign-in facts, then the trash and the sign-in jobs.
 */
export async function registryListingFacts(
  env: LifecycleEnv,
  context: LifecycleContext
): Promise<RegistryListingFacts> {
  const dashboard = await env.getDashboard({ secureTransport: context.secure });
  const facts = env.providerFacts({ secureTransport: context.secure });
  const providers = Array.isArray(dashboard.providers) ? dashboard.providers : [];
  let entries: RegistryAccount[] = [];
  try {
    entries = (await readAdditionalEntries(env.ccsDir())).entries;
  } catch {
    entries = [];
  }
  const codex = env.codex();
  const claude = env.claude();
  const runner = env.runner();
  const rows: RegistryRowFacts[] = [];
  for (const row of Array.isArray(dashboard.accounts) ? dashboard.accounts : []) {
    const running = runner.runningForAccount(row.id) !== null;
    if (row.provider === 'codex' && row.capabilities.codexProfile) {
      const name = row.capabilities.codexProfile;
      const refusal = await codex.removeRefusal(name, running).catch(() => 'unavailable' as const);
      const available = signInState(env, 'codex', context.secure).unavailableReason === null;
      rows.push({ kind: 'codex', row, refusal, available });
      continue;
    }
    if (row.provider === 'claude') {
      const id = row.capabilities.claudeProfileId ?? '';
      const profile = id ? await claude.findProfile(id).catch(() => null) : null;
      rows.push({
        kind: 'claude',
        row,
        profile: profile ? { isDefault: profile.isDefault, email: profile.email } : null,
        removeAllowed: facts.remove?.claude !== false,
      });
      continue;
    }
    const profileId = row.capabilities.antigravityProfileId;
    if (row.provider === 'antigravity' && typeof profileId === 'string' && env.antigravity) {
      const agy = env.antigravity();
      // The page listing asks the registry and the row's own live flag; Remove
      // itself asks the live native login again at both calls.
      const refusal = await agy
        .removeRefusal(profileId, { signinRunning: running, fresh: false, liveHint: row.isActive })
        .catch(() => 'unavailable' as const);
      const reason = signInState(env, 'antigravity', context.secure).unavailableReason;
      rows.push({
        kind: 'antigravity',
        row,
        refusal,
        reason,
        removeAllowed: facts.remove?.antigravity !== false,
      });
      continue;
    }
    const entry = entries.find((candidate) => candidate.id === row.id);
    if (entry) {
      const key = await keyInfo(env, entry);
      const museSignIn =
        entry.provider === 'muse'
          ? env.muse !== undefined &&
            signInState(env, 'muse', context.secure).unavailableReason === null
          : null;
      rows.push({ kind: 'entry', row, entry, key, museSignIn, running });
      continue;
    }
    // Console wallets: Remove deletes the stored opt-in source (no provider change).
    // Antigravity without its lifecycle has no action.
    const isWallet = row.id.startsWith('plan-opencode-go-console-');
    rows.push({
      kind: 'other',
      row,
      running,
      walletRemove: isWallet && facts.remove?.['opencode-go'] !== false,
    });
  }
  const trash = await claude.listTrash().catch(() => []);
  return {
    providers,
    rows,
    jobs: runner.list().map((job) => jobBody(job, context)),
    trash: trash.map(({ trashId, provider, label: name, trashedAt, purgeAfter, state }) => ({
      trashId,
      provider,
      label: name,
      trashedAt,
      purgeAfter,
      state,
    })),
  };
}

/**
 * The listing from its facts; reads nothing. This is the reference the M3 Rust composer
 * (`crates/aac-views/src/registry.rs`) must equal.
 */
export function composeRegistryListing(facts: RegistryListingFacts): {
  providers: DashboardProviderEntry[];
  accounts: RegistryAccountView[];
  jobs: SignInJob[];
  trash: RegistryListingFacts['trash'];
} {
  const canReplace = (provider: DashboardProvider) =>
    facts.providers.find((entry) => entry.id === provider)?.capabilities.replaceKey === true;
  const accounts = facts.rows.map((fact): RegistryAccountView => {
    const { row } = fact;
    switch (fact.kind) {
      case 'codex':
        return accountView(
          row,
          null,
          {
            // The live login signs in again in place (codex-account-lifecycle.ts); its
            // account_active refusal is Remove's only.
            signInAgain: fact.available && fact.refusal !== 'unavailable',
            replaceKey: false,
            remove: fact.refusal !== 'unavailable',
            open: [],
            recheck: false,
          },
          fact.refusal
        );
      case 'claude':
        return accountView(
          row,
          null,
          {
            signInAgain: true,
            replaceKey: false,
            remove: fact.removeAllowed && fact.profile !== null,
            open: [...row.capabilities.claudePlatforms],
            recheck: false,
          },
          fact.profile?.isDefault && !fact.profile.email ? 'account_protected' : null
        );
      case 'antigravity':
        return accountView(
          row,
          null,
          {
            // Sign in again answers with the terminal command (preflight_failed). The
            // live login signs in again in place (account-lifecycle.ts importSignIn);
            // its account_active refusal is Remove's only.
            signInAgain:
              fact.reason !== 'tool_missing' &&
              fact.reason !== 'not_implemented' &&
              (fact.refusal === null || fact.refusal === 'account_active'),
            replaceKey: false,
            remove: fact.removeAllowed && fact.refusal !== 'unavailable',
            open: [],
            recheck: false,
          },
          fact.refusal
        );
      case 'entry': {
        const view = entryView(fact.entry, fact.key, canReplace(fact.entry.provider), row);
        if (fact.entry.provider === 'muse') view.actions.signInAgain = fact.museSignIn === true;
        view.removeRefusal = fact.running ? 'signin_running' : null;
        return view;
      }
      case 'other': {
        const isWallet = row.id.startsWith('plan-opencode-go-console-');
        return accountView(
          row,
          null,
          {
            signInAgain: isWallet,
            replaceKey: false,
            remove: fact.walletRemove,
            open: [],
            recheck: false,
          },
          isWallet && fact.running ? 'signin_running' : null
        );
      }
    }
  });
  return { providers: facts.providers, accounts, jobs: facts.jobs, trash: facts.trash };
}

/** GET /api/accounts/registry (CONTRACT-registry-lifecycle 6.1). */
export async function registryListing(
  env: LifecycleEnv,
  context: LifecycleContext
): Promise<LifecycleResult> {
  return { status: 200, body: composeRegistryListing(await registryListingFacts(env, context)) };
}

/** POST /api/accounts/add */
