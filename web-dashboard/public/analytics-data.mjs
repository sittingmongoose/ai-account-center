import { visibleUsageWindows } from './visible-usage.mjs';
import { mainWindow } from './analytics-quota.mjs';
import { lazyFormat } from './time-format.mjs';

export { mainWindow };

/** A view of observed quotas. These samples are never summed into token or cost totals. */
export const ANALYTICS_PROVIDERS = [
  ['claude', 'Claude'], ['codex', 'Codex'], ['cursor', 'Cursor'],
  ['muse', 'Muse Code'], ['antigravity', 'Google Antigravity CLI'],
  ['kimi-code', 'Kimi Code'], ['qwen', 'Qwen Token Plan'],
  ['zai', 'Z.ai Coding Plan'], ['opencode-go', 'OpenCode Go'],
];
const array = value => Array.isArray(value) ? value : [];
const finite = value => typeof value === 'number' && Number.isFinite(value);
const text = value => typeof value === 'string' ? value : '';
const timestamp = value => typeof value === 'string' && value ? Date.parse(value) : NaN;
const numberFormat = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const compactFormat = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 2 });
const moneyFormat = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', maximumFractionDigits: 2 });
const compactMoneyFormat = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', notation: 'compact', maximumFractionDigits: 2 });
const dateFormat = lazyFormat({ month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
const number = value => numberFormat.format(value);
const axisNumber = value => Math.abs(value) >= 10_000 ? compactFormat.format(value) : number(value);
const percentage = value => finite(value) && value >= 0 ? value : null;
const usedPercent = value => percentage(value?.usedPercent) ?? (finite(value?.remainingPercent) && value.remainingPercent >= 0 && value.remainingPercent <= 100 ? 100 - value.remainingPercent : null);
const windowKind = value => ['rate_limit', 'balance', 'spend', 'extra_usage'].includes(value) ? value : '';
const historyIdentity = (account, window) => [text(account.id), text(window.key), text(window.label), text(window.unit), windowKind(window.kind)];
function compareHistories(account, first, second) {
  const a = historyIdentity(account, first), b = historyIdentity(account, second);
  for (let index = 1; index < a.length; index++) {
    const comparison = a[index].localeCompare(b[index]);
    if (comparison) return comparison;
  }
  return 0;
}
const status = value => ({ ok: 'Live', cached: 'Cached', unavailable: 'Unavailable', error: 'Refresh failed', needs_sign_in: 'Sign-in needed' }[value] || 'Unavailable');
function dateLabel(value, prefix = '') {
  if (!Number.isFinite(timestamp(value))) return '';
  return `${prefix}${dateFormat.format(new Date(value))}`;
}
function accountLabel(account) {
  const provider = text(account.providerLabel) || ANALYTICS_PROVIDERS.find(row => row[0] === account.provider)?.[1] || 'Provider';
  return `${provider} · ${text(account.email) || text(account.label) || 'Identity unavailable'} · ${text(account.platform) || 'Source unavailable'}`;
}
function options(payload, catalog) {
  const merged = new Map();
  for (const account of [...array(catalog), ...array(payload?.accounts)]) {
    if (typeof account?.id === 'string' && ANALYTICS_PROVIDERS.some(row => row[0] === account.provider)) merged.set(account.id, account);
  }
  const provider = text(payload?.filters?.provider) || 'all';
  const accounts = [...merged.values()].filter(account => provider === 'all' || account.provider === provider);
  const counts = new Map();
  for (const account of accounts) counts.set(accountLabel(account), (counts.get(accountLabel(account)) || 0) + 1);
  return {
    providers: [{ id: 'all', label: 'All providers' }, ...ANALYTICS_PROVIDERS.map(([id, label]) => ({ id, label }))],
    accounts: [{ id: 'all', label: 'All accounts' }, ...accounts.map(account => ({ id: account.id, label: accountLabel(account) + (counts.get(accountLabel(account)) > 1 ? ` · ${account.id}` : '') }))],
  };
}
function historicalWindows(account, range) {
  const windows = array(account.windows).filter(window => window && typeof window === 'object' && !Array.isArray(window) && text(window.key));
  const visible = visibleUsageWindows(account.provider, windows);
  if (account.provider !== 'zai') return visible;
  const from = timestamp(range?.from), to = timestamp(range?.to);
  // A currently empty pack summary stays hidden in the dashboard. Analytics
  // retains it only when real in-range history contains a positive pack count.
  const positivePast = windows.filter(window => !visible.includes(window)
    && /packs?|reset[\s_-]*(?:cards?|credits?)/i.test(`${text(window.key)} ${text(window.label)}`)
    && array(window.points).some(point => ['ok', 'cached'].includes(point?.status)
      && Number.isFinite(timestamp(point.sampledAt)) && timestamp(point.sampledAt) >= from && timestamp(point.sampledAt) <= to
      && ['used', 'limit', 'remaining'].some(key => finite(point[key]) && point[key] > 0)));
  return [...visible, ...positivePast];
}
function actualValue(point, metric, valueKind) {
  if (valueKind === 'percent') return usedPercent(point);
  if (valueKind === 'remaining') return finite(point?.remaining) ? point.remaining : null;
  return finite(point?.used) ? point.used : null;
}
function metricValueKind(metric) {
  if (!metric) return 'percent';
  // Current entitlement state must not discard genuine historical observations.
  if (metric.kind === 'balance' || metric.kind === 'extra_usage') {
    if (array(metric.points).some(point => finite(point?.remaining))) return 'remaining';
    if (array(metric.points).some(point => finite(point?.used))) return 'used';
    if (finite(metric.remaining)) return 'remaining';
    if (finite(metric.used)) return 'used';
  }
  if (usedPercent(metric) !== null || array(metric.points).some(point => usedPercent(point) !== null)) return 'percent';
  if (finite(metric.used) || array(metric.points).some(point => finite(point?.used))) return 'used';
  if (finite(metric.remaining) || array(metric.points).some(point => finite(point?.remaining))) return 'remaining';
  return 'unavailable';
}
export function buildQuotaPlot(metric, range, now = Date.now(), percentMaximum = 100) {
  const kind = metricValueKind(metric);
  const from = timestamp(range?.from), to = timestamp(range?.to);
  const usableRange = Number.isFinite(from) && Number.isFinite(to) && to > from;
  const raw = array(metric?.points).flatMap(point => {
    const time = timestamp(point?.sampledAt), value = actualValue(point, metric, kind);
    // Unavailable observations create a visible gap, rather than an invented zero.
    if (kind === 'unavailable' || !Number.isFinite(time) || value === null || !['ok', 'cached'].includes(point?.status) || !usableRange || time < from || time > to) return [];
    return [{ point, time, value }];
  }).sort((a, b) => a.time - b.time);
  const values = raw.map(row => row.value);
  const minimum = kind === 'percent' ? 0 : Math.min(0, ...values);
  const maximum = kind === 'percent' ? Math.max(100, percentMaximum, ...values) : Math.max(0, ...values);
  const span = maximum - minimum || 1;
  const unit = kind === 'percent' ? '%' : text(metric?.unit) ? ` ${metric.unit}` : '';
  const valueLabel = value => `${number(value)}${unit}${kind === 'percent' ? ' used' : kind === 'remaining' ? ' remaining' : ' used'}`;
  const points = raw.map(({ point, time, value }) => {
    const extra = [
      timestamp(point.observedAt) !== timestamp(point.sampledAt) ? dateLabel(point.observedAt, 'Recorded ') : '',
      dateLabel(point.resetAt, 'Resets '), dateLabel(point.expiresAt, 'Expires '),
      text(point.source), text(point.platform), point.status === 'cached' ? 'Cached observation' : '',
      point.isActive === true ? 'Active Codex account' : '',
    ].filter(Boolean);
    return { x: (time - from) / (to - from), percent: Math.max(0, Math.min(100, (value - minimum) / span * 100)), label: `${dateLabel(point.sampledAt, 'Sampled ')} · ${valueLabel(value)}${extra.length ? ` · ${extra.join(' · ')}` : ''}` };
  });
  const sampledAt = raw.at(-1)?.time;
  return {
    points, axisMaximum: maximum, metricPercent: kind === 'percent', chartHasPoints: points.length > 0,
    chartTop: `${axisNumber(maximum)}${unit}`, chartMiddle: `${axisNumber((minimum + maximum) / 2)}${unit}`, chartBottom: `${axisNumber(minimum)}${unit}`,
    chartStart: dateLabel(range?.from), chartEnd: dateLabel(range?.to),
    chartNote: points.length ? `${points.length} actual observations · ${kind === 'percent' ? 'Percent used' : kind === 'remaining' ? 'Remaining balance' : 'Reported usage'}${sampledAt && now - sampledAt > 15 * 60_000 ? ' · Last observation is over 15 minutes old' : ''}. Gaps are unavailable samples; resets are not connected.` : 'No usable historical observations for this metric in the selected range.',
  };
}
export function analyticsChoiceId(view, kind, label) {
  const rows = view?.choices?.[`${kind}s`];
  return array(rows).find(row => row.label === label)?.id ?? null;
}
const tokenFields = ['inputTokens', 'outputTokens', 'cacheCreationTokens', 'cacheReadTokens'];
function tokenTotal(row) {
  return tokenFields.every(key => finite(row?.[key]) && row[key] >= 0) ? tokenFields.reduce((sum, key) => sum + row[key], 0) : null;
}
function dollars(value) {
  return finite(value) && value >= 0 ? moneyFormat.format(value) : 'Unavailable';
}
function nativeRows(activity, range, interval) {
  const hourly = interval === 'Hourly';
  const step = hourly ? 3_600_000 : 86_400_000;
  const grouped = new Map();
  for (const row of array(hourly ? activity.byHour : activity.byDay)) {
    const total = tokenTotal(row);
    const value = text(hourly ? row?.hour : row?.date);
    const validDate = hourly ? /^\d{4}-\d{2}-\d{2}T\d{2}:00(?::00)?Z$/.test(value) : /^\d{4}-\d{2}-\d{2}$/.test(value);
    const time = validDate ? timestamp(hourly ? value : `${value}T00:00:00Z`) : NaN;
    const canonical = Number.isFinite(time) ? new Date(time).toISOString() : '';
    if (total === null || !Number.isFinite(time) || (hourly ? canonical.slice(0, 13) !== value.slice(0, 13) : canonical.slice(0, 10) !== value) || !['claude', 'codex'].includes(row.provider)) continue;
    const prior = grouped.get(time) || { ...Object.fromEntries(tokenFields.map(key => [key, 0])), cost: 0, hasCost: true, providers: new Set() };
    // A native aggregate is unique per provider/bucket. Never count duplicate rows twice.
    if (prior.providers.has(row.provider)) continue;
    tokenFields.forEach(key => { prior[key] += row[key]; });
    if (finite(row.estimatedCostUsd) && row.estimatedCostUsd >= 0) prior.cost += row.estimatedCostUsd;
    else prior.hasCost = false;
    prior.providers.add(row.provider); grouped.set(time, prior);
  }
  const from = timestamp(range.from), to = timestamp(range.to);
  return [...grouped.entries()].filter(([time]) => !Number.isFinite(from) || !Number.isFinite(to) || time + step > from && time <= to).sort((a, b) => a[0] - b[0]);
}
export function activityView(activity = {}, range = {}, interval = 'Daily') {
  interval = interval === 'Hourly' ? 'Hourly' : 'Daily';
  const totals = activity?.totals;
  const available = ['ok', 'cached'].includes(activity.status) && tokenTotal(totals) !== null;
  const rows = available ? nativeRows(activity, range, interval) : [];
  const tokenMaximum = Math.max(0, ...rows.map(([, row]) => tokenTotal(row)));
  const costMaximum = Math.max(0, ...rows.filter(([, row]) => row.hasCost).map(([, row]) => row.cost));
  const step = interval === 'Hourly' ? 3_600_000 : 86_400_000;
  const parsedFrom = timestamp(range.from), parsedTo = timestamp(range.to);
  const from = Number.isFinite(parsedFrom) ? Math.floor(parsedFrom / step) * step : rows[0]?.[0];
  const to = Number.isFinite(parsedTo) ? parsedTo : rows.at(-1)?.[0];
  const position = time => from === to ? 0.5 : (time - from) / (to - from);
  const bucketLabel = time => new Date(time).toISOString().slice(0, interval === 'Hourly' ? 16 : 10).replace('T', ' ') + ' UTC';
  const label = (time, row) => `${bucketLabel(time)} · ${number(tokenTotal(row))} actual tokens · ${number(row.inputTokens)} input · ${number(row.outputTokens)} output · ${number(row.cacheCreationTokens)} cache created · ${number(row.cacheReadTokens)} cache read · ${row.hasCost ? dollars(row.cost) : 'Unavailable'} estimated API-equivalent cost · ${[...row.providers].join(' + ')}`;
  const providers = available ? array(activity.providers).filter(row => tokenTotal(row.totals) !== null && ['claude', 'codex'].includes(row.provider)) : [];
  const uniqueProviders = [...new Map(providers.map(row => [row.provider, row])).values()];
  const counter = key => uniqueProviders.length && uniqueProviders.every(row => Number.isInteger(row[key]) && row[key] >= 0) ? uniqueProviders.reduce((sum, row) => sum + row[key], 0) : null;
  const sessions = counter('sessionCount'), events = counter('usageEvents');
  const models = available ? [...new Map(array(activity.models).filter(row => tokenTotal(row) !== null && ['claude', 'codex'].includes(row.provider) && text(row.model) && text(row.model) !== '<synthetic>').map(row => [JSON.stringify([row.provider, row.model]), row])).values()].sort((a, b) => (finite(b.estimatedCostUsd) ? b.estimatedCostUsd : -1) - (finite(a.estimatedCostUsd) ? a.estimatedCostUsd : -1) || a.provider.localeCompare(b.provider) || a.model.localeCompare(b.model)) : [];
  const listedTokens = models.reduce((sum, row) => sum + tokenTotal(row), 0);
  const largestCost = Math.max(0, ...models.filter(row => finite(row.estimatedCostUsd) && row.estimatedCostUsd >= 0).map(row => row.estimatedCostUsd));
  return {
    activityTitle: 'CLI usage activity', activityIntervalValue: interval,
    activityNote: [text(activity.message), 'Claude Code and Codex CLI logs; UTC buckets. Input excludes cached tokens; output includes reasoning. Activity covers all accounts and cannot be attributed to an individual account. Every cost is an estimated API equivalent, not a subscription charge.', activity.status === 'cached' ? 'Cached activity data.' : '', dateLabel(activity.fetchedAt, 'Updated ')].filter(Boolean).join(' '),
    activityHasData: available,
    activitySummaries: available ? [
      { label: 'Input tokens', value: axisNumber(totals.inputTokens), note: `${number(totals.inputTokens)} tokens · Actual uncached input` },
      { label: 'Output tokens', value: axisNumber(totals.outputTokens), note: `${number(totals.outputTokens)} tokens · Includes reasoning tokens` },
      { label: 'Cache created', value: axisNumber(totals.cacheCreationTokens), note: `${number(totals.cacheCreationTokens)} tokens · Actual cache-creation tokens` },
      { label: 'Cache read', value: axisNumber(totals.cacheReadTokens), note: `${number(totals.cacheReadTokens)} tokens · Actual cache-read tokens` },
      { label: 'Estimated API cost', value: finite(totals.estimatedCostUsd) && totals.estimatedCostUsd >= 10_000 ? compactMoneyFormat.format(totals.estimatedCostUsd) : dollars(totals.estimatedCostUsd), note: `${dollars(totals.estimatedCostUsd)} estimated API equivalent; not your bill` },
      { label: 'Sessions', value: sessions === null ? 'Unavailable' : axisNumber(sessions), note: `${events === null ? 'Unavailable' : number(events)} parsed usage-log entries · ${sessions === null ? 'Unavailable' : number(sessions)} sessions last active in range` },
    ] : [],
    activityPoints: rows.map(([time, row]) => ({ x: position(time), percent: tokenMaximum > 0 ? tokenTotal(row) / tokenMaximum * 100 : 0, label: label(time, row) })),
    activityStackPoints: rows.map(([time, row]) => ({ x: position(time), input: tokenMaximum > 0 ? row.inputTokens / tokenMaximum * 100 : 0, output: tokenMaximum > 0 ? row.outputTokens / tokenMaximum * 100 : 0, cacheCreated: tokenMaximum > 0 ? row.cacheCreationTokens / tokenMaximum * 100 : 0, cacheRead: tokenMaximum > 0 ? row.cacheReadTokens / tokenMaximum * 100 : 0, label: label(time, row) })),
    activityCostPoints: rows.filter(([, row]) => row.hasCost).map(([time, row]) => ({ x: position(time), percent: costMaximum > 0 ? row.cost / costMaximum * 100 : 0, label: `${bucketLabel(time)} · ${dollars(row.cost)} estimated API-equivalent cost · ${[...row.providers].join(' + ')}` })),
    activityChartTop: axisNumber(tokenMaximum), activityChartMiddle: axisNumber(tokenMaximum / 2), activityChartBottom: '0',
    activityCostTop: rows.some(([, row]) => row.hasCost) ? dollars(costMaximum) : 'Unavailable', activityCostMiddle: rows.some(([, row]) => row.hasCost) ? dollars(costMaximum / 2) : '', activityCostBottom: rows.some(([, row]) => row.hasCost) ? '$0.00' : '',
    activityChartStart: Number.isFinite(from) ? bucketLabel(from) : '', activityChartEnd: Number.isFinite(to) ? bucketLabel(to) : '',
    activityModelNote: `Top models reported by the collector (up to 30, ranked by estimated API cost). Token shares use the ${number(models.length)} listed models only; they are not a complete account or subscription distribution. Input, output and the two cache categories remain separate.`,
    activityModels: models.map(row => ({ label: row.model, provider: row.provider === 'claude' ? 'Claude' : 'Codex', input: number(row.inputTokens), output: number(row.outputTokens), cache: number(row.cacheCreationTokens + row.cacheReadTokens), cacheCreated: number(row.cacheCreationTokens), cacheRead: number(row.cacheReadTokens), total: number(tokenTotal(row)), cost: dollars(row.estimatedCostUsd), hasCost: finite(row.estimatedCostUsd) && row.estimatedCostUsd >= 0, costPercent: finite(row.estimatedCostUsd) && row.estimatedCostUsd >= 0 && largestCost > 0 ? row.estimatedCostUsd / largestCost * 100 : 0, tokenPercent: listedTokens > 0 ? tokenTotal(row) / listedTokens * 100 : 0, share: listedTokens > 0 ? `${number(tokenTotal(row) / listedTokens * 100)}%` : '0%' })),
    activityProviders: uniqueProviders.map(row => ({ key: row.provider, label: text(row.label) || row.provider, amount: `${number(tokenTotal(row.totals))} tokens · ${dollars(row.totals.estimatedCostUsd)} estimated API-equivalent cost`, reset: `${Number.isInteger(row.usageEvents) && row.usageEvents >= 0 ? number(row.usageEvents) : 'Unavailable'} parsed usage-log entries`, expiration: `${Number.isInteger(row.sessionCount) && row.sessionCount >= 0 ? number(row.sessionCount) : 'Unavailable'} sessions last active in range`, note: 'CLI usage logs · all accounts' })),
  };
}
export function analyticsView(payload, { catalog = [], metricKey = '', activityInterval = 'Daily' } = {}, now = Date.now()) {
  const providerId = text(payload?.filters?.provider) || 'all';
  const accountId = text(payload?.filters?.account) || 'all';
  const providerOrder = new Map(ANALYTICS_PROVIDERS.map(([id], index) => [id, index]));
  const accounts = array(payload?.accounts).filter(account => account && text(account.id) && ANALYTICS_PROVIDERS.some(row => row[0] === account.provider) && (providerId === 'all' || account.provider === providerId) && (accountId === 'all' || account.id === accountId)).map(account => ({ ...account, windows: historicalWindows(account, payload?.range) })).sort((a, b) => (providerOrder.get(a.provider) ?? 99) - (providerOrder.get(b.provider) ?? 99) || (text(a.email) || text(a.label)).localeCompare(text(b.email) || text(b.label)) || text(a.id).localeCompare(text(b.id)));
  const choices = options(payload, catalog);
  // Changed labels, units and kinds are distinct reported historical series.
  // Qualify the selector with only validated scalar metadata; active status never selects a default.
  const histories = accounts.flatMap(account => array(account.windows).toSorted((a, b) => compareHistories(account, a, b)).map(window => ({ account, window, key: JSON.stringify(historyIdentity(account, window)), plot: buildQuotaPlot(window, payload?.range, now) }))).filter(row => row.plot.chartHasPoints);
  const percentMaximum = Math.max(100, ...histories.filter(row => row.plot.metricPercent).map(row => row.plot.axisMaximum));
  const allCharts = histories.map(({ account, window, key, plot: initial }) => {
    const plot = initial.metricPercent ? { ...initial, chartTop: `${axisNumber(percentMaximum)}%`, chartMiddle: `${axisNumber(percentMaximum / 2)}%`, points: initial.points.map(point => ({ ...point, percent: point.percent * initial.axisMaximum / percentMaximum })) } : initial;
    return { key, title: `${text(account.email) || text(account.label) || 'Account'} · ${text(window.label) || 'Usage'}`, subtitle: [text(account.providerLabel) || ANALYTICS_PROVIDERS.find(row => row[0] === account.provider)?.[1], text(window.unit), account.provider === 'codex' && account.isActive ? 'Active Codex' : '', metricValueKind(window) === 'percent' ? 'Percent used' : metricValueKind(window) === 'remaining' ? 'Remaining balance' : 'Reported usage'].filter(Boolean).join(' · '), note: [status(account.status), text(account.platform), text(account.source), plot.chartNote].filter(Boolean).join(' · '), top: plot.chartTop, middle: plot.chartMiddle, bottom: plot.chartBottom, start: plot.chartStart, end: plot.chartEnd, points: plot.points };
  });
  choices.metrics = [{ id: 'all', label: 'All histories' }, ...allCharts.map(row => {
    const [, key, , unit, kind] = JSON.parse(row.key);
    const kindLabel = { rate_limit: 'Quota', balance: 'Balance', spend: 'Spending', extra_usage: 'Extra usage' }[kind] || '';
    return { id: row.key, label: [row.subtitle.split(' · ')[0], row.title, key.replace(/[_-]+/g, ' '), unit, kindLabel].filter(Boolean).join(' · ') };
  })];
  const selectedKey = choices.metrics.some(row => row.id === metricKey) ? metricKey : 'all';
  const quotaCharts = selectedKey === 'all' ? allCharts : allCharts.filter(row => row.key === selectedKey);
  const summary = payload?.summary || {}, history = payload?.history || {};
  const oldest = dateLabel(history.oldestSampleAt), newest = dateLabel(history.newestSampleAt);
  const available = Number.isInteger(summary.availableAccounts) ? summary.availableAccounts : accounts.filter(account => ['ok', 'cached'].includes(account.status)).length;
  const samples = Number.isInteger(summary.sampleCount) ? summary.sampleCount : Number.isInteger(history.sampleCount) ? history.sampleCount : 0;
  const accountCount = Number.isInteger(summary.accountCount) ? summary.accountCount : accounts.length;
  const rangeValue = { '24h': 'Last 24 hours', '7d': 'Last 7 days', '30d': 'Last 30 days' }[payload?.range?.preset] || 'Last 7 days';
  return {
    loading: false, error: '', updated: dateLabel(payload?.updatedAt, 'Updated ') || 'Analytics data unavailable',
    historyNote: [text(history.message), oldest ? `Observed ${oldest}${newest && newest !== oldest ? ` – ${newest}` : ''}` : 'History starts as AI Account Center receives real account observations.', 'Quota observations are snapshots, not additive token or cost totals. Each account/window keeps its own history; gaps and resets are not connected. Percentage charts share one scale, extended when actual usage exceeds 100%; balance units scale independently.'].filter(Boolean).join(' '),
    overviewNote: `${number(accountCount)} accounts · ${number(available)} live or cached · ${number(samples)} observations in range · ${Number.isInteger(history.retentionDays) ? history.retentionDays : 30} day retention · ${number(allCharts.length)} available histories`,
    rangeValue, providerValue: choices.providers.find(row => row.id === providerId)?.label || 'All providers', accountValue: choices.accounts.find(row => row.id === accountId)?.label || 'All accounts', metricValue: choices.metrics.find(row => row.id === selectedKey)?.label || 'All histories',
    providerOptions: choices.providers.map(row => row.label), accountOptions: choices.accounts.map(row => row.label), metricOptions: choices.metrics.map(row => row.label),
    summaries: [],
    providers: ANALYTICS_PROVIDERS.map(([id, label]) => {
      const provider = array(payload?.providers).find(row => row.provider === id);
      const outside = providerId !== 'all' && providerId !== id || accountId !== 'all' && !accounts.some(account => account.provider === id);
      if (outside) return { id, label, accounts: '—', availability: 'Outside filter', sample: 'Select to view this provider' };
      return { id, label, accounts: `${Number.isInteger(provider?.accountCount) ? provider.accountCount : accounts.filter(account => account.provider === id).length} accounts`, availability: `${Number.isInteger(provider?.availableAccounts) ? provider.availableAccounts : accounts.filter(account => account.provider === id && ['ok', 'cached'].includes(account.status)).length} available`, sample: dateLabel(provider?.latestSampleAt) || 'No observations' };
    }),
    accounts: accounts.map(account => ({ id: text(account.id), label: text(account.email) || text(account.label) || 'Account identity unavailable', provider: text(account.providerLabel) || ANALYTICS_PROVIDERS.find(row => row[0] === account.provider)?.[1] || 'Provider', status: status(account.status), platform: text(account.platform), source: text(account.source), plan: text(account.plan), active: account.provider === 'codex' && account.isActive === true, samples: `${Number.isInteger(account.sampleCount) ? account.sampleCount : 0} observations` })),
    quotaCharts,
    // Legacy single-chart bindings are empty; all histories are rendered by quotaCharts.
    metrics: [], points: [], chartTitle: 'Account histories', chartNote: '', chartHasPoints: false,
    ...activityView(accountId === 'all' ? payload?.activity : { ...payload?.activity, status: 'unavailable', totals: null, message: text(payload?.activity?.message) || 'CLI usage cannot be attributed to an individual account.' }, payload?.range, activityInterval),
    // The quota history (the former Headroom) is one row per account, grouped by provider.
    // quotaCharts above remain the per-window history data that a row's focus chart reads by key.
    version: ANALYTICS_VIEW_VERSION,
    quotaHistory: quotaHistory(accounts, histories, payload?.range, now),
    choices, selection: { providerId, accountId, metricKey: selectedKey },
  };
}

export const ANALYTICS_VIEW_VERSION = 4;
const percentOf = window => usedPercent(window);
function relativeReset(value, now) {
  const time = timestamp(value);
  if (!Number.isFinite(time)) return '';
  const delta = time - now;
  if (delta <= 0) return 'reset due';
  const minutes = Math.floor(delta / 60000), hours = Math.floor(minutes / 60), days = Math.floor(hours / 24);
  return `resets in ${days >= 1 ? `${days}d ${hours % 24}h` : hours >= 1 ? `${hours}h ${minutes % 60}m` : `${Math.max(1, minutes)}m`}`;
}
/** A sparkline path in a 100 x 100 viewbox from a quota plot (gaps are never bridged with invented zeros). */
export function sparklinePath(points) {
  return array(points).map((point, index) => `${index ? 'L' : 'M'}${(point.x * 100).toFixed(2)} ${(100 - point.percent).toFixed(2)}`).join(' ');
}
/** Quota history rows: every account once, grouped by provider; active state never selects or reorders a row. */
export function quotaHistory(accounts, histories, range, now = Date.now()) {
  const groups = [];
  for (const [provider, label] of ANALYTICS_PROVIDERS) {
    const rows = array(accounts).filter(account => account.provider === provider).map(account => {
      const window = mainWindow(account);
      const key = window ? JSON.stringify(historyIdentity(account, window)) : '';
      const plot = window ? (histories.find(row => row.key === key)?.plot || buildQuotaPlot(window, range, now)) : null;
      const current = window ? percentOf(window) : null;
      return {
        id: text(account.id), key: key || text(account.id), provider,
        label: text(account.email) || text(account.label) || 'Account identity unavailable',
        sub: [text(account.plan), text(account.platform)].filter(Boolean).join(' · '),
        windowLabel: window ? text(window.label) || 'Usage' : 'No quota reported',
        hasValue: current !== null, value: current ?? 0, valueText: current === null ? '' : number(current),
        reset: window ? relativeReset(window.resetAt, now) : '',
        active: provider === 'codex' && account.isActive === true,
        spark: plot?.metricPercent ? sparklinePath(plot.points) : '', sparkPoints: plot?.metricPercent ? plot.points.length : 0,
      };
    });
    if (rows.length) groups.push({ provider, label, rows });
  }
  return groups;
}
/**
 * The JSON src/analytics.rs reads (version 3). `page` carries the Analytics page: the page state echoed to
 * the controls, the Usage blocks (analytics-usage.mjs), the quota history with the focus charts of open rows
 * and the resets agenda (analytics-quota.mjs). The head, KPI and quota-group fields of the earlier seam stay.
 */
export function analyticsSlintModel(view, page = null) {
  const base = {
    version: ANALYTICS_VIEW_VERSION,
    head: { updated: text(view?.updated), range: { 'Last 24 hours': '24h', 'Last 7 days': '7d', 'Last 30 days': '30d' }[view?.rangeValue] || '7d', provider: text(view?.providerValue), note: text(view?.historyNote), hasActivity: view?.activityHasData === true },
    kpis: array(view?.activitySummaries).map(row => ({ key: row.label, label: row.label, value: row.value, sub: row.note })),
    quotaGroups: array(view?.quotaHistory),
  };
  if (!page) return base;
  const { usage, quota, agenda, state, paths } = page;
  const { geo, ...trend } = usage.trend;
  const { range, ...rest } = usage;
  return {
    ...base,
    state: { range: state.range, prov: state.prov, split: !!state.split, cache: !!state.cache, donut: state.donut, heat: state.heat, cbmSort: state.cbmSort === 'tokens' ? 'tokens' : 'cost' },
    usage: { ...rest, trend: { ...trend, ...(paths ? { paths } : {}) } },
    quota,
    agenda,
  };
}
