import type {
  DashboardAccount,
  DashboardAccountStatus,
  DashboardAccountWindow,
  DashboardPlatform,
  DashboardProvider,
} from '../services/account-dashboard-types';

/**
 * Dashboard rows -> the agent usage answer. Every output object is built field by field from an
 * allowlist, never spread from a row, so nothing a collector attached (paths, messages, sources,
 * profile names, credentials) reaches an agent. Readings are the dashboard's own cached ones: the
 * same percentages, amounts and reset times, with their sample time, and nothing invented when a
 * provider gave no number.
 */
export const AGENT_USAGE_VERSION = 1;

export interface AgentUsageWindow {
  key: string;
  label: string;
  /** rate_limit: a quota window; balance, spend and extra_usage: amounts, often without a percent. */
  kind: 'rate_limit' | 'balance' | 'spend' | 'extra_usage';
  usedPercent: number | null;
  remainingPercent: number | null;
  resetAt: string | null;
  /** The reset already passed after this reading: the numbers are history until a new reading. */
  resetPassed: boolean;
  windowMinutes: number | null;
  used: number | null;
  limit: number | null;
  remaining: number | null;
  unit: string | null;
  expiresAt: string | null;
  unlimited: boolean | null;
  /** The provider's shared model pool, when it names one (for example Cursor's Grok pool). */
  pool: string | null;
  modelIds: string[];
}

export interface AgentUsageAccount {
  /** The dashboard's account id, stable across snapshots. */
  id: string;
  provider: DashboardProvider;
  providerLabel: string;
  /** The account's name on the dashboard. */
  label: string;
  /** The signed-in provider account: the surest way to tell which login this is. */
  email: string | null;
  /** AAC's saved profile name for this login (Codex, Claude, Antigravity), when it has one. */
  profile: string | null;
  /** Where AAC reads it, for example "Codex usage on Ubuntu". */
  source: string | null;
  plan: string | null;
  platform: DashboardPlatform;
  /** 'nas1' for a source that runs on Nas1 (reported as platform ubuntu). */
  host: DashboardPlatform | 'nas1';
  /** The account this provider currently uses on its computer. */
  active: boolean;
  /** ok: fresh; cached: an earlier verified reading; the rest carry no usable reading. */
  status: DashboardAccountStatus;
  sampledAt: string | null;
  ageSeconds: number | null;
  /** Hidden on the dashboard by its owner; still collected. */
  hidden: boolean;
  windows: AgentUsageWindow[];
}

export interface AgentUsageAnswer {
  version: typeof AGENT_USAGE_VERSION;
  generatedAt: string;
  accounts: AgentUsageAccount[];
}

const PROVIDERS: readonly DashboardProvider[] = [
  'codex',
  'claude',
  'antigravity',
  'muse',
  'cursor',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
];
const STATUSES: readonly DashboardAccountStatus[] = [
  'ok',
  'cached',
  'unavailable',
  'error',
  'needs_sign_in',
];
const PLATFORMS: readonly DashboardPlatform[] = ['ubuntu', 'mac', 'windows'];
const KINDS = ['rate_limit', 'balance', 'spend', 'extra_usage'] as const;
const CONTROL = /[\u0000-\u001f\u007f]/;
const MODEL_ID = /^[A-Za-z0-9][A-Za-z0-9_.:/@-]{0,159}$/;

function text(value: unknown, max: number): string | null {
  return typeof value === 'string' &&
    value.length > 0 &&
    value.length <= max &&
    !CONTROL.test(value)
    ? value
    : null;
}

function finite(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function iso(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? new Date(parsed).toISOString() : null;
}

/** Profile names are short identifiers; anything else (a path, free text) is left out. */
const PROFILE = /^[A-Za-z0-9][A-Za-z0-9_.@-]{0,63}$/;

function profileOf(account: DashboardAccount): string | null {
  const capabilities = account.capabilities ?? ({} as DashboardAccount['capabilities']);
  for (const value of [
    capabilities.codexProfile,
    capabilities.claudeProfileId,
    capabilities.antigravityProfileId,
  ])
    if (typeof value === 'string' && PROFILE.test(value)) return value;
  return null;
}

function sourceOf(account: DashboardAccount): string | null {
  const value = text(account.source, 120);
  return value && !/[\\/]/.test(value) ? value : null;
}

function windowOf(
  window: DashboardAccountWindow,
  now: number,
  accountSampledAt: string | null
): AgentUsageWindow | null {
  const key = text(window.key, 64);
  const label = text(window.label, 80);
  if (!key || !label) return null;
  const models = Array.isArray(window.modelIds)
    ? window.modelIds.filter((id) => typeof id === 'string' && MODEL_ID.test(id)).slice(0, 64)
    : [];
  return {
    key,
    label,
    kind: KINDS.includes(window.kind as (typeof KINDS)[number])
      ? (window.kind as AgentUsageWindow['kind'])
      : 'rate_limit',
    usedPercent: finite(window.usedPercent),
    remainingPercent: finite(window.remainingPercent),
    resetAt: iso(window.resetAt),
    resetPassed: window.resetPassed === true || resetPassedAt(window, now, accountSampledAt),
    windowMinutes: finite(window.windowMinutes),
    used: finite(window.used),
    limit: finite(window.limit),
    remaining: finite(window.remaining),
    unit: text(window.unit, 40),
    expiresAt: iso(window.expiresAt),
    unlimited: typeof window.unlimited === 'boolean' ? window.unlimited : null,
    pool: text(window.poolLabel, 80),
    modelIds: models,
  };
}

/** The dashboard's rule: the reset is due and the reading predates it (or its time is unknown). */
function resetPassedAt(
  window: DashboardAccountWindow,
  now: number,
  accountSampledAt: string | null
): boolean {
  const reset = Date.parse(iso(window.resetAt) ?? '');
  if (!Number.isFinite(reset) || reset > now) return false;
  const sampled = Date.parse(iso(window.sampledAt) ?? accountSampledAt ?? '');
  return !Number.isFinite(sampled) || sampled < reset;
}

function accountOf(account: DashboardAccount, now: number): AgentUsageAccount | null {
  const id = text(account.id, 200);
  const label = text(account.label, 120);
  const providerLabel = text(account.providerLabel, 60);
  if (!id || !label || !providerLabel) return null;
  if (!PROVIDERS.includes(account.provider) || !PLATFORMS.includes(account.platform)) return null;
  const sampledAt = iso(account.sampledAt);
  const age =
    sampledAt === null ? null : Math.max(0, Math.floor((now - Date.parse(sampledAt)) / 1000));
  return {
    id,
    provider: account.provider,
    providerLabel,
    label,
    email: text(account.email, 254),
    profile: profileOf(account),
    source: sourceOf(account),
    plan: text(account.plan, 80),
    platform: account.platform,
    host: account.host === 'nas1' ? 'nas1' : account.platform,
    active: account.isActive === true,
    status: STATUSES.includes(account.status) ? account.status : 'unavailable',
    sampledAt,
    ageSeconds: age,
    hidden: account.hidden === true,
    windows: (account.windows ?? [])
      .map((window) => windowOf(window, now, sampledAt))
      .filter((window): window is AgentUsageWindow => window !== null),
  };
}

function newer(left: AgentUsageAccount, right: AgentUsageAccount): boolean {
  return Date.parse(left.sampledAt ?? '') > Date.parse(right.sampledAt ?? '');
}

/**
 * One answer from dashboard rows. A row listed twice (once per Claude desktop view) appears once,
 * with its newest reading. Accounts keep provider order, then row order; `provider` keeps only
 * that provider's rows.
 */
export function projectAgentUsage(
  rows: DashboardAccount[],
  options: { now: number; provider?: DashboardProvider; includeHidden?: boolean }
): AgentUsageAnswer {
  const byId = new Map<string, AgentUsageAccount>();
  for (const row of rows) {
    const account = accountOf(row, options.now);
    if (!account) continue;
    if (options.provider && account.provider !== options.provider) continue;
    if (account.hidden && options.includeHidden !== true) continue;
    const existing = byId.get(account.id);
    if (!existing || newer(account, existing)) byId.set(account.id, account);
  }
  const accounts = [...byId.values()].sort(
    (a, b) => PROVIDERS.indexOf(a.provider) - PROVIDERS.indexOf(b.provider)
  );
  return {
    version: AGENT_USAGE_VERSION,
    generatedAt: new Date(options.now).toISOString(),
    accounts,
  };
}

export function isAgentUsageProvider(value: string): value is DashboardProvider {
  return (PROVIDERS as readonly string[]).includes(value);
}
