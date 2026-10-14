import type {
  DashboardAccount,
  DashboardAccountWindow,
  DashboardPlatform,
  DashboardProvider,
} from './account-dashboard-types';
import type { DashboardHost } from './dashboard-hosts';
import { analyticsWindow } from './account-analytics-history';
import { antigravityPlanDisplay } from '../../antigravity/plan';

/** Private collector protocol. None of these identity/credential digests enter an HTTP DTO. */
export type FailoverProvider = DashboardProvider | 'opencode-wallet';
export const FAILOVER_PROVIDERS: readonly FailoverProvider[] = [
  'codex',
  'claude',
  'antigravity',
  'muse',
  'cursor',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
  'opencode-wallet',
];
export interface UsageIdentity {
  kind: string;
  key: string;
}
export interface UsageCandidate {
  locator: string;
  selectors: string[];
  identity: UsageIdentity;
  revision: string;
  email: string | null;
  plan: string | null;
}
export interface UsageHost {
  host: DashboardHost;
  platform: DashboardPlatform;
  /** null is the dashboard computer; aliases come only from private server configuration. */
  sshHost: string | null;
}
export interface UsagePrimarySource extends UsageHost {
  selector: string;
  /** Private server-owned launcher context, never accepted from an HTTP usage request. */
  claudeProfile?: UsageClaudeProfile;
}
export interface UsageClaudeProfile {
  id: string;
  email: string;
  profilePath?: string;
  isDefault?: boolean;
}
export interface UsageFailoverTarget {
  accountId: string;
  provider: FailoverProvider;
  ccsDir: string;
  /** Full account/source configuration, including identity/revision where the registry supplies it. */
  binding: string;
  primarySources: UsagePrimarySource[];
  expectedEmail?: string | null;
  expectedIdentity?: UsageIdentity | null;
}
export interface UsageReading {
  status: 'ok' | 'cached' | 'unavailable' | 'error' | 'needs_sign_in';
  email: string | null;
  plan: string | null;
  sampledAt: string | null;
  fetchedAt: string | null;
  windows: DashboardAccountWindow[];
  antigravityPlan?: DashboardAccount['antigravityPlan'];
  /** Private optional-Claude cache binding; never expose in a dashboard DTO. */
  optionalExtrasContext?: string;
  optionalExtras?: {
    resetCredits: 'ok' | 'unavailable';
    prepaidBalance: 'ok' | 'unavailable';
  };
}
export const USAGE_FAILURE_CODES = new Set([
  'unavailable',
  'not_installed',
  'needs_sign_in',
  'identity_mismatch',
  'account_mismatch',
  'source_changed',
  'rate_limited',
  'invalid_response',
  'network_error',
  'provider_error',
  'error',
  'busy',
  'no_browser_cookie',
  'local_storage_error',
  'identity_unavailable',
  'plan_mismatch',
  'team_mismatch',
  'choose_team',
  'inactive_subscription',
]);
export interface UsageCollection {
  sample: UsageReading | null;
  failureCode: string | null;
  retryAfterSeconds?: number;
  /** Healthy source-local caching; no provider request is due before this interval. */
  refreshAfterSeconds?: number;
  /** Verified Muse plan even when quota failed; never part of a dashboard row. */
  observedPlan?: string;
}
export type UsageHelperRequest =
  | {
      version: 1;
      action: 'inventory';
      provider: FailoverProvider;
      ccsDir?: string;
      homeDir?: string;
      claudeProfile?: UsageClaudeProfile;
    }
  | {
      version: 1;
      action: 'collect';
      provider: FailoverProvider;
      candidate: UsageCandidate;
      ccsDir?: string;
      homeDir?: string;
      claudeProfile?: UsageClaudeProfile;
    };
export type UsageHelperResponse =
  | {
      version: 1;
      action: 'inventory';
      candidates: UsageCandidate[];
      /** Claude desktop signed-out slots have no credential candidate to collect. */
      signInNeededSelectors?: string[];
    }
  | ({ version: 1; action: 'collect'; candidate: UsageCandidate | null } & UsageCollection);

export const HEX_DIGEST = /^[a-f0-9]{64}$/;
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
export function safeUsageText(value: unknown, max = 160): string | null {
  return typeof value === 'string' &&
    value.trim() &&
    value.length <= max &&
    !/[\u0000-\u001f\u007f]|(?:bearer\s|sk[-_]|dca:|eyJ[A-Za-z0-9_-]{8}|access_token|refresh_token|https?:\/\/)/i.test(
      value
    )
    ? value.trim()
    : null;
}
export function usageEmail(value: unknown): string | null {
  const text = safeUsageText(value, 254);
  return text && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(text) ? text : null;
}
export function usageTimestamp(value: unknown, now = Date.now()): string | null {
  if (typeof value !== 'string' || value.length > 64 || !/^\d{4}-\d{2}-\d{2}T/.test(value))
    return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) && parsed >= Date.UTC(2000, 0, 1) && parsed <= now + 300_000
    ? new Date(parsed).toISOString()
    : null;
}
export function parseUsageIdentity(value: unknown): UsageIdentity | null {
  return isRecord(value) &&
    Object.keys(value).sort().join(',') === 'key,kind' &&
    typeof value.kind === 'string' &&
    /^[a-z][a-z0-9-]{0,63}$/.test(value.kind) &&
    typeof value.key === 'string' &&
    HEX_DIGEST.test(value.key)
    ? { kind: value.kind, key: value.key }
    : null;
}
export function sameUsageIdentity(
  a: UsageIdentity | null | undefined,
  b: UsageIdentity | null | undefined
): boolean {
  return !!a && !!b && a.kind === b.kind && a.key === b.key;
}
export function parseUsageCandidate(value: unknown): UsageCandidate | null {
  if (
    !isRecord(value) ||
    Object.keys(value).some(
      (key) => !['locator', 'selectors', 'identity', 'revision', 'email', 'plan'].includes(key)
    )
  )
    return null;
  const identity = parseUsageIdentity(value.identity);
  if (
    !identity ||
    typeof value.locator !== 'string' ||
    !HEX_DIGEST.test(value.locator) ||
    typeof value.revision !== 'string' ||
    !HEX_DIGEST.test(value.revision) ||
    !Array.isArray(value.selectors) ||
    value.selectors.length < 1 ||
    value.selectors.length > 32 ||
    value.selectors.some(
      (s) => typeof s !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9:_.@-]{0,159}$/.test(s)
    ) ||
    (value.email !== null && usageEmail(value.email) === null) ||
    (value.plan !== null && safeUsageText(value.plan) === null)
  )
    return null;
  return {
    locator: value.locator,
    selectors: [...new Set(value.selectors as string[])],
    identity,
    revision: value.revision,
    email: usageEmail(value.email),
    plan: safeUsageText(value.plan),
  };
}
export function parseUsageReading(value: unknown, now = Date.now()): UsageReading | null {
  if (
    !isRecord(value) ||
    !['ok', 'cached', 'unavailable', 'error', 'needs_sign_in'].includes(String(value.status)) ||
    !Array.isArray(value.windows) ||
    value.windows.length > 256
  )
    return null;
  const sampledAt = usageTimestamp(value.sampledAt, now);
  const fetchedAt = usageTimestamp(value.fetchedAt, now);
  const successful = value.status === 'ok' || value.status === 'cached';
  if (
    successful &&
    (!sampledAt || !fetchedAt || Date.parse(sampledAt) > Date.parse(fetchedAt) + 300_000)
  )
    return null;
  const windows: DashboardAccountWindow[] = [];
  const keys = new Set<string>();
  for (const raw of value.windows) {
    if (
      !isRecord(raw) ||
      !safeUsageText(raw.key, 64) ||
      !safeUsageText(raw.label, 80) ||
      keys.has(String(raw.key))
    )
      return null;
    const window = analyticsWindow(raw);
    if (!window) return null;
    if (
      [window.usedPercent, window.used, window.limit, window.windowMinutes].some(
        (n) => n !== null && n < 0
      )
    )
      return null;
    if (
      window.remainingPercent !== null &&
      (window.remainingPercent < 0 || window.remainingPercent > 100)
    )
      return null;
    if (window.unit !== null && !safeUsageText(window.unit, 40)) return null;
    keys.add(window.key);
    windows.push(window);
  }
  const result: UsageReading = {
    status: value.status as UsageReading['status'],
    email: usageEmail(value.email),
    plan: safeUsageText(value.plan),
    sampledAt,
    fetchedAt,
    windows,
  };
  if (
    typeof value.optionalExtrasContext === 'string' &&
    HEX_DIGEST.test(value.optionalExtrasContext)
  )
    result.optionalExtrasContext = value.optionalExtrasContext;
  const extras = value.optionalExtras;
  if (
    isRecord(extras) &&
    (extras.resetCredits === 'ok' || extras.resetCredits === 'unavailable') &&
    (extras.prepaidBalance === 'ok' || extras.prepaidBalance === 'unavailable')
  )
    result.optionalExtras = {
      resetCredits: extras.resetCredits,
      prepaidBalance: extras.prepaidBalance,
    };
  // The standard Antigravity validator rebuilds its entitlement DTO. Never copy helper extras.
  if (isRecord(value.reportedPlan)) {
    const display = antigravityPlanDisplay(
      result.plan,
      sampledAt ?? fetchedAt ?? now,
      value.reportedPlan
    );
    result.plan = display.plan;
    if (display.antigravityPlan) result.antigravityPlan = display.antigravityPlan;
  } else if (isRecord(value.antigravityPlan)) {
    const display = antigravityPlanDisplay(result.plan, sampledAt ?? fetchedAt ?? now);
    if (display.antigravityPlan) result.antigravityPlan = display.antigravityPlan;
  }
  return result;
}
export function hasUsage(reading: UsageReading | DashboardAccount | null): reading is UsageReading {
  return (
    !!reading &&
    (reading.status === 'ok' || reading.status === 'cached') &&
    reading.windows.length > 0 &&
    reading.sampledAt !== null
  );
}
