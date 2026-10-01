import {
  getCodexAuthProfilesSummary,
  type CodexAuthProfilesSummary,
} from '../../codex-auth/codex-auth-dashboard-service';
import { getCcsDir } from '../../utils/config-manager';
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
import { getAdditionalDashboardAccounts } from './additional-account-service';
import { getOpenCodeConsoleWalletAccounts } from './opencode-console-wallet-service';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import {
  getCachedClaudeDesktopLiveUsage,
  getLiveClaudeDesktopUsage,
  type ClaudeDesktopLiveUsage,
} from './claude-desktop-live-service';
import type {
  AccountDashboard,
  ClaudeDashboardPlatform,
  DashboardAccount,
} from './account-dashboard-types';

import {
  ADDITIONAL_PROVIDERS,
  codexAccount,
  claudeAccount,
  additionalFallback,
  additionalAccounts,
  applyClaudeLiveUsage,
  emailForComparison,
} from './account-dashboard-projection';

export type { AccountDashboard, DashboardAccount } from './account-dashboard-types';

export interface AccountDashboardDeps {
  getCodexSummary?: () => Promise<CodexAuthProfilesSummary>;
  getCodexRows?: (names: string[], refresh: boolean) => Promise<BarSummaryRow[]>;
  getCachedCodexRows?: (names: string[]) => BarSummaryRow[];
  listClaudeProfiles?: () => Promise<ClaudeDesktopProfile[]>;
  getClaudeUsage?: (platform: ClaudeDashboardPlatform) => Promise<ClaudeDesktopUsage>;
  getLiveClaudeUsage?: (
    profileId: string,
    refresh: boolean
  ) => Promise<ClaudeDesktopLiveUsage | null>;
  getCachedLiveClaudeUsage?: (profileId: string) => Promise<ClaudeDesktopLiveUsage | null>;
  getAdditionalAccounts?: () => Promise<DashboardAccount[]>;
  getOptionalWalletAccounts?: (refresh: boolean) => Promise<DashboardAccount[]>;
  getAutoSwitchStatus?: () => AccountDashboard['codexAutoSwitch'];
  invalidateClaudeCache?: () => void;
  now?: () => number;
  responseBudgetMs?: number;
  scope?: () => string;
  refreshIntervalSeconds?: () => number;
}

const REFRESH_DEBOUNCE_MS = 5_000;
const MAX_SCOPES = 16;
interface DashboardState {
  fetchedAt: number;
  refreshedAt: number;
  generation: number;
  pending: Promise<DashboardAccount[]> | null;
  codex: DashboardAccount[];
  claude: DashboardAccount[];
  additional: DashboardAccount[];
}

export class AccountDashboardService {
  private readonly states = new Map<string, DashboardState>();
  private readonly codexInventories = new Map<string, CodexAuthProfilesSummary>();
  private readonly claudeInventories = new Map<string, ClaudeDesktopProfile[]>();
  private readonly claudeLiveSamples = new Map<string, ClaudeDesktopLiveUsage>();

  constructor(private readonly deps: AccountDashboardDeps = {}) {}

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
    const rows = await this.bounded(
      (
        this.deps.getCodexRows ??
        ((profiles, force) => getCodexProfileQuotaRows(profiles, {}, { force }))
      )(names, refresh),
      () => (this.deps.getCachedCodexRows ?? getCachedCodexProfileQuotaRows)(names)
    );
    return summary.profiles.map((profile) =>
      codexAccount(
        profile,
        summary.activated,
        rows.find((row) => row.profile === profile.name)
      )
    );
  }

  private cachedCodex(scope: string): DashboardAccount[] {
    const summary = this.codexInventories.get(scope);
    if (!summary) return [];
    const rows = (this.deps.getCachedCodexRows ?? getCachedCodexProfileQuotaRows)(
      summary.profiles.map((profile) => profile.name)
    );
    return summary.profiles.map((profile) =>
      codexAccount(
        profile,
        summary.activated,
        rows.find((row) => row.profile === profile.name)
      )
    );
  }

  private async collectClaude(
    scope: string,
    platform: ClaudeDashboardPlatform,
    refresh: boolean,
    publishCached: (accounts: DashboardAccount[]) => void
  ): Promise<DashboardAccount[]> {
    const profiles = await (this.deps.listClaudeProfiles ?? listClaudeDesktopProfiles)();
    this.claudeInventories.set(scope, profiles);
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
    publishCached(accounts);
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
        publishCached(accounts);
      });
    const live = Promise.all(
      profiles.map(async (profile, index) => {
        if (!profile.id) return;
        const sample = await Promise.resolve()
          .then(() =>
            (
              this.deps.getLiveClaudeUsage ??
              ((id, force) => getLiveClaudeDesktopUsage(id, { refresh: force }))
            )(profile.id as string, refresh)
          )
          .catch(() => null);
        if (!sample) return;
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
        publishCached(accounts);
      })
    );
    await Promise.all([history, live]);
    return accounts;
  }

  private async collect(
    state: DashboardState,
    scope: string,
    platform: ClaudeDashboardPlatform,
    refresh: boolean
  ): Promise<DashboardAccount[]> {
    const generation = ++state.generation;
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
        Promise.resolve()
          .then(() => {
            if (this.deps.getAdditionalAccounts && !this.deps.getOptionalWalletAccounts)
              return this.deps.getAdditionalAccounts();
            const primary = (
              this.deps.getAdditionalAccounts ?? (() => getAdditionalDashboardAccounts({ refresh }))
            )().then((accounts) => {
              // The optional wallet must never delay the seven existing usage rows.
              if (state.generation === generation) state.additional = additionalAccounts(accounts);
              return accounts;
            });
            const wallet = (
              this.deps.getOptionalWalletAccounts ??
              ((force) => getOpenCodeConsoleWalletAccounts({ refresh: force }))
            )(refresh).catch(() => []);
            return Promise.all([primary, wallet]).then((parts) => parts.flat());
          })
          .then(additionalAccounts),
        () => ADDITIONAL_PROVIDERS.map(([provider, label]) => additionalFallback(provider, label)),
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
    return [...state.codex, ...state.claude, ...state.additional];
  }

  async get(platform: ClaudeDashboardPlatform = 'mac', refresh = false): Promise<AccountDashboard> {
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
    if (!state.pending && (force || expired)) {
      state.refreshedAt = now;
      const current = state;
      // Scheduled checks must bypass individual source TTLs, while those sources
      // retain their own in-flight coalescing and failure cooldowns.
      state.pending = this.collect(current, scope, platform, true).finally(() => {
        current.pending = null;
      });
    }
    const usedCache = state.pending === null;
    if (state.pending) await state.pending;
    // Account switches invalidate the native summary by live auth file signature.
    // Refresh this inexpensive identity even when quota samples are still cached.
    const summary = await this.bounded(
      Promise.resolve().then(() => (this.deps.getCodexSummary ?? getCodexAuthProfilesSummary)()),
      () => null,
      Math.max(0, (this.deps.responseBudgetMs ?? 2500) - (Date.now() - startedAt))
    );
    const accounts = [...state.codex, ...state.claude, ...state.additional];
    return {
      schemaVersion: 1,
      updatedAt: new Date((this.deps.now ?? Date.now)()).toISOString(),
      settings: { refreshIntervalSeconds },
      accounts: accounts.flatMap((account) => {
        let current = account;
        if (account.provider === 'codex' && summary) {
          const profile = summary.profiles.find(
            (candidate) => candidate.name === account.capabilities.codexProfile
          );
          if (!profile) return [];
          const previous = this.codexInventories
            .get(scope)
            ?.profiles.find((candidate) => candidate.name === profile.name);
          if (
            !profile.authValid ||
            previous?.accountId !== profile.accountId ||
            account.email !== emailForComparison(profile.email)
          ) {
            current = codexAccount(profile, summary.activated, undefined);
          }
        }
        return [
          {
            ...current,
            status: usedCache && current.status === 'ok' ? 'cached' : current.status,
            isActive:
              current.provider === 'codex' &&
              summary?.activated?.name === current.capabilities.codexProfile,
          },
        ];
      }),
      codexAutoSwitch: (
        this.deps.getAutoSwitchStatus ?? (() => getCodexAutoSwitchService().getStatus())
      )(),
    };
  }
}

let service: AccountDashboardService | null = null;
export function getAccountDashboard(
  platform: ClaudeDashboardPlatform = 'mac',
  refresh = false
): Promise<AccountDashboard> {
  service ??= new AccountDashboardService();
  return service.get(platform, refresh);
}
