import { createHash } from 'crypto';
import * as path from 'path';
import type { DashboardAccount } from './account-dashboard-types';
import { HOST_LABELS, DASHBOARD_HOSTS, helperPlatform } from './dashboard-hosts';
import {
  configuredUsageHosts,
  runUsageHelper,
  usageHelperDeadlineMs,
} from './usage-failover-transport';
import { withDeadline } from '../../utils/settle-deadline';
import {
  holdUsageCooldown,
  identityUsageCooldownKey,
  readUsageCooldown,
  readUsageCooldownState,
  settleUsageCooldown,
} from './usage-failover-cooldown';
import {
  projectClaudeFailoverExtras,
  retainClaudeFailoverExtras,
  stripClaudeFailoverExtras,
} from './usage-failover-claude-extras';
import { readUsageCache, writeUsageCache } from './usage-failover-cache';
import { claudeDesktopDelayMs, MAX_CLAUDE_429_STREAK } from './usage-failover-claude-cadence';
import { readAccountRefreshSettings } from './account-refresh-settings';
import {
  isBoundMusePeer,
  parseMuseUsagePeer,
  type MuseUsagePeer,
} from './muse-usage-peer-bindings';
import {
  hasUsage,
  isRecord,
  parseUsageCandidate,
  parseUsageIdentity,
  parseUsageReading,
  sameUsageIdentity,
  safeUsageText,
  usageTimestamp,
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
  fresh: boolean | null;
  freshUntil: number;
  primarySignIn: Set<'mac' | 'windows'>;
  primaryFailure: string | null;
  observedPlan: string | null;
  observedPlanAt: string | null;
  observedPlanPeer: MuseUsagePeer | null;
}
export interface UsageFailoverDeps {
  transport?: (host: UsageHost, request: UsageHelperRequest) => Promise<UsageHelperResponse>;
  hosts?: () => Promise<UsageHost[]>;
  now?: () => number;
  /** Overrides usageHelperDeadlineMs so tests need not wait for a real hang. */
  requestDeadlineMs?: (action: UsageHelperRequest['action']) => number;
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
const responseIsRateLimited = (response: UsageHelperResponse): boolean =>
  response.action === 'collect' &&
  (response.failureCode === 'rate_limited' ||
    (response.failureCode === 'source_changed' && response.rateLimited === true));
const tracksVerifiedPlan = (target: UsageFailoverTarget): boolean =>
  target.provider === 'muse' || target.provider === 'cursor';

/** Display-only quota recovery. It never changes which account a program uses. */
export class UsageFailoverService {
  private readonly states = new Map<string, State>();
  private readonly inventories = new Map<
    string,
    { expires: number; pending: Promise<UsageCandidate[] | null> }
  >();
  private readonly cooldowns = new Map<string, number>();
  private readonly signInSelectors = new Map<string, string[]>();
  private readonly unreadSelectors = new Map<string, Set<string>>();
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
  private matchesIdentity(
    target: UsageFailoverTarget,
    identity: UsageIdentity | null,
    candidate: Pick<UsageCandidate, 'identity'>,
    host: UsageHost['host']
  ): boolean {
    return (
      sameUsageIdentity(identity, candidate.identity) ||
      (target.provider === 'muse' &&
        isBoundMusePeer(target.ccsDir, this.key(target), identity, candidate.identity, host))
    );
  }
  private validateSaved(target: UsageFailoverTarget, state: State): void {
    if (
      state.observedPlanPeer &&
      !this.matchesIdentity(
        target,
        state.identity,
        state.observedPlanPeer,
        state.observedPlanPeer.host
      )
    ) {
      state.observedPlan = null;
      state.observedPlanAt = null;
      state.observedPlanPeer = null;
    }
    if (
      state.saved &&
      !this.matchesIdentity(target, state.identity, state.saved.candidate, state.saved.host)
    ) {
      state.saved = null;
    }
  }
  private state(target: UsageFailoverTarget): State {
    const key = this.key(target);
    const existing = this.states.get(key);
    if (existing) {
      // Revoking a link also revokes an in-memory observation, even during cooldown.
      this.validateSaved(target, existing);
      return existing;
    }
    const state: State = {
      identity: target.expectedIdentity ?? null,
      primary: null,
      saved: null,
      nextAttempt: 0,
      pending: null,
      fresh: null,
      freshUntil: 0,
      primarySignIn: new Set(),
      primaryFailure: null,
      observedPlan: null,
      observedPlanAt: null,
      observedPlanPeer: null,
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
        const identityMatchesTarget =
          identity &&
          (!target.expectedIdentity || sameUsageIdentity(target.expectedIdentity, identity));
        if (identityMatchesTarget) {
          state.identity = identity;
          state.observedPlan = tracksVerifiedPlan(target)
            ? safeUsageText(value.observedPlan)
            : null;
          state.observedPlanAt = usageTimestamp(value.observedPlanAt, this.now());
          if (!state.observedPlanAt) state.observedPlan = null;
          if (value.observedPlanPeer !== undefined && value.observedPlanPeer !== null) {
            state.observedPlanPeer =
              target.provider === 'muse' ? parseMuseUsagePeer(value.observedPlanPeer) : null;
            if (!state.observedPlanPeer) {
              state.observedPlan = null;
              state.observedPlanAt = null;
            }
          }
          state.primary = parseUsageCandidate(value.primary);
          if (
            isRecord(value.saved) &&
            DASHBOARD_HOSTS.includes(value.saved.host as UsageHost['host'])
          ) {
            const reading = parseUsageReading(value.saved.reading, this.now());
            const candidate = parseUsageCandidate(value.saved.candidate);
            if (candidate && hasUsage(reading) && emailMatches(target.expectedEmail, reading.email))
              state.saved = {
                reading,
                candidate,
                host: value.saved.host as UsageHost['host'],
                fallback: value.saved.fallback === true,
              };
          }
          // Only new caches record the outcome of the last attempt. An old
          // saved sample alone cannot prove that a later read succeeded.
          state.fresh =
            value.fresh === true && state.saved?.reading.status === 'ok'
              ? true
              : value.fresh === false
                ? false
                : null;
          if (typeof value.freshUntil === 'number' && Number.isFinite(value.freshUntil))
            state.freshUntil = Math.min(value.freshUntil, this.now() + 86400_000);
        }
        // A 429 can invalidate the source binding without ending its scheduled hold.
        if (
          (identityMatchesTarget ||
            (target.claudeDesktopCadence && state.primaryFailure === 'source_changed')) &&
          typeof value.nextAttempt === 'number' &&
          Number.isFinite(value.nextAttempt)
        )
          state.nextAttempt = Math.min(value.nextAttempt, this.now() + 86400_000);
      }
    } catch {
      /* Missing or damaged cache is not quota. */
    }
    this.validateSaved(target, state);
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
  private persist(target: UsageFailoverTarget, state: State): void | Promise<void> {
    // A remote attempt may outlive a grant, including when that attempt fails.
    this.validateSaved(target, state);
    return writeUsageCache(this.file(target), {
      version: 1,
      key: this.key(target),
      identity: state.identity,
      primary: state.primary,
      saved: state.saved,
      nextAttempt: state.nextAttempt,
      fresh: state.fresh,
      freshUntil: state.freshUntil,
      primaryFailure: state.primaryFailure,
      primarySignIn: [...state.primarySignIn],
      observedPlan: state.observedPlan,
      observedPlanAt: state.observedPlanAt,
      observedPlanPeer: state.observedPlanPeer,
    });
  }
  private freshUntil(target: UsageFailoverTarget, refreshAfterSeconds = 0): number {
    const seconds =
      refreshAfterSeconds > 0
        ? Math.min(86400, refreshAfterSeconds)
        : readAccountRefreshSettings(target.ccsDir).refreshIntervalSeconds;
    return this.now() + seconds * 1000;
  }
  private async request(
    host: UsageHost,
    request: UsageHelperRequest,
    beforeStart?: () => boolean,
    observe?: (response: UsageHelperResponse) => void | Promise<void>
  ): Promise<UsageHelperResponse | null> {
    if (this.active >= 3) await new Promise<void>((resolve) => this.waiting.push(resolve));
    else this.active++;
    try {
      if (beforeStart && !beforeStart()) return null;
      // Every provider shares these three slots: a request whose completion is
      // lost must release its slot, or recovery stops for all of them.
      // The default is slightly after the transport's deadline; a shorter caller
      // budget must reach source loading and the command too.
      const deadlineAt =
        Date.now() +
        (this.deps.requestDeadlineMs?.(request.action) ??
          usageHelperDeadlineMs(request.action) + 1_000);
      const response = await withDeadline(
        this.deps.transport
          ? this.deps.transport(host, request)
          : runUsageHelper(host, request, deadlineAt),
        Math.max(0, deadlineAt - Date.now()),
        undefined,
        'Usage helper request'
      );
      // Publish identity cooldowns before a queued request receives this slot.
      if (observe) await observe(response);
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
        if (value.incompleteSelectors?.length)
          this.unreadSelectors.set(key, new Set(value.incompleteSelectors));
        else this.unreadSelectors.delete(key);
        return value.candidates;
      })
      .catch(() => null);
    this.inventories.set(key, { expires: this.now() + 15_000, pending });
    if (this.inventories.size > 256) {
      for (const [old, entry] of this.inventories)
        if (entry.expires <= this.now()) {
          this.inventories.delete(old);
          this.signInSelectors.delete(old);
          this.unreadSelectors.delete(old);
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
  /**
   * Cursor keeps one account and the desktop session may be on any computer
   * the dashboard already reads. A readable empty configured host is not
   * signed out while another computer has that session. Two different
   * accounts are left unsigned rather than attributed to the wrong one.
   * A computer that could not be read does not hide a session found elsewhere.
   * Candidates that computer did return still count, so a second account is
   * not ignored just because another store on that computer failed.
   * An unreadable configured computer is not a sign-out: its saved reading
   * stays, and a different account elsewhere is not adopted in its place.
   * An unreadable extra computer does not keep a signed-out row cached.
   */
  private async cursorSession(
    target: UsageFailoverTarget,
    state: State
  ): Promise<{
    chosen: { host: UsageHost; candidate: UsageCandidate } | null;
    conflict: boolean;
    configuredUnreadable: boolean;
  }> {
    const hosts = [
      ...target.primarySources,
      ...(await (this.deps.hosts ?? configuredUsageHosts)()),
    ].filter(
      (host, index, all) => all.findIndex((other) => hostKey(other) === hostKey(host)) === index
    );
    const selector = target.primarySources[0]?.selector ?? 'discover';
    const found: Array<{ host: UsageHost; candidate: UsageCandidate }> = [];
    let configuredUnreadable = false;
    for (const host of hosts) {
      const candidates = await this.inventory(target, host);
      this.applyInventorySignIn(target, state, host);
      const configured = target.primarySources.some((source) => hostKey(source) === hostKey(host));
      const gaps = this.unreadSelectors.get(this.inventoryKey(target, host));
      if (
        configured &&
        (candidates === null || gaps?.has('*') === true || gaps?.has(selector) === true)
      )
        configuredUnreadable = true;
      // A failed store does not erase sessions the same inventory already returned.
      if (candidates === null) continue;
      for (const candidate of candidates) {
        if (!candidate.selectors.includes(selector)) continue;
        if (candidateMatchesTarget(target, candidate)) found.push({ host, candidate });
      }
    }
    const byIdentity = new Map<string, { host: UsageHost; candidate: UsageCandidate }>();
    for (const item of found) {
      const id = JSON.stringify([item.candidate.identity.kind, item.candidate.identity.key]);
      if (!byIdentity.has(id)) byIdentity.set(id, item);
    }
    if (byIdentity.size > 1) return { chosen: null, conflict: true, configuredUnreadable };
    if (byIdentity.size === 1)
      return { chosen: [...byIdentity.values()][0], conflict: false, configuredUnreadable };
    return { chosen: null, conflict: false, configuredUnreadable };
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
    this.validateSaved(target, state);
    if (!state.saved || template.statusReason === 'identity_unbound') {
      if (
        target.provider === 'cursor' &&
        state.observedPlan &&
        state.identity &&
        template.statusReason !== 'identity_unbound'
      )
        template = { ...template, plan: state.observedPlan };
      return this.withSignIn(target, template, state);
    }
    const { reading, host, fallback } = state.saved;
    if (!emailMatches(target.expectedEmail, reading.email))
      return this.withSignIn(target, template, state);
    const refreshDue = state.fresh === true && this.now() >= state.freshUntil;
    const held = state.primaryFailure === 'rate_limited' && state.nextAttempt > this.now();
    const stale = cached || reading.status === 'cached' || refreshDue;
    const nativeMusePeer =
      target.provider === 'muse' &&
      !sameUsageIdentity(state.identity, state.saved.candidate.identity);
    const result: DashboardAccount = {
      ...template,
      email: template.email ?? reading.email,
      plan: reading.plan,
      // The reading's class replaces the template's outright: a class from
      // an earlier plan must not ride along under a new plan name.
      planClass: reading.planClass ?? undefined,
      sampledAt: reading.sampledAt,
      fetchedAt: reading.fetchedAt,
      windows: structuredClone(reading.windows),
      // Same outright replacement as the class: a plan detail from an
      // earlier reading must not ride along under new windows.
      antigravityPlan: reading.antigravityPlan
        ? structuredClone(reading.antigravityPlan)
        : undefined,
      status: stale ? 'cached' : 'ok',
      source: `Verified ${nativeMusePeer ? 'native subscription ' : ''}quota on ${HOST_LABELS[host]}${fallback ? ' (fallback)' : ''}`,
      cacheReason: stale
        ? refreshDue
          ? held
            ? 'rate_limited'
            : 'refresh_pending'
          : state.fresh === null
            ? 'outcome_unknown'
            : state.primaryFailure === 'rate_limited'
              ? 'rate_limited'
              : 'read_failed'
        : undefined,
      message: stale
        ? refreshDue
          ? held
            ? 'Usage refresh is rate limited; showing the last successful reading.'
            : 'The next usage read is pending; showing the last successful reading.'
          : state.fresh === null
            ? 'The previous usage read outcome was not recorded; showing the saved reading.'
            : state.primaryFailure === 'rate_limited'
              ? 'Usage refresh is rate limited; showing the last successful reading.'
              : 'The usage read failed or was skipped; showing the last successful reading.'
        : null,
      capabilities: { ...template.capabilities },
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
    if (template.provider !== 'claude') result.platform = helperPlatform(host);
    if (host !== result.platform) result.host = host;
    else delete result.host;
    return this.withSignIn(target, result, state);
  }
  /**
   * Claude only: another verified login for this same account (a Claude Code
   * login, or another Desktop profile) holds the saved reading. A Desktop
   * profile that is signed out is then not needed, so it is a quiet note and
   * never a sign-in prompt. A reading left over from the primary Desktop
   * profile itself does not count.
   */
  claudeLoginCovers(target: UsageFailoverTarget): boolean {
    return this.otherLoginWorks(target, this.state(target));
  }
  /**
   * Claude only, while the Desktop primary is signed out: the one account
   * identity whose saved login email (on any computer) is this account's
   * email. Two identities for one email are ambiguous and adopt nothing. The
   * read itself still checks the live profile names the same account.
   */
  private async claudeEmailPeer(
    target: UsageFailoverTarget,
    state: State,
    peerLists: () => Promise<Array<{ host: UsageHost; candidates: UsageCandidate[] | null }>>
  ): Promise<UsageIdentity | null> {
    if (target.provider !== 'claude' || !target.expectedEmail || state.primarySignIn.size === 0)
      return null;
    const found = new Map<string, UsageIdentity>();
    for (const { candidates } of await peerLists())
      for (const candidate of candidates ?? [])
        if (
          candidate.email &&
          emailMatches(target.expectedEmail, candidate.email) &&
          !target.primarySources.some((source) => candidate.selectors.includes(source.selector))
        )
          found.set(JSON.stringify([candidate.identity.kind, candidate.identity.key]), {
            ...candidate.identity,
          });
    return found.size === 1 ? [...found.values()][0] : null;
  }
  private otherLoginWorks(target: UsageFailoverTarget, state: State): boolean {
    const saved = state.saved;
    return (
      target.provider === 'claude' &&
      !!saved &&
      sameUsageIdentity(state.identity, saved.candidate.identity) &&
      emailMatches(target.expectedEmail, saved.reading.email) &&
      !target.primarySources.some((source) => saved.candidate.selectors.includes(source.selector))
    );
  }
  private withSignIn(
    target: UsageFailoverTarget,
    account: DashboardAccount,
    state: State
  ): DashboardAccount {
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
    const out = new Set<string>([...(account.signInNeeded ?? []), ...state.primarySignIn]);
    const signedOut = (['mac', 'windows'] as const).filter((host) => out.has(host));
    const result = projectClaudeFailoverExtras({ ...account }, this.now());
    delete result.signInNeeded;
    delete result.claudeDesktopSignedOut;
    if (signedOut.length && this.otherLoginWorks(target, state))
      result.claudeDesktopSignedOut = signedOut;
    else if (signedOut.length) result.signInNeeded = signedOut;
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
        Date.parse(template.sampledAt ?? '') > Date.parse(state.saved.reading.sampledAt ?? '') ||
        (template.status === 'cached' && template.sampledAt === state.saved.reading.sampledAt))
    )
      return this.withSignIn(target, template, state);
    return this.project(target, template, state, !state.fresh);
  }
  collect(target: UsageFailoverTarget, template: DashboardAccount): Promise<DashboardAccount> {
    return this.run(target, template, false);
  }
  /**
   * Every distinct credential identity the target's primary computer stores for its selector,
   * in store order, each marked `bound` when a candidate carrying one of `bound` selectors (an
   * account the registry already names, such as `aac-key:<id>`) holds the same identity.
   * `unread` lists the `bound` selectors whose stores could not be read this time: their keys
   * are unknown, so the caller must not treat them as free. It is an inventory of local stores
   * only: no provider is asked and no secret leaves the helper. Null when the computer's stores
   * could not be read at all.
   */
  async discoveredIdentities(
    target: UsageFailoverTarget,
    bound: string[]
  ): Promise<{
    found: Array<{ identity: UsageIdentity; bound: boolean }>;
    unread: string[];
  } | null> {
    const source = target.primarySources[0];
    if (!source) return null;
    const candidates = await this.inventory(target, source);
    if (candidates === null) return null;
    const gaps = this.unreadSelectors.get(this.inventoryKey(target, source));
    if (gaps?.has('*') || gaps?.has(source.selector)) return null;
    const unread = bound.filter((selector) => {
      const family = selector.includes(':')
        ? `${selector.slice(0, selector.indexOf(':'))}:*`
        : null;
      return gaps?.has(selector) === true || (family !== null && gaps?.has(family) === true);
    });
    const taken = candidates
      .filter((candidate) => candidate.selectors.some((selector) => bound.includes(selector)))
      .map((candidate) => candidate.identity);
    const found: Array<{ identity: UsageIdentity; bound: boolean }> = [];
    for (const candidate of candidates) {
      if (!candidate.selectors.includes(source.selector)) continue;
      if (found.some((item) => sameUsageIdentity(item.identity, candidate.identity))) continue;
      found.push({
        identity: candidate.identity,
        bound: taken.some((identity) => sameUsageIdentity(identity, candidate.identity)),
      });
    }
    return { found, unread };
  }
  /**
   * The credential identity the target's shown reading came from, or null when it has none
   * yet. Read-only: never loads, creates or changes state and never asks a provider.
   */
  readingIdentity(target: UsageFailoverTarget): UsageIdentity | null {
    return this.states.get(this.key(target))?.saved?.candidate.identity ?? null;
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
    state.fresh = false;
    state.primaryFailure = 'rate_limited';
    void this.persist(target, state);
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
        .finally(async () => {
          try {
            await this.persist(target, state);
          } finally {
            state.pending = null;
          }
        });
    }
    if (state.pending) await state.pending;
    if (
      hasUsage(template) &&
      template.status === 'ok' &&
      (!state.saved ||
        Date.parse(template.sampledAt ?? '') > Date.parse(state.saved.reading.sampledAt ?? '') ||
        (state.fresh &&
          this.now() < state.freshUntil &&
          template.sampledAt === state.saved.reading.sampledAt))
    )
      return this.withSignIn(target, template, state);
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
    // A process exit during I/O must not preserve the previous successful outcome.
    await this.persist(target, state);
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
        state.observedPlanAt = null;
        state.observedPlanPeer = null;
        return;
      }
      if (!candidate) {
        // A store that could not be read is an unavailable source, not a removed sign-in;
        // an unreadable store for another selector does not hide this one's removal.
        const unread = this.unreadSelectors.get(this.inventoryKey(target, source));
        const family = source.selector.includes(':')
          ? `${source.selector.slice(0, source.selector.indexOf(':'))}:*`
          : null;
        if (
          candidates !== null &&
          !unread?.has('*') &&
          !unread?.has(source.selector) &&
          !(family && unread?.has(family))
        )
          missingPrimary = true;
        continue;
      }
      if (!state.identity || !sameUsageIdentity(state.identity, candidate.identity)) {
        state.identity = candidate.identity;
        state.saved = null;
        state.observedPlan = null;
        state.observedPlanAt = null;
        state.observedPlanPeer = null;
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
    const signedOutPeer =
      !primaries.length && missingPrimary && !target.expectedIdentity
        ? await this.claudeEmailPeer(target, state, peerLists)
        : null;
    if (!primaries.length && target.provider === 'cursor' && !target.expectedIdentity) {
      const scan = await this.cursorSession(target, state);
      const cached = state.identity;
      const different =
        !!scan.chosen && !!cached && !sameUsageIdentity(cached, scan.chosen.candidate.identity);
      // The configured computer being down is not proof it signed out, so a
      // different account on another computer must not replace the saved one.
      if (scan.chosen && !(scan.configuredUnreadable && different)) {
        if (!sameUsageIdentity(cached, scan.chosen.candidate.identity)) {
          state.saved = null;
          state.observedPlan = null;
          state.observedPlanAt = null;
          state.observedPlanPeer = null;
        }
        state.identity = scan.chosen.candidate.identity;
        state.primary = scan.chosen.candidate;
        primaries.push(scan.chosen);
      } else if (scan.configuredUnreadable && cached) {
        // Keep the verified reading and let the known account refresh below.
      } else if (scan.conflict || !scan.configuredUnreadable) {
        state.identity = null;
        state.primary = null;
        state.saved = null;
        state.observedPlan = null;
        state.observedPlanAt = null;
        state.observedPlanPeer = null;
        return;
      } else if (!cached) return;
    } else if (signedOutPeer) {
      // A signed-out Desktop profile is not a removed account: its Claude Code login reads it.
      const peer = signedOutPeer;
      if (!sameUsageIdentity(state.identity, peer)) {
        state.saved = null;
        state.observedPlan = null;
        state.observedPlanAt = null;
        state.observedPlanPeer = null;
      }
      state.identity = peer;
    } else if (!primaries.length && missingPrimary && !target.expectedIdentity) {
      state.identity = null;
      state.primary = null;
      state.saved = null;
      state.observedPlan = null;
      state.observedPlanAt = null;
      state.observedPlanPeer = null;
      return;
    }
    if (!state.identity && state.primarySignIn.size > 0 && !target.expectedIdentity)
      state.identity = await this.claudeEmailPeer(target, state, peerLists);
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
        if (
          !state.saved ||
          Date.parse(reading.sampledAt ?? '') >= Date.parse(state.saved.reading.sampledAt ?? '')
        ) {
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
          if (state.fresh) state.freshUntil = this.freshUntil(target);
        }
        if (template.status === 'ok') {
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
      await settleUsageCooldown(target.ccsDir, cooldownKey);
      return;
    }
    const tried = new Set<string>();
    const attempt = async (
      host: UsageHost,
      candidate: UsageCandidate,
      fallback: boolean
    ): Promise<'done' | 'next'> => {
      if (
        !this.matchesIdentity(target, state.identity, candidate, host.host) ||
        !walletMatches(target, candidate) ||
        (candidate.email && !emailMatches(target.expectedEmail, candidate.email))
      )
        return 'next';
      // A linked peer is its own provider account too: honor and extend its own 429 hold.
      const peerKey = sameUsageIdentity(state.identity, candidate.identity)
        ? null
        : identityUsageCooldownKey(target.ccsDir, target.provider, candidate.identity);
      const peerHeld = (): boolean =>
        !!peerKey &&
        Math.max(
          this.cooldowns.get(peerKey) ?? 0,
          readUsageCooldown(target.ccsDir, peerKey, this.now())
        ) > this.now();
      if (peerHeld()) return 'next';
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
          // Recheck both holds when a queued request finally starts.
          () => !applyCooldown() && !peerHeld(),
          async (value) => {
            if (value.action !== 'collect' || !responseIsRateLimited(value)) return;
            const now = this.now();
            const providerUntil =
              now + Math.max(60, Math.min(86400, value.retryAfterSeconds ?? 60)) * 1000;
            let until = providerUntil;
            let streak: number | undefined;
            if (target.claudeDesktopCadence) {
              const previous = readUsageCooldownState(target.ccsDir, cooldownKey, now);
              streak = Math.min((previous.claude429Streak ?? 0) + 1, MAX_CLAUDE_429_STREAK);
              until =
                now + claudeDesktopDelayMs(this.key(target), (providerUntil - now) / 1000, streak);
              state.primaryFailure = 'rate_limited';
            }
            state.nextAttempt = holdUsageCooldown(target.ccsDir, cooldownKey, until, now, streak);
            this.cooldowns.set(cooldownKey, state.nextAttempt);
            if (peerKey)
              this.cooldowns.set(
                peerKey,
                holdUsageCooldown(target.ccsDir, peerKey, until, this.now())
              );
            await Promise.all([
              settleUsageCooldown(target.ccsDir, cooldownKey),
              peerKey ? settleUsageCooldown(target.ccsDir, peerKey) : undefined,
            ]);
          }
        );
      } catch {
        return 'next';
      }
      // Held account: stop. Held linked peer only: other computers may still answer.
      if (response === null) return applyCooldown() ? 'done' : 'next';
      if (response.action !== 'collect') return 'next';
      // The computer whose Cursor session was adopted is the source for this
      // read, even when it is not the configured host.
      const adoptedCursor =
        target.provider === 'cursor' &&
        primaries.some(
          (primary) =>
            hostKey(primary.host) === hostKey(host) &&
            primary.candidate.locator === candidate.locator &&
            sameUsageIdentity(primary.candidate.identity, candidate.identity)
        );
      const primarySource =
        adoptedCursor ||
        target.primarySources.some(
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
      const blocked = responseIsRateLimited(response);
      // identity_unavailable means the provider did not name the signed-in account this time.
      // It proves no change of account, so the verified identity keeps its other computers.
      if (
        primarySource &&
        [
          'identity_mismatch',
          'account_mismatch',
          'source_changed',
          'plan_mismatch',
          'team_mismatch',
        ].includes(response.failureCode ?? '')
      ) {
        state.identity = target.expectedIdentity ?? null;
        state.primary = null;
        state.saved = null;
        state.observedPlan = null;
        state.observedPlanAt = null;
        state.observedPlanPeer = null;
        return 'done';
      }
      if (
        !response.candidate ||
        response.candidate.locator !== candidate.locator ||
        response.candidate.revision !== candidate.revision ||
        !sameUsageIdentity(candidate.identity, response.candidate.identity)
      )
        return blocked ? 'done' : 'next';
      // A link can be removed while a remote request is in flight.
      if (!this.matchesIdentity(target, state.identity, candidate, host.host)) return 'next';
      const observedPlan = safeUsageText(response.observedPlan);
      const observedPlanAt = usageTimestamp(response.observedPlanAt, this.now());
      if (
        tracksVerifiedPlan(target) &&
        observedPlan &&
        observedPlanAt &&
        Date.parse(observedPlanAt) >=
          Math.max(
            Date.parse(state.observedPlanAt ?? '') || 0,
            Date.parse(state.saved?.reading.sampledAt ?? '') || 0
          )
      ) {
        state.observedPlan = observedPlan;
        state.observedPlanAt = observedPlanAt;
        state.observedPlanPeer = sameUsageIdentity(state.identity, candidate.identity)
          ? null
          : { host: host.host, identity: candidate.identity };
        if (state.saved && state.saved.reading.plan !== state.observedPlan) state.saved = null;
      }
      const reading = parseUsageReading(response.sample, this.now());
      if (!hasUsage(reading) || !emailMatches(target.expectedEmail, reading.email))
        return blocked ? 'done' : 'next';
      if (tracksVerifiedPlan(target)) {
        const sampled = Date.parse(reading.sampledAt ?? '') || 0;
        const knownPlanAt = Date.parse(state.observedPlanAt ?? '') || 0;
        if (
          reading.status === 'ok' &&
          sampled >= knownPlanAt &&
          sampled >= (Date.parse(state.saved?.reading.sampledAt ?? '') || 0)
        ) {
          state.observedPlan = reading.plan;
          state.observedPlanAt = reading.sampledAt;
          state.observedPlanPeer = sameUsageIdentity(state.identity, candidate.identity)
            ? null
            : { host: host.host, identity: candidate.identity };
        } else if (state.observedPlan && reading.plan !== state.observedPlan)
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
        if (state.fresh)
          state.freshUntil = this.freshUntil(
            target,
            response.failureCode === null ? response.refreshAfterSeconds : 0
          );
      }
      if (blocked) return 'done';
      if (target.claudeDesktopCadence && reading.status === 'ok' && response.failureCode === null) {
        const now = this.now();
        const cooldown = readUsageCooldownState(target.ccsDir, cooldownKey, now);
        // One clean read lowers a 429 streak one level: the next read keeps the
        // slower pace, so a token shared with other clients stops re-tripping 429.
        const streak = Math.max(0, (cooldown.claude429Streak ?? 0) - 1);
        const floor = Math.max(
          readAccountRefreshSettings(target.ccsDir).refreshIntervalSeconds,
          response.refreshAfterSeconds ?? 0
        );
        state.nextAttempt = now + claudeDesktopDelayMs(this.key(target), floor, streak);
        if (cooldown.claude429Streak) {
          this.cooldowns.set(
            cooldownKey,
            holdUsageCooldown(
              target.ccsDir,
              cooldownKey,
              streak ? state.nextAttempt : now,
              now,
              streak
            )
          );
          await settleUsageCooldown(target.ccsDir, cooldownKey);
        }
        // A stepped-down pace is not a fresh reading: the row goes cached once a
        // normal refresh interval has passed, whatever the slower hold allows.
        if (state.fresh)
          state.freshUntil = cooldown.claude429Streak
            ? Math.min(
                state.nextAttempt,
                now + readAccountRefreshSettings(target.ccsDir).refreshIntervalSeconds * 1000
              )
            : state.nextAttempt;
        return 'done';
      }
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
