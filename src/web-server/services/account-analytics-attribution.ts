import type { HourlyUsage, ModelBreakdown, SessionUsage } from '../usage/types';
import type { UsageWorkerResult } from '../usage/worker-client';
import type { DashboardProvider } from './account-dashboard-types';
import type {
  AccountAnalyticsActivityProvider,
  AccountAnalyticsUsageProvider,
} from './account-analytics-types';

/**
 * Usage is grouped under the dashboard provider whose subscription or route served it, not under the tool that
 * logged it. OMP records the route of every call (`message.provider`) and zcode records one per row
 * (`model_usage.provider_id`, for example `builtin:zai-coding-plan`); both reach the projection as the model
 * breakdown's `provider`, lower-cased by the usage readers. This is the one table that maps a route to a provider.
 * A route it does not name (a local vLLM server, OpenRouter, a test endpoint) is "other", never a guess.
 */
export const USAGE_ROUTE_PROVIDERS: Readonly<Record<string, DashboardProvider>> = Object.freeze({
  anthropic: 'claude',
  claude: 'claude',
  openai: 'codex',
  codex: 'codex',
  'alibaba-token-plan': 'qwen',
  zai: 'zai',
  'zai-coding-plan': 'zai',
  'kimi-code': 'kimi-code',
  'opencode-go': 'opencode-go',
  // OpenCode Zen is the balance of the OpenCode console wallet, which the dashboard shows as OpenCode Go.
  'opencode-zen': 'opencode-go',
  cursor: 'cursor',
  'muse-code': 'muse',
  'google-antigravity': 'antigravity',
});

/**
 * The tools whose logs are one provider's usage by construction: Claude Code (Anthropic), Codex (OpenAI) and the
 * Muse Code CLI (Muse Code). OMP and zcode log a route per call instead.
 */
const TOOL_PROVIDERS: Readonly<
  Partial<Record<AccountAnalyticsActivityProvider, DashboardProvider>>
> = Object.freeze({ claude: 'claude', codex: 'codex', muse: 'muse' });

/** The provider order of the dashboard (Claude, Codex, then the additional providers), then "other". */
export const USAGE_PROVIDER_ORDER: readonly AccountAnalyticsUsageProvider[] = [
  'claude',
  'codex',
  'antigravity',
  'muse',
  'cursor',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
  'other',
];

/** The dashboard provider a route string names, or null when the table does not name it. */
export function routeProvider(route: string | null | undefined): DashboardProvider | null {
  if (typeof route !== 'string') return null;
  let key = route.trim().toLowerCase();
  // zcode prefixes its built-in routes ("builtin:zai-coding-plan").
  if (key.startsWith('builtin:')) key = key.slice('builtin:'.length);
  return Object.prototype.hasOwnProperty.call(USAGE_ROUTE_PROVIDERS, key)
    ? USAGE_ROUTE_PROVIDERS[key]
    : null;
}

/**
 * The provider one model breakdown counts under: the tool's own provider for Claude Code, Codex and the Muse Code
 * CLI; otherwise the logged route; with no route, the model id's provider prefix ("zai/glm-5"); else "other".
 */
export function usageProviderFor(
  tool: AccountAnalyticsActivityProvider,
  route: string | null | undefined,
  model: string | null | undefined
): AccountAnalyticsUsageProvider {
  const own = TOOL_PROVIDERS[tool];
  if (own) return own;
  if (typeof route === 'string' && route.trim()) return routeProvider(route) ?? 'other';
  const name = typeof model === 'string' ? model.trim() : '';
  const slash = name.indexOf('/');
  if (slash > 0) return routeProvider(name.slice(0, slash)) ?? 'other';
  return 'other';
}

/** One tool's rows for one provider: hourly parts, and session parts with the whole session they came from. */
export interface AttributedResult {
  tool: AccountAnalyticsActivityProvider;
  hourly: HourlyUsage[];
  session: Array<{ part: SessionUsage; whole: SessionUsage; dominant: boolean }>;
}

/** All retained usage that one provider served, across tools and hosts. */
export interface AttributedSource {
  provider: AccountAnalyticsUsageProvider;
  fetchedAt: string;
  data: AttributedResult[];
}

const TOKEN_FIELDS = [
  'inputTokens',
  'outputTokens',
  'cacheCreationTokens',
  'cacheReadTokens',
] as const;

function count(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : 0;
}
function tokensOf(row: Pick<ModelBreakdown, (typeof TOKEN_FIELDS)[number]>): number {
  return TOKEN_FIELDS.reduce((sum, field) => sum + count(row[field]), 0);
}

/**
 * Divide one hourly or session row by provider. A row whose breakdowns all count under one provider is kept as
 * it is (its request count and any unnamed residual included). A mixed row becomes one part per provider, each
 * summing only its own breakdowns; a part's request count is known only when its breakdowns carry one. Tokens
 * that no breakdown names stay with the tool's own provider, or "other", with no request count.
 */
function splitRow<T extends HourlyUsage | SessionUsage>(
  row: T,
  tool: AccountAnalyticsActivityProvider
): Array<[AccountAnalyticsUsageProvider, T]> {
  const groups = new Map<AccountAnalyticsUsageProvider, ModelBreakdown[]>();
  for (const breakdown of Array.isArray(row.modelBreakdowns) ? row.modelBreakdowns : []) {
    if (typeof breakdown !== 'object' || breakdown === null) continue;
    const provider = usageProviderFor(tool, breakdown.provider, breakdown.modelName);
    const list = groups.get(provider) ?? [];
    list.push(breakdown);
    groups.set(provider, list);
  }
  const home = TOOL_PROVIDERS[tool] ?? 'other';
  if (groups.size === 0) return [[home, row]];
  if (groups.size === 1) return [[[...groups.keys()][0], row]];
  const parts: Array<[AccountAnalyticsUsageProvider, T]> = [];
  const covered = { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0 };
  let coveredCost = 0;
  let coveredFallback = 0;
  for (const [provider, list] of groups) {
    const sum = (field: (typeof TOKEN_FIELDS)[number]) =>
      list.reduce((total, item) => total + count(item[field]), 0);
    const cost = list.reduce((total, item) => total + count(item.cost), 0);
    const fallbackCost = list.reduce((total, item) => total + count(item.fallbackCost), 0);
    const counted = list.every((item) => typeof item.requestCount === 'number');
    const part = {
      ...row,
      inputTokens: sum('inputTokens'),
      outputTokens: sum('outputTokens'),
      cacheCreationTokens: sum('cacheCreationTokens'),
      cacheReadTokens: sum('cacheReadTokens'),
      cost,
      totalCost: cost,
      modelsUsed: [...new Set(list.map((item) => item.modelName))],
      modelBreakdowns: list,
    } as T;
    if (fallbackCost > 0) part.fallbackCost = fallbackCost;
    else delete part.fallbackCost;
    if ('hour' in part) {
      const hourly = part as HourlyUsage;
      if (counted)
        hourly.requestCount = list.reduce((total, item) => total + count(item.requestCount), 0);
      else delete hourly.requestCount;
    }
    for (const field of TOKEN_FIELDS) covered[field] += part[field];
    coveredCost += cost;
    coveredFallback += fallbackCost;
    parts.push([provider, part]);
  }
  const residual = {
    inputTokens: Math.max(0, count(row.inputTokens) - covered.inputTokens),
    outputTokens: Math.max(0, count(row.outputTokens) - covered.outputTokens),
    cacheCreationTokens: Math.max(0, count(row.cacheCreationTokens) - covered.cacheCreationTokens),
    cacheReadTokens: Math.max(0, count(row.cacheReadTokens) - covered.cacheReadTokens),
  };
  if (tokensOf(residual) > 0) {
    const cost = Math.max(0, count(row.totalCost ?? row.cost) - coveredCost);
    const part = {
      ...row,
      ...residual,
      cost,
      totalCost: cost,
      modelsUsed: [],
      modelBreakdowns: [],
    } as T;
    const fallback = Math.max(0, count(row.fallbackCost) - coveredFallback);
    if (fallback > 0) part.fallbackCost = fallback;
    else delete part.fallbackCost;
    delete (part as HourlyUsage).requestCount;
    parts.push([home, part]);
  }
  return parts;
}

/**
 * Regroup the retained tool snapshots by provider, in the dashboard's provider order. Hosts are already merged
 * per tool; nothing is attributed to an account. A session counts under every provider that served it, and is
 * the sample candidate only under the provider that served most of its tokens.
 */
export function attributeActivitySources(
  sources: Array<{
    provider: AccountAnalyticsActivityProvider;
    data: Array<Pick<UsageWorkerResult, 'hourly' | 'session'>>;
    fetchedAt: string;
  }>
): AttributedSource[] {
  const byProvider = new Map<AccountAnalyticsUsageProvider, AttributedSource>();
  const resultFor = (
    provider: AccountAnalyticsUsageProvider,
    fetchedAt: string,
    tool: AccountAnalyticsActivityProvider,
    seen: Map<AccountAnalyticsUsageProvider, AttributedResult>
  ): AttributedResult => {
    let result = seen.get(provider);
    if (!result) {
      result = { tool, hourly: [], session: [] };
      seen.set(provider, result);
      const source = byProvider.get(provider) ?? { provider, fetchedAt, data: [] };
      if (fetchedAt < source.fetchedAt) source.fetchedAt = fetchedAt;
      source.data.push(result);
      byProvider.set(provider, source);
    }
    return result;
  };
  for (const source of sources) {
    for (const data of source.data) {
      const seen = new Map<AccountAnalyticsUsageProvider, AttributedResult>();
      for (const hour of data.hourly)
        for (const [provider, part] of splitRow(hour, source.provider))
          resultFor(provider, source.fetchedAt, source.provider, seen).hourly.push(part);
      for (const session of data.session) {
        const parts = splitRow(session, source.provider);
        const dominant = parts.reduce(
          (best, item, index) => (tokensOf(item[1]) > tokensOf(parts[best][1]) ? index : best),
          0
        );
        parts.forEach(([provider, part], index) =>
          resultFor(provider, source.fetchedAt, source.provider, seen).session.push({
            part,
            whole: session,
            dominant: index === dominant,
          })
        );
      }
    }
  }
  const ordered = USAGE_PROVIDER_ORDER.filter((provider) => byProvider.has(provider));
  const rest = [...byProvider.keys()].filter((provider) => !ordered.includes(provider)).sort();
  return [...ordered, ...rest].map((provider) => byProvider.get(provider) as AttributedSource);
}
