import { createHash } from 'crypto';
import * as path from 'path';
import type { DashboardAccount } from './account-dashboard-types';
import { HOST_LABELS, DASHBOARD_HOSTS } from './dashboard-hosts';
import { configuredUsageHosts, runUsageHelper } from './usage-failover-transport';
import {
  holdUsageCooldown,
  identityUsageCooldownKey,
  readUsageCooldown,
} from './usage-failover-cooldown';
import {
  projectClaudeFailoverExtras,
  retainClaudeFailoverExtras,
  stripClaudeFailoverExtras,
} from './usage-failover-claude-extras';
import { readUsageCache, writeUsageCache } from './usage-failover-cache';
import {
  hasUsage,
  isRecord,
  parseUsageCandidate,
  parseUsageIdentity,
  parseUsageReading,
  sameUsageIdentity,
  safeUsageText,
  USAGE_FAILURE_CODES,
  type UsageCandidate,
  type UsageFailoverTarget,
  type UsageHelperRequest,
  type UsageHelperResponse,
  type UsageHost,
  type UsageIdentity,
  type UsageReading,
} from './usage-failover-contract';

interface SavedReading {
  reading: UsageReading;
  host: UsageHost['host'];
  candidate: UsageCandidate;
  fallback: boolean;
}
interface State {
  identity: UsageIdentity | null;
  primary: UsageCandidate | null;
  saved: SavedReading | null;
  nextAttempt: number;
  pending: Promise<void> | null;
  fresh: boolean;
  primarySignIn: Set<'mac' | 'windows'>;
  primaryFailure: string | null;
  observedPlan: string | null;
}
export interface UsageFailoverDeps {
  transport?: (host: UsageHost, request: UsageHelperRequest) => Promise<UsageHelperResponse>;
  hosts?: () => Promise<UsageHost[]>;
  now?: () => number;
}
const digest = (value: unknown) => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const hostKey = (host: UsageHost) => JSON.stringify([host.platform, host.sshHost]);
const emailMatches = (expected: string | null | undefined, actual: string | null) =>
  !expected || (!!actual && expected.toLowerCase() === actual.toLowerCase());
const walletMatches = (target: UsageFailoverTarget, candidate: UsageCandidate) => {
  if (target.provider !== 'opencode-wallet') return true;
  const match = /^plan-opencode-go-console-mac-([a-f0-9]{12})$/.exec(target.accountId);
  return (
    !!match &&
    candidate.identity.kind === 'opencode-workspace' &&
    candidate.identity.key.slice(0, 12) === match[1]
  );
};
const candidateMatchesTarget = (target: UsageFailoverTarget, candidate: UsageCandidate) =>
  walletMatches(target, candidate) &&
  (!target.expectedIdentity || sameUsageIdentity(target.expectedIdentity, candidate.identity)) &&
  (!candidate.email || emailMatches(target.expectedEmail, candidate.email));

/** Display-only quota recovery. It cannot activate accounts or produce an activation proof. */
export class UsageFailoverService {
  private readonly states = new Map<string, State>();
  private readonly inventories = new Map<
    string,
    { expires: number; pending: Promise<UsageCandidate[] | null> }
  >();
  private readonly cooldowns = new Map<string, number>();
  private readonly signInSelectors = new Map<string, string[]>();
  private active = 0;
  private readonly waiting: Array<() => void> = [];
  constructor(private readonly deps: UsageFailoverDeps = {}) {}

  private now(): number {
    return (this.deps.now ?? Date.now)();
  }
  private key(target: UsageFailoverTarget): string {
    return digest([
      path.resolve(target.ccsDir),
      target.provider,
      target.accountId,
      target.binding,
      target.primarySources,
      target.expectedEmail?.toLowerCase() ?? null,
      target.expectedIdentity ?? null,
    ]);
  }
  private file(target: UsageFailoverTarget): string {
    return path.join(target.ccsDir, 'account-usage-failover', `${this.key(target)}.json`);
  }
  private state(target: UsageFailoverTarget): State {
    const key = this.key(target);
    const existing = this.states.get(key);
    if (existing) return existing;
    const state: State = {
      identity: target.expectedIdentity ?? null,
      primary: null,
      saved: null,
      nextAttempt: 0,
      pending: null,
      fresh: false,
      primarySignIn: new Set(),
      primaryFailure: null,
      observedPlan: null,
    };
    try {
      const file = this.file(target);
      const value = readUsageCache(file);
      if (isRecord(value) && value.version === 1 && value.key === key) {
        if (
          Array.isArray(value.primarySignIn) &&
          value.primarySignIn.length <= 2 &&
          value.primarySignIn.every((host) => host === 'mac' || host === 'windows')
        )
          state.primarySignIn = new Set(value.primarySignIn as Array<'mac' | 'windows'>);
        if (
          typeof value.primaryFailure === 'string' &&
          USAGE_FAILURE_CODES.has(value.primaryFailure)
        )
          state.primaryFailure = value.primaryFailure;
        const identity = parseUsageIdentity(value.identity);
        if (
          identity &&
          (!target.expectedIdentity || sameUsageIdentity(target.expectedIdentity, identity))
        ) {
          state.identity = identity;
          state.observedPlan =
            target.provider === 'muse' ? safeUsageText(value.observedPlan) : null;
          state.primary = parseUsageCandidate(value.primary);
          if (
            isRecord(value.saved) &&
            DASHBOARD_HOSTS.includes(value.saved.host as UsageHost['host'])
          ) {
            const reading = parseUsageReading(value.saved.reading, this.now());
            const candidate = parseUsageCandidate(value.saved.candidate);
            if (
              candidate &&
              sameUsageIdentity(identity, candidate.identity) &&
              hasUsage(reading) &&
              emailMatches(target.expectedEmail, reading.email)
            )
              state.saved = {
                reading,
                candidate,
                host: value.saved.host as UsageHost['host'],
                fallback: value.saved.fallback === true,
              };
          }
          if (typeof value.nextAttempt === 'number' && Number.isFinite(value.nextAttempt))
            state.nextAttempt = Math.min(value.nextAttempt, this.now() + 86400_000);
        }
      }
    } catch {
      /* Missing or damaged cache is not quota. */
    }
    this.states.set(key, state);
    if (this.states.size > 256) {
      for (const [old, entry] of this.states) {
        if (old !== key && !entry.pending) {
          this.states.delete(old);
          break;
        }
      }
    }
    return state;
  }
  private persist(target: UsageFailoverTarget, state: State): void {
    writeUsageCache(this.file(target), {
      version: 1,
      key: this.key(target),
      identity: state.identity,
      primary: state.primary,
      saved: state.saved,
      nextAttempt: state.nextAttempt,
      primaryFailure: state.primaryFailure,
      primarySignIn: [...state.primarySignIn],
      observedPlan: state.observedPlan,
    });
  }
  private async request(
    host: UsageHost,
    request: UsageHelperRequest,
    beforeStart?: () => boolean,
    observe?: (response: UsageHelperResponse) => void
  ): Promise<UsageHelperResponse | null> {
    if (this.active >= 3) await new Promise<void>((resolve) => this.waiting.push(resolve));
    else this.active++;
    try {
      if (beforeStart && !beforeStart()) return null;
      const response = await (this.deps.transport ?? runUsageHelper)(host, request);
      // Publish identity cooldowns before a queued request receives this slot.
      observe?.(response);
      return response;
    } finally {
      const next = this.waiting.shift();
      if (next) next();
      else this.active--;
    }
  }
  private inventory(
    target: UsageFailoverTarget,
    host: UsageHost
  ): Promise<UsageCandidate[] | null> {
    const key = this.inventoryKey(target, host);
    const cached = this.inventories.get(key);
    if (cached && cached.expires > this.now()) return cached.pending;
    const pending = this.request(host, {
      version: 1,
      action: 'inventory',
      provider: target.provider,
      ...(this.claudeProfile(target, host)
        ? { claudeProfile: this.claudeProfile(target, host) }
        : {}),
      ...(host.sshHost === null ? { ccsDir: target.ccsDir } : {}),
    })
      .then((value) => {
        if (value?.action !== 'inventory') return null;
        if (target.provider === 'claude')
          this.signInSelectors.set(key, value.signInNeededSelectors ?? []);
        return value.candidates;
      })
      .catch(() => null);
    this.inventories.set(key, { expires: this.now() + 15_000, pending });
    if (this.inventories.size > 256) {
      for (const [old, entry] of this.inventories)
        if (entry.expires <= this.now()) {
          this.inventories.delete(old);
          this.signInSelectors.delete(old);
        }
    }
    return pending;
  }
  private inventoryKey(target: UsageFailoverTarget, host: UsageHost): string {
    return JSON.stringify([
      target.ccsDir,
      target.provider,
      hostKey(host),
      this.claudeProfile(target, host),
    ]);
  }
  private claudeProfile(target: UsageFailoverTarget, host: UsageHost) {
    return target.provider === 'claude'
      ? target.primarySources.find((source) => hostKey(source) === hostKey(host))?.claudeProfile
      : undefined;
  }
  private applyInventorySignIn(target: UsageFailoverTarget, state: State, host: UsageHost): void {
    if (target.provider !== 'claude' || (host.host !== 'mac' && host.host !== 'windows')) return;
    const selectors = this.signInSelectors.get(this.inventoryKey(target, host)) ?? [];
    if (
      target.primarySources.some(
        (source) => hostKey(source) === hostKey(host) && selectors.includes(source.selector)
      )
    )
      state.primarySignIn.add(host.host);
  }
  private project(
    target: UsageFailoverTarget,
    template: DashboardAccount,
    state: State,
    cached: boolean
  ): DashboardAccount {
    if (!state.saved || template.statusReason === 'identity_unbound')
      return this.withSignIn(template, state);
    const { reading, host, fallback } = state.saved;
    if (!emailMatches(target.expectedEmail, reading.email)) return this.withSignIn(template, state);
    const stale = cached || reading.status === 'cached';
    const result: DashboardAccount = {
      ...template,
      email: template.email ?? reading.email,
      plan: reading.plan,
      sampledAt: reading.sampledAt,
      fetchedAt: reading.fetchedAt,
      windows: structuredClone(reading.windows),
      ...(reading.antigravityPlan
        ? { antigravityPlan: structuredClone(reading.antigravityPlan) }
        : {}),
      status: stale ? 'cached' : 'ok',
      source: `Verified quota on ${HOST_LABELS[host]}${fallback ? ' (fallback)' : ''}`,
      message: stale ? 'Showing the last successful reading; live usage is unavailable.' : null,
      capabilities: {
        ...template.capabilities,
        ...(template.provider === 'antigravity' && fallback
          ? { antigravityCanActivate: false }
          : {}),
      },
    };
    if (template.status === 'needs_sign_in') {
      result.status = 'needs_sign_in';
      result.message = template.message;
      if (stale)
        result.windows = result.windows.map((window) => ({
          ...window,
          status: 'cached',
          sampledAt: window.sampledAt ?? reading.sampledAt ?? undefined,
        }));
    }
    // Claude's platform selects its launcher. Source identifies where quota was read.
    if (template.provider !== 'claude') result.platform = host === 'nas1' ? 'ubuntu' : host;
    if (host === 'nas1') result.host = 'nas1';
    else delete result.host;
    return this.withSignIn(result, state);
  }
  private withSignIn(account: DashboardAccount, state: State): DashboardAccount {
    const messages: Record<string, string> = {
      needs_sign_in: 'Sign in again on the configured source to refresh usage.',
      no_browser_cookie: 'Connect a browser quota session for the configured account.',
      choose_team: 'Choose a team in the configured browser quota session.',
      inactive_subscription: 'The configured account subscription is inactive.',
      rate_limited: 'Usage refresh is rate limited. The provider cooldown is being respected.',
      identity_mismatch: 'The configured source no longer matches this account.',
      account_mismatch: 'The configured source no longer matches this account.',
      source_changed: 'The configured sign-in changed; usage will refresh shortly.',
      identity_unavailable: 'The configured account identity could not be verified.',
      plan_mismatch: 'The configured quota session no longer matches this subscription.',
      team_mismatch: 'The configured quota session no longer matches this team.',
    };
    const code = state.primaryFailure;
    const requiresSignIn =
      code && ['needs_sign_in', 'no_browser_cookie', 'choose_team'].includes(code);
    if (
      code &&
      messages[code] &&
      account.status !== 'needs_sign_in' &&
      (!hasUsage(account) || (requiresSignIn && account.provider !== 'claude'))
    )
      account = {
        ...account,
        status: requiresSignIn ? 'needs_sign_in' : 'unavailable',
        message: messages[code],
        ...(account.status === 'cached'
          ? {
              windows: account.windows.map((window) => ({
                ...window,
                status: 'cached' as const,
                sampledAt: window.sampledAt ?? account.sampledAt ?? undefined,
              })),
            }
          : {}),
      };
    if (account.provider !== 'claude') return account;
    const signInNeeded = [...new Set([...(account.signInNeeded ?? []), ...state.primarySignIn])];
    const result = projectClaudeFailoverExtras({ ...account }, this.now());
    if (signInNeeded.length) result.signInNeeded = signInNeeded;
    else delete result.signInNeeded;
    return result;
  }
  private retainExtras(
    target: UsageFailoverTarget,
    state: State,
    reading: UsageReading,
    candidate: UsageCandidate,
    host: UsageHost['host']
  ): UsageReading {
    if (target.provider !== 'claude') return reading;
    const previous = state.saved;
    const sameSource =
      !!previous &&
      previous.host === host &&
      previous.candidate.locator === candidate.locator &&
      previous.candidate.revision === candidate.revision &&
      sameUsageIdentity(previous.candidate.identity, candidate.identity);
    return retainClaudeFailoverExtras(previous?.reading ?? null, reading, sameSource, this.now());
  }
  peek(target: UsageFailoverTarget, template: DashboardAccount): DashboardAccount {
    const state = this.state(target);
    // Keep a newer independently verified primary sample.
    if (
      hasUsage(template) &&
      (!state.saved ||
        Date.parse(template.sampledAt ?? '') >= Date.parse(state.saved.reading.sampledAt ?? ''))
    )
      return this.withSignIn(template, state);
    return this.project(target, template, state, true);
  }
  collect(target: UsageFailoverTarget, template: DashboardAccount): Promise<DashboardAccount> {
    return this.run(target, template, false);
  }
  recover(target: UsageFailoverTarget, primary: DashboardAccount): Promise<DashboardAccount> {
    return this.run(target, primary, true);
  }
  /** Native collectors can inspect the same account hold before their own provider request. */
  cooldownUntil(target: UsageFailoverTarget): number {
    const identity = target.expectedIdentity ?? this.state(target).identity;
    if (!identity) return 0;
    const key = identityUsageCooldownKey(target.ccsDir, target.provider, identity);
    const until = Math.max(
      this.cooldowns.get(key) ?? 0,
      readUsageCooldown(target.ccsDir, key, this.now())
    );
    return until > this.now() ? until : 0;
  }
  holdCooldown(target: UsageFailoverTarget, requestedUntil: number): number {
    const state = this.state(target);
    const identity = target.expectedIdentity ?? state.identity;
    if (!identity) return 0;
    const key = identityUsageCooldownKey(target.ccsDir, target.provider, identity);
    const until = holdUsageCooldown(target.ccsDir, key, requestedUntil, this.now());
    this.cooldowns.set(key, until);
    state.nextAttempt = Math.max(state.nextAttempt, until);
    this.persist(target, state);
    return until;
  }
  private async run(
    target: UsageFailoverTarget,
    template: DashboardAccount,
    primaryAlreadyRead: boolean
  ): Promise<DashboardAccount> {
    if (template.statusReason === 'identity_unbound') return template;
    const state = this.state(target);
    if (!state.pending && this.now() >= state.nextAttempt) {
      state.pending = this.refresh(target, template, state, primaryAlreadyRead)
        .catch(() => {
          state.nextAttempt = this.now() + 30_000;
        })
        .finally(() => {
          state.pending = null;
          this.persist(target, state);
        });
    }
    if (state.pending) await state.pending;
    if (
      hasUsage(template) &&
      template.status === 'ok' &&
      (!state.saved ||
        Date.parse(template.sampledAt ?? '') >= Date.parse(state.saved.reading.sampledAt ?? ''))
    )
      return this.withSignIn(template, state);
    return this.project(target, template, state, !state.fresh);
  }
  private async refresh(
    target: UsageFailoverTarget,
    template: DashboardAccount,
    state: State,
    primaryAlreadyRead: boolean
  ): Promise<void> {
    state.nextAttempt = this.now() + 15_000;
    state.fresh = false;
    state.primaryFailure = template.status === 'needs_sign_in' ? 'needs_sign_in' : null;
    const primaries: Array<{ host: UsageHost; candidate: UsageCandidate }> = [];
    let missingPrimary = false;
    for (const source of target.primarySources) {
      const candidates = await this.inventory(target, source);
      this.applyInventorySignIn(target, state, source);
      const selected = candidates?.filter((item) => item.selectors.includes(source.selector));
      const eligible = selected?.filter((item) => candidateMatchesTarget(target, item));
      const candidate =
        eligible?.find((item) => emailMatches(target.expectedEmail, item.email)) ?? eligible?.[0];
      if (selected?.length && !candidate) {
        state.identity = target.expectedIdentity ?? null;
        state.primary = null;
        state.saved = null;
        state.observedPlan = null;
        return;
      }
      if (!candidate) {
        if (candidates !== null) missingPrimary = true;
        continue;
      }
      if (!state.identity || !sameUsageIdentity(state.identity, candidate.identity)) {
        state.identity = candidate.identity;
        state.saved = null;
        state.observedPlan = null;
        state.primary = candidate;
      } else if (
        state.primary &&
        state.primary.locator === candidate.locator &&
        state.primary.revision !== candidate.revision
      ) {
        // Token renewal does not erase a dated reading for the same verified
        // account. Browser-derived Claude extras need their original revision.
        if (target.provider === 'claude' && state.saved)
          state.saved.reading = stripClaudeFailoverExtras(state.saved.reading);
      }
      state.primary = candidate;
      primaries.push({ host: source, candidate });
      break;
    }
    if (!primaries.length && missingPrimary && !target.expectedIdentity) {
      state.identity = null;
      state.primary = null;
      state.saved = null;
      state.observedPlan = null;
      return;
    }
    let lists: Array<{ host: UsageHost; candidates: UsageCandidate[] | null }> | null = null;
    const peerLists = async () => {
      if (lists) return lists;
      const hosts = [
        ...target.primarySources,
        ...(await (this.deps.hosts ?? configuredUsageHosts)()),
      ].filter(
        (host, index, all) => all.findIndex((other) => hostKey(other) === hostKey(host)) === index
      );
      lists = await Promise.all(
        hosts.map(async (host) => ({ host, candidates: await this.inventory(target, host) }))
      );
      for (const { host } of lists) this.applyInventorySignIn(target, state, host);
      return lists;
    };
    if (!state.identity) return; // Never bootstrap from an unrelated peer login.
    const cooldownKey = identityUsageCooldownKey(target.ccsDir, target.provider, state.identity);
    const applyCooldown = (): boolean => {
      const until = Math.max(
        this.cooldowns.get(cooldownKey) ?? 0,
        readUsageCooldown(target.ccsDir, cooldownKey, this.now())
      );
      if (until <= this.now()) {
        this.cooldowns.delete(cooldownKey);
        return false;
      }
      this.cooldowns.set(cooldownKey, until);
      state.nextAttempt = Math.max(state.nextAttempt, until);
      return true;
    };
    if (primaryAlreadyRead && hasUsage(template) && primaries.length) {
      const reading = parseUsageReading(template, this.now());
      if (hasUsage(reading) && emailMatches(target.expectedEmail, reading.email)) {
        state.saved = {
          reading: this.retainExtras(
            target,
            state,
            reading,
            primaries[0].candidate,
            primaries[0].host.host
          ),
          candidate: primaries[0].candidate,
          host: primaries[0].host.host,
          fallback: false,
        };
        state.fresh = template.status === 'ok';
        if (state.fresh) {
          state.primaryFailure = null;
          if (primaries[0].host.host === 'mac' || primaries[0].host.host === 'windows')
            state.primarySignIn.delete(primaries[0].host.host);
          return;
        }
      }
    }
    if (applyCooldown()) return;
    if (
      primaryAlreadyRead &&
      /(?:rate.?limit|\b429\b|too many requests)/i.test(template.message ?? '')
    ) {
      state.nextAttempt = holdUsageCooldown(
        target.ccsDir,
        cooldownKey,
        this.now() + 60_000,
        this.now()
      );
      this.cooldowns.set(cooldownKey, state.nextAttempt);
      return;
    }
    const tried = new Set<string>();
    const attempt = async (
      host: UsageHost,
      candidate: UsageCandidate,
      fallback: boolean
    ): Promise<'done' | 'next'> => {
      if (
        !sameUsageIdentity(state.identity, candidate.identity) ||
        !walletMatches(target, candidate) ||
        (candidate.email && !emailMatches(target.expectedEmail, candidate.email))
      )
        return 'next';
      const key = JSON.stringify([hostKey(host), candidate.locator]);
      if (tried.has(key)) return 'next';
      tried.add(key);
      let response: UsageHelperResponse | null;
      try {
        response = await this.request(
          host,
          {
            version: 1,
            action: 'collect',
            provider: target.provider,
            candidate,
            ...(this.claudeProfile(target, host)
              ? { claudeProfile: this.claudeProfile(target, host) }
              : {}),
            ...(host.sshHost === null ? { ccsDir: target.ccsDir } : {}),
          },
          () => !applyCooldown(),
          (value) => {
            if (value.action !== 'collect' || value.failureCode !== 'rate_limited') return;
            state.nextAttempt = holdUsageCooldown(
              target.ccsDir,
              cooldownKey,
              this.now() + Math.max(60, Math.min(86400, value.retryAfterSeconds ?? 60)) * 1000,
              this.now()
            );
            this.cooldowns.set(cooldownKey, state.nextAttempt);
          }
        );
      } catch {
        return 'next';
      }
      if (response === null) return 'done';
      if (response.action !== 'collect') return 'next';
      const primarySource = target.primarySources.some(
        (source) =>
          hostKey(source) === hostKey(host) && candidate.selectors.includes(source.selector)
      );
      if (primarySource) state.primaryFailure = response.failureCode;
      if (
        target.provider === 'claude' &&
        primarySource &&
        response.failureCode === 'needs_sign_in' &&
        (host.host === 'mac' || host.host === 'windows')
      )
        state.primarySignIn.add(host.host);
      if (
        [
          'needs_sign_in',
          'identity_mismatch',
          'account_mismatch',
          'identity_unavailable',
          'source_changed',
          'plan_mismatch',
          'team_mismatch',
        ].includes(response.failureCode ?? '') &&
        state.saved?.candidate.locator === candidate.locator &&
        state.saved.host === host.host
      )
        state.saved = null;
      const blocked = response.failureCode === 'rate_limited';
      if (
        primarySource &&
        [
          'identity_mismatch',
          'account_mismatch',
          'identity_unavailable',
          'source_changed',
          'plan_mismatch',
          'team_mismatch',
        ].includes(response.failureCode ?? '')
      ) {
        state.identity = target.expectedIdentity ?? null;
        state.primary = null;
        state.saved = null;
        state.observedPlan = null;
        return 'done';
      }
      if (
        !response.candidate ||
        response.candidate.locator !== candidate.locator ||
        response.candidate.revision !== candidate.revision ||
        !sameUsageIdentity(candidate.identity, response.candidate.identity)
      )
        return blocked ? 'done' : 'next';
      const observedPlan = safeUsageText(response.observedPlan);
      if (target.provider === 'muse' && primarySource && observedPlan) {
        state.observedPlan = observedPlan;
        if (state.saved && state.saved.reading.plan !== state.observedPlan) state.saved = null;
      }
      const reading = parseUsageReading(response.sample, this.now());
      if (!hasUsage(reading) || !emailMatches(target.expectedEmail, reading.email))
        return blocked ? 'done' : 'next';
      if (target.provider === 'muse') {
        if (reading.status === 'ok') state.observedPlan = reading.plan;
        else if (state.observedPlan && reading.plan !== state.observedPlan)
          return blocked ? 'done' : 'next';
      }
      if (
        target.provider === 'claude' &&
        primarySource &&
        reading.status === 'ok' &&
        (host.host === 'mac' || host.host === 'windows')
      )
        state.primarySignIn.delete(host.host);
      if (
        !state.saved ||
        Date.parse(reading.sampledAt ?? '') >= Date.parse(state.saved.reading.sampledAt ?? '')
      ) {
        state.saved = {
          reading: this.retainExtras(target, state, reading, candidate, host.host),
          candidate,
          host: host.host,
          fallback,
        };
        state.fresh = reading.status === 'ok';
      }
      if (blocked) return 'done';
      if (
        response.failureCode === null &&
        response.refreshAfterSeconds &&
        response.refreshAfterSeconds > 0
      ) {
        state.nextAttempt = Math.max(
          state.nextAttempt,
          this.now() + Math.min(86400, response.refreshAfterSeconds) * 1000
        );
        return 'done';
      }
      return reading.status === 'ok' ? 'done' : 'next';
    };
    for (const primary of primaries) {
      if (!primaryAlreadyRead && (await attempt(primary.host, primary.candidate, false)) === 'done')
        return;
      if (primaryAlreadyRead)
        tried.add(JSON.stringify([hostKey(primary.host), primary.candidate.locator]));
    }
    for (const { host, candidates } of await peerLists()) {
      for (const candidate of candidates ?? [])
        if ((await attempt(host, candidate, true)) === 'done') return;
    }
    state.nextAttempt = this.now() + 30_000;
  }
}

let singleton: UsageFailoverService | null = null;
export function getUsageFailoverService(): UsageFailoverService {
  return (singleton ??= new UsageFailoverService());
}
