export const NATIVE_HOST = 'com.ccs.qwen_usage_bridge';
export const CONSOLE_URLS = Object.freeze({
  intl: ['https://home.qwencloud.com/', 'https://cs-data.qwencloud.com/'],
  cn: ['https://bailian.console.aliyun.com/', 'https://bailian-cs.console.aliyun.com/'],
});
export const COOKIE_DOMAINS = Object.freeze({
  intl: ['qwencloud.com', 'home.qwencloud.com', 'cs-data.qwencloud.com'],
  cn: ['aliyun.com', 'bailian.console.aliyun.com', 'bailian-cs.console.aliyun.com'],
});
const DOMAINS = {
  intl: new Set(['qwencloud.com', '.qwencloud.com', 'home.qwencloud.com', '.home.qwencloud.com', 'cs-data.qwencloud.com', '.cs-data.qwencloud.com']),
  cn: new Set(['aliyun.com', '.aliyun.com', 'bailian.console.aliyun.com', '.bailian.console.aliyun.com', 'bailian-cs.console.aliyun.com', '.bailian-cs.console.aliyun.com']),
};
const COOKIE_NAME = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]{1,128}$/;
const FORBIDDEN_TEXT = /[\u0000-\u001f\u007f]|bearer\s|sk[-_]|eyJ[A-Za-z0-9_-]{8}|https?:\/\/|[{}]|access_token|refresh_token|secret/i;
const PLAN_LABELS = new Set(['free', 'lite', 'basic', 'standard', 'pro', 'professional', 'premium', 'max', 'ultra', 'team', 'business', 'enterprise', 'go']);
const WINDOW_LABELS = Object.freeze({
  '5h': '5 hours', weekly: 'Weekly', monthly: 'Monthly',
  subscription: 'Plan subscription', 'addon-credits': 'Additional credits',
  'addon-packs': 'Active credit packs', 'addon-listed-packs': 'Listed active credit packs',
});
const CREDIT_PACK_KEY = /^addon-pack-[0-9a-f]{12}$/;
const MAX_USAGE_WINDOWS = 107;
const nonnegative = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
const percentage = value => nonnegative(value) !== null && value <= 100 ? value : null;
function iso(value) {
  if (typeof value !== 'string' || value.length > 40 || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)) return null;
  const parsed = new Date(value);
  return Number.isFinite(parsed.getTime()) && parsed.getUTCFullYear() >= 2000 && parsed.getUTCFullYear() <= 2200 ? parsed.toISOString() : null;
}
export function filterCookies(values, region, nowSeconds = Date.now() / 1000) {
  if (!Object.hasOwn(CONSOLE_URLS, region) || !Array.isArray(values) || values.length > 400) throw new Error('invalid_response');
  const unique = new Map();
  for (const value of values) {
    if (!value || typeof value !== 'object') continue;
    const domain = typeof value.domain === 'string' ? value.domain.toLowerCase() : '';
    if (!DOMAINS[region].has(domain)) continue;
    if (!COOKIE_NAME.test(value.name) || typeof value.value !== 'string' || value.value.length > 8192 || /[\u0000-\u001f\u007f;]/.test(value.value)) throw new Error('invalid_response');
    const path = typeof value.path === 'string' && value.path.startsWith('/') && value.path.length <= 1024 && !/[\u0000-\u001f\u007f]/.test(value.path) ? value.path : '/';
    const expirationDate = nonnegative(value.expirationDate);
    if (expirationDate !== null && expirationDate <= nowSeconds) continue;
    const cookie = {name: value.name, value: value.value, domain, path, secure: value.secure === true};
    if (expirationDate !== null) cookie.expirationDate = expirationDate;
    unique.set(`${domain}\n${path}\n${value.name}`, cookie);
  }
  if (unique.size > 200) throw new Error('invalid_response');
  return [...unique.values()];
}
export function cookieDiagnostics(values, selected, region) {
  return {region, browserRecords: Array.isArray(values) ? values.length : 0,
    selectedRecords: Array.isArray(selected) ? selected.length : 0,
    exactConsoleHostRecords: Array.isArray(selected) ? selected.filter(cookie => cookie.domain.replace(/^\./, '') === new URL(CONSOLE_URLS[region][0]).hostname).length : 0,
    exactGatewayHostRecords: Array.isArray(selected) ? selected.filter(cookie => cookie.domain.replace(/^\./, '') === new URL(CONSOLE_URLS[region][1]).hostname).length : 0};
}
export function projectSample(value) {
  if (!value || typeof value !== 'object' || value.provider !== 'qwen' || value.platform !== 'windows' || value.status !== 'ok' || !Array.isArray(value.windows) || value.windows.length > MAX_USAGE_WINDOWS) throw new Error('invalid_response');
  const fetchedAt = iso(value.fetchedAt ?? value.collectedAt);
  const sampledAt = iso(value.sampledAt ?? value.fetchedAt ?? value.collectedAt);
  if (!fetchedAt || !sampledAt) throw new Error('invalid_response');
  const windows = [];
  const seen = new Set();
  let creditPacks = 0;
  for (const window of value.windows) {
    if (!window || typeof window !== 'object' || typeof window.key !== 'string') throw new Error('invalid_response');
    const creditPack = CREDIT_PACK_KEY.test(window.key);
    if ((!Object.hasOwn(WINDOW_LABELS, window.key) && !creditPack) || seen.has(window.key)) throw new Error('invalid_response');
    seen.add(window.key);
    if (creditPack && ++creditPacks > 100) throw new Error('invalid_response');
    const usedPercent = percentage(window.usedPercent);
    const item = {key: window.key, label: creditPack ? `Additional credit pack ${creditPacks}` : WINDOW_LABELS[window.key], usedPercent,
      remainingPercent: usedPercent === null ? null : 100 - usedPercent,
      resetAt: creditPack || window.key === 'subscription' ? null : iso(window.resetAt), windowMinutes: nonnegative(window.windowMinutes),
      used: nonnegative(window.used), limit: nonnegative(window.limit),
      unit: ['credits', 'packs'].includes(window.unit) ? window.unit : null};
    if (['rate_limit', 'balance', 'spend', 'extra_usage'].includes(window.kind)) item.kind = window.kind;
    if (Object.hasOwn(window, 'remaining')) item.remaining = nonnegative(window.remaining);
    if (creditPack || window.key === 'subscription') item.expiresAt = iso(window.expiresAt);
    if ([item.usedPercent, item.resetAt, item.used, item.limit, item.remaining, item.expiresAt].some(x => x !== null && x !== undefined)) windows.push(item);
  }
  if (!windows.length) throw new Error('invalid_response');
  const email = typeof value.email === 'string' && value.email.length <= 254 && !FORBIDDEN_TEXT.test(value.email) && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value.email) ? value.email : null;
  const plan = typeof value.plan === 'string' && PLAN_LABELS.has(value.plan.toLowerCase()) ? value.plan.toLowerCase() : null;
  return {id: 'plan-qwen-windows', provider: 'qwen', providerLabel: 'Qwen token plan', label: 'Qwen token plan', email, plan,
    platform: 'windows', source: 'Browser session on Windows', status: 'ok', message: null,
    fetchedAt, sampledAt, isActive: false, windows,
    capabilities: {codexProfile: null, claudeProfileId: null, claudePlatforms: []}};
}
export const ERROR_MESSAGES = Object.freeze({
  no_browser_cookie: 'No saved Qwen console sign-in was found in this browser profile.',
  needs_sign_in: 'The existing Qwen console sign-in cannot read usage. Open the console in this browser.',
  host_unavailable: 'The Windows CCS usage helper is not installed or could not start.',
  network_error: 'The Qwen usage request could not finish. Try again later.',
  busy: 'Another Qwen usage refresh is still running. Try again soon.',
  invalid_response: 'Qwen returned no usable usage sample.',
  protocol_error: 'The local usage helper returned an invalid response.',
  error: 'Qwen usage could not be read.',
});
export function safeError(code) {
  if (typeof code !== 'string') return 'error';
  const aliases = {timeout: 'network_error', collector_unavailable: 'host_unavailable', unsupported_platform: 'host_unavailable', invalid_request: 'protocol_error'};
  const mapped = Object.hasOwn(aliases, code) ? aliases[code] : code;
  return Object.hasOwn(ERROR_MESSAGES, mapped) ? mapped : 'error';
}
