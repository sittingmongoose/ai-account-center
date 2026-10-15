import {
  getCodexAuthProfilesSummary,
  type CodexAuthProfilesSummary,
} from '../../codex-auth/codex-auth-dashboard-service';
import { getCcsDir } from '../../utils/config-manager';
import {
  getAntigravityAccounts,
  getCachedAntigravityAccounts,
  hasAntigravityProfiles,
  readSelectedAntigravityProfileId,
  getAntigravityAutoSwitchStatus,
} from '../../antigravity/runtime-service';
import { publicDashboardAccount } from '../../antigravity/usage-normalization';
import type { AntigravityDashboardAccount } from '../../antigravity/usage-contract';
import {
  getCachedCodexProfileQuotaRows,
  getCodexProfileQuotaRows,
} from '../usage/native-quota-collector';
import type { BarSummaryRow } from '../routes/bar-routes';
import {
  listClaudeDesktopProfiles,
  type ClaudeDesktopProfile,
} from './claude-desktop-profile-service';
import {
  getClaudeDesktopUsage,
  invalidateClaudeDesktopUsageCache,
  type ClaudeDesktopUsage,
} from './claude-desktop-usage-service';
import { getCodexAutoSwitchService } from './codex-auto-switch-service';
import { readCodexRenewalEntries, type CodexRenewalEntries } from './codex-renewal-entries';
import type { CodexProfileRenewalProfileStatus } from '../../codex-auth/codex-profile-renewal';
import {
  getAdditionalDashboardSnapshot,
  getConfiguredAdditionalAccounts,
  type AdditionalAccountsSnapshot,
} from './additional-account-service';
import {
  readAccountVisibility,
  VisibilityMemory,
  type AccountVisibilityRead,
} from './account-visibility';
import { buildDashboardProviders, type ProviderRegistryFacts } from './dashboard-provider-registry';
import { getSignInJobRunner, lifecycleProviderFacts } from './account-lifecycle-runtime';
import { readPendingProfiles, type PendingClaudeProfile } from './claude-account-stores';
import { getOpenCodeConsoleWalletAccounts } from './opencode-console-wallet-service';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import { getClaudeMacSignInState, type ClaudeSignInState } from './claude-desktop-signin-state';
import {
  ClaudeDesktopLiveUsageError,
  ClaudeDesktopSignInNeededError,
  getCachedClaudeDesktopLiveUsage,
  getLiveClaudeDesktopUsage,
  type ClaudeDesktopLiveUsage,
} from './claude-desktop-live-service';
import type {
  AccountDashboard,
  ClaudeDashboardPlatform,
  DashboardAccount,
  DashboardServerInfo,
} from './account-dashboard-types';
import {
  pendingClaudeAccount,
  withAccountState,
  withThresholdUsedPercent,
  withJobState,
  type AccountJobStates,
} from './account-dashboard-state';
import { getDashboardServerInfo } from './dashboard-server-info';
import { markPassedResets } from './account-window-reset';
import { getUsageFailoverService, type UsageFailoverService } from './usage-failover-service';
import { localUsageHost } from './usage-failover-transport';
import type { UsageFailoverTarget, UsagePrimarySource } from './usage-failover-contract';
import { recoverAntigravityUsage, peekAntigravityUsage } from './usage-failover-antigravity';
import { codexUsageTarget, recoverCodexUsage, peekCodexUsage } from './usage-failover-codex';
import { projectClaudeFailoverExtras } from './usage-failover-claude-extras';
import { isDefaultClaudeDataFolder } from './claude-account-records';

import {
  ADDITIONAL_PROVIDERS,
  codexAccount,
  claudeAccount,
  additionalFallback,
  additionalAccounts,
  applyClaudeLiveUsage,
  emailForComparison,
  withClaudeSignIn,
} from './account-dashboard-projection';

export type { AccountDashboard, DashboardAccount } from './account-dashboard-types';

export interface AccountDashboardDeps {
  getCodexSummary?: () => Promise<CodexAuthProfilesSummary>;
  getCodexRows?: (names: string[], refresh: boolean) => Promise<BarSummaryRow[]>;
  getCachedCodexRows?: (names: string[]) => BarSummaryRow[];
  /** Renewal state per saved Codex login (local files only); unreadable means no renewal notes. */
  getRenewalStatus?: () => Promise<{ profiles: CodexProfileRenewalProfileStatus[] }>;
  listClaudeProfiles?: () => Promise<ClaudeDesktopProfile[]>;
  getClaudeUsage?: (platform: ClaudeDashboardPlatform) => Promise<ClaudeDesktopUsage>;
  getLiveClaudeUsage?: (
    profileId: string,
    refresh: boolean
  ) => Promise<ClaudeDesktopLiveUsage | null>;
  getCachedLiveClaudeUsage?: (profileId: string) => Promise<ClaudeDesktopLiveUsage | null>;
  /** Mac profile sign-in marker (no secrets); null when unknown. */
  getClaudeMacSignIn?: (
    profile: ClaudeDesktopProfile,
    refresh: boolean
  ) => Promise<ClaudeSignInState | null>;
  /** Legacy fixture source: rows as the version 1 manifest returns them. */
  getAdditionalAccounts?: () => Promise<DashboardAccount[]>;
  /** Rows plus the store that listed them (version 1 manifest or registry v2). */
  getAdditionalSnapshot?: (refresh: boolean) => Promise<AdditionalAccountsSnapshot>;
  /** Placeholder rows for the configured accounts while collection is slow. */
  getConfiguredAdditional?: () => AdditionalAccountsSnapshot | null;
  readVisibility?: (scope: string) => Promise<AccountVisibilityRead>;
  /** Live facts behind providers[].signIn and capabilities. */
  providerFacts?: (context: AccountDashboardRequestContext) => ProviderRegistryFacts;
  /** Running sign-in jobs by account id (lifecycle.state signing_in or verifying). */
  accountJobStates?: () => AccountJobStates;
  /** Claude profiles created by Add that wait for their first sign-in. */
  listClaudePending?: () => Promise<PendingClaudeProfile[]>;
  getOptionalWalletAccounts?: (refresh: boolean) => Promise<DashboardAccount[]>;
  getAutoSwitchStatus?: () => Omit<AccountDashboard['codexAutoSwitch'], 'thresholdUsedPercent'>;
  hasAntigravityProfiles?: () => boolean;
  getAntigravityAccounts?: (refresh: boolean) => Promise<AntigravityDashboardAccount[]>;
  getCachedAntigravityAccounts?: () => AntigravityDashboardAccount[];
  readSelectedAntigravityProfileId?: () => Promise<string | null>;
  getAntigravityAutoSwitchStatus?: () => NonNullable<AccountDashboard['antigravityAutoSwitch']>;
  invalidateClaudeCache?: () => void;
  now?: () => number;
  responseBudgetMs?: number;
  scope?: () => string;
  refreshIntervalSeconds?: () => number;
  serverInfo?: () => DashboardServerInfo | undefined;
  /** Display-only usage failover; injected legacy collectors keep their offline fixture path. */
  failover?: Pick<UsageFailoverService, 'collect' | 'recover' | 'peek'> | null;
}

/** Per-request facts the cached collection does not know. */
export interface AccountDashboardRequestContext {
  /** isSecureTransport(req) (CONTRACT-auth-devices 2a); false until that check ships. */
  secureTransport?: boolean;
}

const REFRESH_DEBOUNCE_MS = 5_000;
const MAX_SCOPES = 16;
const VISIBILITY_BUDGET_FLOOR_MS = 250;
interface DashboardState {
  fetchedAt: number;
  refreshedAt: number;
  generation: number;
  pending: Promise<DashboardAccount[]> | null;
  codex: DashboardAccount[];
  claude: DashboardAccount[];
  additional: DashboardAccount[];
  antigravity: DashboardAccount[];
}

export class AccountDashboardService {
  private readonly states = new Map<string, DashboardState>();
  private readonly codexInventories = new Map<string, CodexAuthProfilesSummary>();
  /** A published reading keeps the exact auth snapshot taken before its collection. */
  private readonly codexRowTargets = new WeakMap<DashboardAccount, UsageFailoverTarget | null>();
  /** Last renewal entries per scope, for the cached Codex rows that cannot await a read. */
  private readonly codexRenewals = new Map<string, CodexRenewalEntries>();
  private readonly claudeInventories = new Map<string, ClaudeDesktopProfile[]>();
  private readonly claudeLiveSamples = new Map<string, ClaudeDesktopLiveUsage>();
  /** Last known "Sign-in needed" computers per Claude manifest entry, kept across collections. */
  private readonly claudeSignIn = new Map<string, Set<ClaudeDashboardPlatform>>();
  private readonly visibility = new VisibilityMemory(MAX_SCOPES);

  constructor(private readonly deps: AccountDashboardDeps = {}) {}

  private claudeTarget(scope: string, profile: ClaudeDesktopProfile): UsageFailoverTarget {
    const primarySources: UsagePrimarySource[] = [];
    const local = localUsageHost();
    for (const platform of ['windows'] as const) {
      const launcher = profile[platform];
      if (!launcher || (!launcher.sshHost && local.platform !== platform)) continue;
      const isDefault =
        launcher.isDefault === true || isDefaultClaudeDataFolder(launcher.profilePath);
      primarySources.push({
        host: platform,
        platform,
        sshHost: launcher.sshHost ?? null,
        selector: `claude-desktop-profile:${isDefault ? 'default' : (profile.id ?? 'default')}`,
        claudeProfile: {
          id: profile.id ?? 'default',
          email: profile.email,
          ...(launcher.profilePath ? { profilePath: launcher.profilePath } : {}),
          isDefault,
        },
      });
    }
    return {
      accountId: `claude:${profile.id ?? profile.email}`,
      provider: 'claude',
      ccsDir: scope,
      binding: JSON.stringify(profile),
      expectedEmail: profile.email,
      primarySources,
    };
  }

  private async collectClaudeFailover(
    scope: string,
    platform: ClaudeDashboardPlatform,
    profiles: ClaudeDesktopProfile[],
    refresh: boolean,
    publish: (accounts: DashboardAccount[]) => void
  ): Promise<DashboardAccount[]> {
    const failover = this.deps.failover ?? getUsageFailoverService();
    const targets = profiles.map((profile) => this.claudeTarget(scope, profile));
    const accounts = profiles.map((profile, index) =>
      failover.peek(targets[index], claudeAccount(profile, platform))
    );
    // A Mac-only Desktop profile has no supported native quota identity yet;
    // retain its existing dated history until a verified source is available.
    const macHistory = profiles.some((profile) => !profile.windows)
      ? (this.deps.getClaudeUsage ?? getClaudeDesktopUsage)('mac').catch(() => null)
      : Promise.resolve(null);
    publish([...accounts]);
    await Promise.all(
      profiles.map(async (profile, index) => {
        const history = profile.windows ? null : await macHistory;
        const row = profile.windows
          ? await failover.collect(targets[index], claudeAccount(profile, platform))
          : claudeAccount(
              profile,
              platform,
              history?.profiles.find((sample) =>
                profile.id ? sample.id === profile.id : sample.email === profile.email
              )
            );
        const macState = profile.mac
          ? await (
              this.deps.getClaudeMacSignIn ??
              ((candidate, force) => getClaudeMacSignInState(candidate, { refresh: force }))
            )(profile, refresh).catch(() => null)
          : null;
        const signIn = new Set(row.signInNeeded ?? []);
        if (macState === 'signed-out') signIn.add('mac');
        accounts[index] = withClaudeSignIn(row, signIn);
        publish([...accounts]);
      })
    );
    return accounts;
  }

  private async bounded<T>(
    pending: Promise<T>,
    fallback: () => T,
    budgetMs = this.deps.responseBudgetMs ?? 2500
  ): Promise<T> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<T>((resolve) => {
      timer = setTimeout(() => resolve(fallback()), budgetMs);
    });
    try {
      return await Promise.race([pending.catch(fallback), timeout]);
    } finally {
      clearTimeout(timer);
    }
  }

  private async collectCodex(scope: string, refresh: boolean): Promise<DashboardAccount[]> {
    const summary = await (this.deps.getCodexSummary ?? getCodexAuthProfilesSummary)();
    this.codexInventories.set(scope, summary);
    const names = summary.profiles.map((profile) => profile.name);
    const recoverUsage =
      this.deps.failover !== null &&
      (this.deps.failover || (!this.deps.getCodexSummary && !this.deps.getCodexRows));
    const targets = recoverUsage
      ? new Map(summary.profiles.map((profile) => [profile.name, codexUsageTarget(scope, profile)]))
      : null;
    // Kept as soon as read, so a slow quota fetch still lets the cached rows show notes.
    const renewals = readCodexRenewalEntries(this.deps.getRenewalStatus).then((entries) => {
      this.codexRenewals.set(scope, entries);
      return entries;
    });
    const [rows, entries] = await Promise.all([
      this.bounded(
        (
          this.deps.getCodexRows ??
          ((profiles, force) => getCodexProfileQuotaRows(profiles, {}, { force }))
        )(names, refresh),
        () => (this.deps.getCachedCodexRows ?? getCachedCodexProfileQuotaRows)(names)
      ),
      renewals,
    ]);
    return Promise.all(
      summary.profiles.map(async (profile) => {
        const account = codexAccount(
          profile,
          summary.activated,
          rows.find((row) => row.profile === profile.name),
          entries.get(profile.name)
        );
        if (!targets) return account;
        const target = targets.get(profile.name) ?? null;
        const recovered = profile.authValid
          ? await recoverCodexUsage(target, account, this.deps.failover ?? undefined)
          : account;
        this.codexRowTargets.set(recovered, target);
        return recovered;
      })
    );
  }

  private cachedCodex(scope: string): DashboardAccount[] {
    const summary = this.codexInventories.get(scope);
    if (!summary) return [];
    const recoverUsage =
      this.deps.failover !== null &&
      (this.deps.failover || (!this.deps.getCodexSummary && !this.deps.getCodexRows));
    const targets = recoverUsage
      ? new Map(summary.profiles.map((profile) => [profile.name, codexUsageTarget(scope, profile)]))
      : null;
    const rows = (this.deps.getCachedCodexRows ?? getCachedCodexProfileQuotaRows)(
      summary.profiles.map((profile) => profile.name)
    );
    const renewals = this.codexRenewals.get(scope);
    return summary.profiles.map((profile) => {
      const account = codexAccount(
        profile,
        summary.activated,
        rows.find((row) => row.profile === profile.name),
        renewals?.get(profile.name)
      );
      if (!targets) return account;
      const target = targets.get(profile.name) ?? null;
      const cached = profile.authValid
        ? peekCodexUsage(target, account, this.deps.failover ?? undefined)
        : account;
      this.codexRowTargets.set(cached, target);
      return cached;
    });
  }

  private async collectClaude(
    scope: string,
    platform: ClaudeDashboardPlatform,
    refresh: boolean,
    publishCached: (accounts: DashboardAccount[]) => void
  ): Promise<DashboardAccount[]> {
    const profiles = await (this.deps.listClaudeProfiles ?? listClaudeDesktopProfiles)();
    this.claudeInventories.set(scope, profiles);
    if (
      this.deps.failover !== null &&
      (this.deps.failover ||
        (!this.deps.listClaudeProfiles &&
          !this.deps.getClaudeUsage &&
          !this.deps.getLiveClaudeUsage))
    ) {
      return this.collectClaudeFailover(scope, platform, profiles, refresh, publishCached);
    }
    // Launch preference never partitions a verified account's quota. Bind retained
    // samples to the complete manifest entry so a changed source cannot inherit them.
    const sampleKeys = profiles.map((profile) => JSON.stringify([scope, profile]));
    const persisted = await Promise.all(
      profiles.map((profile) =>
        profile.id
          ? (
              this.deps.getCachedLiveClaudeUsage ??
              (this.deps.getLiveClaudeUsage ? async () => null : getCachedClaudeDesktopLiveUsage)
            )(profile.id).catch(() => null)
          : Promise.resolve(null)
      )
    );
    const signIn = sampleKeys.map(
      (key) => new Set<ClaudeDashboardPlatform>(this.claudeSignIn.get(key) ?? [])
    );
    const withSignIn = (rows: DashboardAccount[]): DashboardAccount[] =>
      rows.map((row, index) => withClaudeSignIn(row, signIn[index]));
    const markSignIn = (index: number, host: ClaudeDashboardPlatform, needed: boolean): boolean => {
      const set = signIn[index];
      if (set.has(host) === needed) return false;
      if (needed) set.add(host);
      else set.delete(host);
      this.claudeSignIn.delete(sampleKeys[index]);
      this.claudeSignIn.set(sampleKeys[index], new Set(set));
      while (this.claudeSignIn.size > MAX_SCOPES * 4) {
        const oldest = this.claudeSignIn.keys().next().value;
        if (oldest === undefined) break;
        this.claudeSignIn.delete(oldest);
      }
      return true;
    };
    const publish = (): void => publishCached(withSignIn(accounts));
    let accounts = profiles.map((profile, index) => {
      const account = claudeAccount(profile, platform);
      const previous = this.claudeLiveSamples.get(sampleKeys[index]);
      const disk = persisted[index];
      const retained =
        disk && (!previous || Date.parse(disk.fetchedAt) > Date.parse(previous.fetchedAt))
          ? disk
          : previous;
      const result = applyClaudeLiveUsage(account, retained ?? null);
      if (result !== account && retained) this.claudeLiveSamples.set(sampleKeys[index], retained);
      return result === account ? account : { ...result, status: 'cached' as const };
    });
    while (this.claudeLiveSamples.size > MAX_SCOPES * 4) {
      const oldest = this.claudeLiveSamples.keys().next().value;
      if (oldest === undefined) break;
      this.claudeLiveSamples.delete(oldest);
    }
    publish();
    const history = Promise.resolve()
      .then(() => (this.deps.getClaudeUsage ?? getClaudeDesktopUsage)(platform))
      .catch(() => null)
      .then((usage) => {
        accounts = profiles.map((profile, index) => {
          // Desktop history omits resets and balances. Its late completion must
          // not replace the independently verified live sample or its timestamp.
          if (accounts[index].source === 'Claude Desktop live quota on Windows') {
            return accounts[index];
          }
          return claudeAccount(
            profile,
            platform,
            usage?.profiles.find((row) =>
              profile.id ? row.id === profile.id : row.email === profile.email
            )
          );
        });
        publish();
      });
    const live = Promise.all(
      profiles.map(async (profile, index) => {
        if (!profile.id) return;
        let helperOutdated = false;
        let signInNeeded = false;
        const sample = await Promise.resolve()
          .then(() =>
            (
              this.deps.getLiveClaudeUsage ??
              ((id, force) => getLiveClaudeDesktopUsage(id, { refresh: force }))
            )(profile.id as string, refresh)
          )
          .catch((error: unknown) => {
            if (error instanceof ClaudeDesktopLiveUsageError && error.helperOutdated) {
              helperOutdated = true;
            }
            if (error instanceof ClaudeDesktopSignInNeededError) signInNeeded = true;
            return null;
          });
        if (signInNeeded) {
          if (markSignIn(index, 'windows', true)) publish();
          return;
        }
        if (sample && markSignIn(index, 'windows', false)) publish();
        if (!sample) {
          // A new ID the old installed collector cannot know: say so, instead
          // of a misleading sign-in prompt. Rows that already show live or
          // cached readings are left alone.
          const current = accounts[index];
          if (
            helperOutdated &&
            current.source !== 'Claude Desktop live quota on Windows' &&
            current.windows.length === 0
          ) {
            accounts = accounts.map((account, rowIndex) =>
              rowIndex === index
                ? {
                    ...account,
                    status: 'unavailable' as const,
                    message: 'Update the usage helper on Windows.',
                  }
                : account
            );
            publish();
          }
          return;
        }
        const previous = this.claudeLiveSamples.get(sampleKeys[index]);
        if (previous && Date.parse(previous.fetchedAt) > Date.parse(sample.fetchedAt)) return;
        const result = applyClaudeLiveUsage(accounts[index], sample);
        if (result === accounts[index]) return;
        this.claudeLiveSamples.delete(sampleKeys[index]);
        this.claudeLiveSamples.set(sampleKeys[index], sample);
        while (this.claudeLiveSamples.size > MAX_SCOPES * 4) {
          const oldest = this.claudeLiveSamples.keys().next().value;
          if (oldest === undefined) break;
          this.claudeLiveSamples.delete(oldest);
        }
        accounts = accounts.map((account, rowIndex) => (rowIndex === index ? result : account));
        publish();
      })
    );
    const macCheck =
      this.deps.getClaudeMacSignIn ??
      (this.deps.listClaudeProfiles
        ? async () => null
        : (profile: ClaudeDesktopProfile, force: boolean) =>
            getClaudeMacSignInState(profile, { refresh: force }));
    const mac = Promise.all(
      profiles.map(async (profile, index) => {
        if (!profile.id || !profile.mac) return;
        const state = await Promise.resolve()
          .then(() => macCheck(profile, refresh))
          .catch(() => null);
        if (state === null) return;
        if (markSignIn(index, 'mac', state === 'signed-out')) publish();
      })
    );
    await Promise.all([history, live, mac]);
    return withSignIn(accounts);
  }

  private additionalSnapshot(
    refresh: boolean,
    excludeAntigravity: boolean
  ): Promise<AdditionalAccountsSnapshot> {
    if (this.deps.getAdditionalSnapshot) return this.deps.getAdditionalSnapshot(refresh);
    if (this.deps.getAdditionalAccounts) {
      return this.deps.getAdditionalAccounts().then((accounts) => ({ registry: 'v1', accounts }));
    }
    return getAdditionalDashboardSnapshot({ refresh, excludeAntigravity });
  }

  /** Before the first collection finishes: the configured registry v2 accounts, else the seven defaults. */
  private additionalPlaceholders(excludeAntigravity: boolean): DashboardAccount[] {
    const configured = this.deps.getConfiguredAdditional
      ? this.deps.getConfiguredAdditional()
      : this.deps.getAdditionalAccounts || this.deps.getAdditionalSnapshot
        ? null
        : getConfiguredAdditionalAccounts({ excludeAntigravity });
    return configured && configured.registry !== 'v1'
      ? additionalAccounts(configured.accounts, configured.registry)
      : ADDITIONAL_PROVIDERS.map(([provider, label]) => additionalFallback(provider, label));
  }

  private async collect(
    state: DashboardState,
    scope: string,
    platform: ClaudeDashboardPlatform,
    refresh: boolean
  ): Promise<DashboardAccount[]> {
    const generation = ++state.generation;
    const registeredAntigravity = (
      this.deps.hasAntigravityProfiles ?? (() => hasAntigravityProfiles(scope))
    )();
    if (refresh) (this.deps.invalidateClaudeCache ?? invalidateClaudeDesktopUsageCache)();
    const parts = [
      ['codex', this.collectCodex(scope, refresh), () => this.cachedCodex(scope)],
      [
        'claude',
        this.collectClaude(scope, platform, refresh, (accounts) => {
          if (state.generation === generation) state.claude = accounts;
        }),
        () =>
          (this.claudeInventories.get(scope) ?? []).map((profile) =>
            claudeAccount(profile, platform)
          ),
      ],
      [
        'additional',
        Promise.resolve().then(async () => {
          if (this.deps.getAdditionalAccounts && !this.deps.getOptionalWalletAccounts)
            return additionalAccounts(await this.deps.getAdditionalAccounts());
          const primary = this.additionalSnapshot(refresh, registeredAntigravity).then(
            (snapshot) => {
              // The optional wallet must never delay the existing usage rows.
              if (state.generation === generation)
                state.additional = additionalAccounts(snapshot.accounts, snapshot.registry);
              return snapshot;
            }
          );
          const wallet = (
            this.deps.getOptionalWalletAccounts ??
            ((force) => getOpenCodeConsoleWalletAccounts({ refresh: force }))
          )(refresh).catch(() => []);
          const [snapshot, wallets] = await Promise.all([primary, wallet]);
          return additionalAccounts([...snapshot.accounts, ...wallets], snapshot.registry);
        }),
        () => this.additionalPlaceholders(registeredAntigravity),
      ],
      [
        'antigravity',
        registeredAntigravity
          ? Promise.resolve()
              .then(() =>
                (
                  this.deps.getAntigravityAccounts ??
                  ((force) => getAntigravityAccounts(force, scope))
                )(refresh)
              )
              .then((accounts) => {
                const rows = accounts.map(publicDashboardAccount);
                return this.deps.getAntigravityAccounts || this.deps.failover === null
                  ? rows
                  : recoverAntigravityUsage(rows, scope, {
                      failover: this.deps.failover ?? undefined,
                    });
              })
          : Promise.resolve([]),
        () =>
          registeredAntigravity
            ? (
                this.deps.getCachedAntigravityAccounts ??
                (() => getCachedAntigravityAccounts(scope))
              )().map(publicDashboardAccount)
            : [],
      ],
    ] as const;
    await Promise.all(
      parts.map(async ([key, pending, fallback]) => {
        // Slow sources can finish after the response deadline. Retain their safe
        // projections without allowing an older cycle to overwrite a newer one.
        const retained = pending.then((accounts) => {
          if (state.generation === generation) state[key] = accounts;
          return accounts;
        });
        const result = await this.bounded(retained, () =>
          state[key].length ? state[key] : fallback()
        );
        if (state.generation === generation) state[key] = result;
      })
    );
    state.fetchedAt = (this.deps.now ?? Date.now)();
    return [...state.codex, ...state.claude, ...state.additional, ...state.antigravity];
  }

  /** After a lifecycle change: the next response collects again instead of serving the cache. */
  invalidate(): void {
    for (const state of this.states.values()) state.fetchedAt = -Infinity;
  }

  /** A re-checked additional account replaces its cached row in every scope and platform. */
  replaceAdditionalRow(account: DashboardAccount): void {
    for (const state of this.states.values()) {
      state.additional = state.additional.map((row) => (row.id === account.id ? account : row));
    }
  }

  async get(
    platform: ClaudeDashboardPlatform = 'mac',
    refresh = false,
    context: AccountDashboardRequestContext = {}
  ): Promise<AccountDashboard> {
    return this.withPassedResets(await this.read(platform, refresh, context, true));
  }

  /**
   * The rows the last collection left, projected exactly like `get`, without
   * ever starting or waiting for a collection: no provider is asked for usage.
   * Used by the T3 usage hub, which must never add upstream quota reads on top
   * of the dashboard's own refresh (the Analytics sampler keeps it current).
   * Before the first collection of this scope and platform it has no rows.
   */
  async peek(
    platform: ClaudeDashboardPlatform = 'mac',
    context: AccountDashboardRequestContext = {}
  ): Promise<AccountDashboard> {
    return this.withPassedResets(await this.read(platform, false, context, false));
  }

  /** Per response: a window's reset can pass while its sample is still cached. */
  private withPassedResets(dashboard: AccountDashboard): AccountDashboard {
    const now = (this.deps.now ?? Date.now)();
    return {
      ...dashboard,
      accounts: dashboard.accounts.map((account) => markPassedResets(account, now)),
    };
  }

  private async read(
    platform: ClaudeDashboardPlatform,
    refresh: boolean,
    context: AccountDashboardRequestContext,
    collect: boolean
  ): Promise<AccountDashboard> {
    const startedAt = Date.now();
    const now = (this.deps.now ?? Date.now)();
    const scope = (this.deps.scope ?? getCcsDir)();
    const key = JSON.stringify([scope, platform]);
    let state = this.states.get(key);
    if (!state) {
      state = {
        fetchedAt: -Infinity,
        refreshedAt: -Infinity,
        generation: 0,
        pending: null,
        codex: [],
        claude: [],
        additional: [],
        antigravity: [],
      };
      this.states.set(key, state);
      while (this.states.size > MAX_SCOPES) {
        const oldest = this.states.keys().next().value;
        if (oldest === undefined) break;
        this.states.delete(oldest);
      }
    }
    const force = refresh && now - state.refreshedAt >= REFRESH_DEBOUNCE_MS;
    const refreshIntervalSeconds = (
      this.deps.refreshIntervalSeconds ?? getAccountRefreshIntervalSeconds
    )();
    const expired = now - state.fetchedAt >= refreshIntervalSeconds * 1000;
    if (collect && !state.pending && (force || expired)) {
      state.refreshedAt = now;
      const current = state;
      // Scheduled checks must bypass individual source TTLs, while those sources
      // retain their own in-flight coalescing and failure cooldowns.
      state.pending = this.collect(current, scope, platform, true).finally(() => {
        current.pending = null;
      });
    }
    const usedCache = !collect || state.pending === null;
    if (collect && state.pending) await state.pending;
    // Account switches invalidate the native summary by live auth file signature.
    // Refresh this inexpensive identity even when quota samples are still cached.
    const registeredAntigravity = (
      this.deps.hasAntigravityProfiles ?? (() => hasAntigravityProfiles(scope))
    )();
    const remainingBudget = Math.max(
      0,
      (this.deps.responseBudgetMs ?? 2500) - (Date.now() - startedAt)
    );
    const [summary, selectedAntigravityProfileId, visibility, pendingClaude] = await Promise.all([
      this.bounded(
        Promise.resolve().then(() => (this.deps.getCodexSummary ?? getCodexAuthProfilesSummary)()),
        () => null,
        remainingBudget
      ),
      registeredAntigravity
        ? this.bounded(
            Promise.resolve().then(() =>
              (
                this.deps.readSelectedAntigravityProfileId ??
                (() => readSelectedAntigravityProfileId(scope))
              )()
            ),
            () => null,
            remainingBudget
          )
        : Promise.resolve(null),
      // A small private file; unreadable means "could not be read", never "nothing hidden":
      // the last good lists of this scope stay in force, marked unavailable. A slow read
      // shows the last result, so a slow cycle never unhides rows either.
      this.bounded(
        Promise.resolve()
          .then(() => (this.deps.readVisibility ?? readAccountVisibility)(scope))
          .catch((): AccountVisibilityRead => ({ state: 'unavailable' }))
          .then((read) => this.visibility.record(scope, read)),
        () => this.visibility.current(scope),
        Math.max(remainingBudget, VISIBILITY_BUDGET_FLOOR_MS)
      ),
      // A small private file that exists only after a Claude Add; unreadable means no rows.
      this.bounded(
        Promise.resolve()
          .then(() => (this.deps.listClaudePending ?? (() => readPendingProfiles(scope)))())
          .catch((): PendingClaudeProfile[] => []),
        (): PendingClaudeProfile[] => [],
        Math.max(remainingBudget, VISIBILITY_BUDGET_FLOOR_MS)
      ),
    ]);
    const hiddenProviders = new Set<string>(visibility.hiddenProviders);
    const hiddenAccountIds = new Set(visibility.hiddenAccountIds);
    const trayHiddenProviders = new Set<string>(visibility.trayHiddenProviders);
    const trayHiddenAccountIds = new Set(visibility.trayHiddenAccountIds);
    // Validate saved credential revisions on every response, including dashboard
    // cache hits. This synchronous cache projection never starts a provider call.
    if (registeredAntigravity) {
      const previous = state.antigravity;
      state.antigravity = (
        this.deps.getCachedAntigravityAccounts ?? (() => getCachedAntigravityAccounts(scope))
      )().map((account) => {
        // The runtime cache never proves activation (it always reports false); the proof is
        // the one the last refresh retained below. Only a fallback or unverifiable cached
        // reading may veto it here, so the cache's own false is not carried into the peek.
        const publicAccount = publicDashboardAccount(account);
        const unvetoed = {
          ...publicAccount,
          capabilities: { ...publicAccount.capabilities, antigravityCanActivate: true },
        };
        const current =
          this.deps.getCachedAntigravityAccounts || this.deps.failover === null
            ? unvetoed
            : peekAntigravityUsage(unvetoed, scope, {
                failover: this.deps.failover ?? undefined,
              });
        const retained = previous.find((candidate) => candidate.id === current.id);
        return {
          ...current,
          capabilities: {
            ...current.capabilities,
            antigravityCanActivate:
              current.capabilities.antigravityCanActivate !== false &&
              (current.status === 'ok' || current.status === 'cached') &&
              current.capabilities.antigravityHostIds?.includes('ubuntu') === true &&
              retained?.email === current.email &&
              retained?.capabilities.antigravityProfileId ===
                current.capabilities.antigravityProfileId &&
              retained?.capabilities.antigravityCanActivate === true,
          },
        };
      });
    }
    const claudeIds = new Set(state.claude.map((account) => account.id));
    const accounts = [
      ...state.codex,
      ...state.claude,
      ...pendingClaude
        .map((profile) => pendingClaudeAccount(profile, platform))
        .filter((account) => !claudeIds.has(account.id)),
      ...state.additional.filter(
        (account) => !(registeredAntigravity && account.provider === 'antigravity')
      ),
      ...(registeredAntigravity ? state.antigravity : []),
    ];
    const server = (this.deps.serverInfo ?? getDashboardServerInfo)();
    const jobs = (this.deps.accountJobStates ?? (() => getSignInJobRunner().accountStates()))();
    const rows = accounts.flatMap((account) => {
      let current = account;
      let codexAuthValid: boolean | undefined;
      if (account.provider === 'codex' && summary) {
        const profile = summary.profiles.find(
          (candidate) => candidate.name === account.capabilities.codexProfile
        );
        if (!profile) return [];
        codexAuthValid = profile.authValid;
        const previous = this.codexInventories
          .get(scope)
          ?.profiles.find((candidate) => candidate.name === profile.name);
        if (
          !profile.authValid ||
          previous?.accountId !== profile.accountId ||
          account.email !== emailForComparison(profile.email)
        ) {
          current = codexAccount(
            profile,
            summary.activated,
            undefined,
            this.codexRenewals.get(scope)?.get(profile.name)
          );
        }
      }
      if (account.provider === 'codex' && codexAuthValid !== false) {
        // A slow or failed summary must not bypass the saved auth check for a
        // previously collected fallback. It needs no asynchronous inventory.
        current = peekCodexUsage(
          this.codexRowTargets.get(account) ?? null,
          current,
          this.deps.failover ?? undefined
        );
      }
      if (usedCache && current.status === 'ok') current = { ...current, status: 'cached' };
      if (current.provider === 'claude') current = projectClaudeFailoverExtras(current, now);
      return [
        {
          ...withJobState(
            withAccountState(
              {
                ...current,
                isActive:
                  current.provider === 'codex'
                    ? summary?.activated?.name === current.capabilities.codexProfile
                    : current.provider === 'antigravity' &&
                      registeredAntigravity &&
                      selectedAntigravityProfileId !== null &&
                      selectedAntigravityProfileId === current.capabilities.antigravityProfileId,
              },
              codexAuthValid
            ),
            jobs
          ),
          // Display only: hidden rows stay in the list, collected and switchable.
          hidden: hiddenProviders.has(current.provider) || hiddenAccountIds.has(current.id),
          // The trays' own switches; never derived from the dashboard's.
          trayHidden:
            trayHiddenProviders.has(current.provider) || trayHiddenAccountIds.has(current.id),
        },
      ];
    });
    return {
      schemaVersion: 1,
      updatedAt: new Date((this.deps.now ?? Date.now)()).toISOString(),
      settings: {
        refreshIntervalSeconds,
        hiddenProviders: [...visibility.hiddenProviders],
        hiddenAccountIds: [...visibility.hiddenAccountIds],
        trayHiddenProviders: [...visibility.trayHiddenProviders],
        trayHiddenAccountIds: [...visibility.trayHiddenAccountIds],
        visibilityAvailable: visibility.available,
      },
      providers: buildDashboardProviders(
        rows,
        visibility.hiddenProviders,
        visibility.trayHiddenProviders,
        (this.deps.providerFacts ?? lifecycleProviderFacts)(context)
      ),
      accounts: rows,
      antigravityAutoSwitch: (
        this.deps.getAntigravityAutoSwitchStatus ?? getAntigravityAutoSwitchStatus
      )(),
      codexAutoSwitch: withThresholdUsedPercent(
        (this.deps.getAutoSwitchStatus ?? (() => getCodexAutoSwitchService().getStatus()))()
      ),
      ...(server ? { server } : {}),
    };
  }
}

let service: AccountDashboardService | null = null;
export function getAccountDashboard(
  platform: ClaudeDashboardPlatform = 'mac',
  refresh = false,
  context: AccountDashboardRequestContext = {}
): Promise<AccountDashboard> {
  service ??= new AccountDashboardService();
  return service.get(platform, refresh, context);
}

/** Cache only: never starts a collection (see AccountDashboardService.peek). */
export function peekAccountDashboard(
  platform: ClaudeDashboardPlatform = 'mac',
  context: AccountDashboardRequestContext = {}
): Promise<AccountDashboard> {
  service ??= new AccountDashboardService();
  return service.peek(platform, context);
}

export function invalidateAccountDashboard(): void {
  service?.invalidate();
}

export function replaceAccountDashboardRow(account: DashboardAccount): void {
  service?.replaceAdditionalRow(account);
}
