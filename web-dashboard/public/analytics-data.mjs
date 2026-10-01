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
const number = value => new Intl.NumberFormat(undefined, { maximumFractionDigits: 4 }).format(value);
const percentage = value => finite(value) && value >= 0 ? value : null;
const usedPercent = value => percentage(value?.usedPercent) ?? (finite(value?.remainingPercent) && value.remainingPercent >= 0 && value.remainingPercent <= 100 ? 100 - value.remainingPercent : null);
const status = value => ({ ok: 'Live', cached: 'Cached', unavailable: 'Unavailable', error: 'Refresh failed', needs_sign_in: 'Sign-in needed' }[value] || 'Unavailable');
function dateLabel(value, prefix = '') {
  if (!Number.isFinite(timestamp(value))) return '';
  return `${prefix}${new Date(value).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })}`;
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
  return {
    providers: [{ id: 'all', label: 'All providers' }, ...ANALYTICS_PROVIDERS.map(([id, label]) => ({ id, label }))],
    accounts: [{ id: 'all', label: 'All accounts' }, ...[...merged.values()]
      .filter(account => provider === 'all' || account.provider === provider)
      .map(account => ({ id: account.id, label: accountLabel(account) }))],
  };
}
function metrics(windows) {
  const counts = new Map();
  for (const window of windows) counts.set(text(window.label), (counts.get(text(window.label)) || 0) + 1);
  return windows.filter(window => typeof window?.key === 'string').map(window => ({
    id: window.key,
    label: (text(window.label) || 'Usage') + (counts.get(text(window.label)) > 1 ? ` · ${window.key}` : ''),
  }));
}
function actualValue(point, metric, valueKind) {
  if (valueKind === 'percent') return usedPercent(point);
  if (valueKind === 'remaining') return finite(point?.remaining) ? point.remaining : null;
  return finite(point?.used) ? point.used : null;
}
function metricValueKind(metric) {
  if (!metric) return 'percent';
  if (metric.unlimited === true || metric.enabled === false) return 'unavailable';
  if (metric.kind === 'balance' || metric.kind === 'extra_usage') {
    if (finite(metric.remaining) || array(metric.points).some(point => finite(point?.remaining))) return 'remaining';
    if (finite(metric.used) || array(metric.points).some(point => finite(point?.used))) return 'used';
  }
  if (usedPercent(metric) !== null || array(metric.points).some(point => usedPercent(point) !== null)) return 'percent';
  if (finite(metric.used) || array(metric.points).some(point => finite(point?.used))) return 'used';
  if (finite(metric.remaining) || array(metric.points).some(point => finite(point?.remaining))) return 'remaining';
  return 'unavailable';
}
function hasHistory(account) {
  return array(account?.windows).some(metric => {
    const kind = metricValueKind(metric);
    return kind !== 'unavailable' && array(metric.points).some(point => actualValue(point, metric, kind) !== null && Number.isFinite(timestamp(point?.sampledAt)));
  });
}
function currentAmount(metric) {
  if (metric.enabled === false) return 'Disabled';
  if (metric.unlimited === true) return 'Unlimited';
  const unit = text(metric.unit) ? ` ${metric.unit}` : '';
  const parts = [];
  const percent = usedPercent(metric);
  if (percent !== null && metric.kind !== 'balance' && metric.kind !== 'extra_usage') parts.push(`${number(percent)}% used`);
  if (finite(metric.used)) parts.push(`${number(metric.used)}${finite(metric.limit) ? ` / ${number(metric.limit)}` : ''}${unit} used`);
  else if (finite(metric.limit)) parts.push(`${number(metric.limit)}${unit} limit`);
  if (finite(metric.remaining)) parts.push(`${number(metric.remaining)}${unit} remaining`);
  return parts.join(' · ') || 'Usage unavailable';
}
export function buildQuotaPlot(metric, range, now = Date.now()) {
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
  const maximum = kind === 'percent' ? Math.max(100, ...values) : Math.max(0, ...values);
  const span = maximum - minimum || 1;
  const unit = kind === 'percent' ? '%' : text(metric?.unit) ? ` ${metric.unit}` : '';
  const valueLabel = value => `${number(value)}${unit}${kind === 'percent' ? ' used' : kind === 'remaining' ? ' remaining' : ' used'}`;
  const points = raw.map(({ point, time, value }) => {
    const extra = [
      dateLabel(point.resetAt, 'Resets '), dateLabel(point.expiresAt, 'Expires '),
      text(point.source), text(point.platform), point.status === 'cached' ? 'Cached observation' : '',
      point.isActive === true ? 'Active Codex account' : '',
    ].filter(Boolean);
    return { x: (time - from) / (to - from), percent: Math.max(0, Math.min(100, (value - minimum) / span * 100)), label: `${dateLabel(point.sampledAt)} · ${valueLabel(value)}${extra.length ? ` · ${extra.join(' · ')}` : ''}` };
  });
  const sampledAt = raw.at(-1)?.time;
  return {
    points, metricPercent: kind === 'percent', chartHasPoints: points.length > 0,
    chartTop: `${number(maximum)}${unit}`, chartMiddle: `${number((minimum + maximum) / 2)}${unit}`, chartBottom: `${number(minimum)}${unit}`,
    chartStart: dateLabel(range?.from), chartEnd: dateLabel(range?.to),
    chartNote: points.length ? `${points.length} actual observations · ${kind === 'percent' ? 'Percent used' : kind === 'remaining' ? 'Remaining balance' : 'Reported usage'}${sampledAt && now - sampledAt > 15 * 60_000 ? ' · Last observation is over 15 minutes old' : ''}. Gaps are unavailable samples; resets are not connected.` : 'No usable historical observations for this metric in the selected range. Current values are shown below.',
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
  return finite(value) && value >= 0 ? new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', maximumFractionDigits: 4 }).format(value) : 'Unavailable';
}
export function activityView(activity = {}, range = {}) {
  const totals = activity?.totals;
  const validTotals = tokenTotal(totals) !== null;
  const available = ['ok', 'cached'].includes(activity.status) && validTotals;
  const daily = new Map();
  if (available) for (const row of array(activity.byDay)) {
    const total = tokenTotal(row);
    const time = /^\d{4}-\d{2}-\d{2}$/.test(text(row?.date)) ? timestamp(`${row.date}T00:00:00Z`) : NaN;
    if (total === null || !Number.isFinite(time) || !['claude', 'codex'].includes(row.provider)) continue;
    const prior = daily.get(time) || { total: 0, date: row.date, providers: new Set() };
    // The backend returns one provider/day. Reject duplicate rows instead of double-counting them.
    if (prior.providers.has(row.provider)) continue;
    prior.total += total; prior.providers.add(row.provider); daily.set(time, prior);
  }
  const from = timestamp(range.from), to = timestamp(range.to);
  const rows = [...daily.entries()].filter(([time]) => !Number.isFinite(from) || !Number.isFinite(to) || time + 86_400_000 > from && time <= to).sort((a, b) => a[0] - b[0]);
  const maximum = Math.max(0, ...rows.map(([, row]) => row.total));
  const first = rows[0]?.[0], last = rows.at(-1)?.[0];
  const points = rows.map(([time, row]) => ({ x: first === last ? 0.5 : (time - first) / (last - first), percent: maximum > 0 ? row.total / maximum * 100 : 0, label: `${row.date} UTC · ${number(row.total)} actual tokens · ${[...row.providers].join(' + ')}` }));
  return {
    activityTitle: 'Local CLI activity',
    activityNote: [text(activity.message), 'Ubuntu Claude Code and Codex CLI logs only. Input excludes cached tokens; output includes reasoning. These totals cover all accounts and cannot be attributed to the currently active account. Costs are estimated API equivalents, not subscription charges.', activity.status === 'cached' ? 'Cached activity data.' : '', dateLabel(activity.fetchedAt, 'Updated ')].filter(Boolean).join(' '),
    activityHasData: available,
    activitySummaries: available ? [
      { label: 'Input tokens', value: number(totals.inputTokens), note: 'Uncached input from local CLI events' },
      { label: 'Output tokens', value: number(totals.outputTokens), note: 'Actual local CLI events' },
      { label: 'Cache tokens', value: number(totals.cacheCreationTokens + totals.cacheReadTokens), note: `${number(totals.cacheCreationTokens)} created · ${number(totals.cacheReadTokens)} read` },
      { label: 'Estimated API cost', value: dollars(totals.estimatedCostUsd), note: 'API equivalent; not your bill' },
    ] : [],
    activityPoints: points, activityChartTop: number(maximum), activityChartMiddle: number(maximum / 2), activityChartBottom: '0',
    activityChartStart: rows[0]?.[1].date || '', activityChartEnd: rows.at(-1)?.[1].date || '',
    activityModels: available ? array(activity.models).filter(row => tokenTotal(row) !== null && ['claude', 'codex'].includes(row.provider)).map(row => ({ label: text(row.model) || 'Model unavailable', provider: row.provider === 'claude' ? 'Claude' : 'Codex', input: number(row.inputTokens), output: number(row.outputTokens), cache: number(row.cacheCreationTokens + row.cacheReadTokens), cost: dollars(row.estimatedCostUsd) })) : [],
    activityProviders: available ? array(activity.providers).filter(row => tokenTotal(row.totals) !== null && ['claude', 'codex'].includes(row.provider)).map(row => ({ key: row.provider, label: text(row.label) || row.provider, amount: `${number(tokenTotal(row.totals))} tokens · ${dollars(row.totals.estimatedCostUsd)} estimated API cost`, reset: `${Number.isInteger(row.usageEvents) ? number(row.usageEvents) : 'Unavailable'} usage events`, expiration: `${Number.isInteger(row.sessionCount) ? number(row.sessionCount) : 'Unavailable'} sessions`, note: 'Ubuntu local CLI · all accounts' })) : [],
  };
}
export function analyticsView(payload, { catalog = [], metricKey = '' } = {}, now = Date.now()) {
  const accounts = array(payload?.accounts);
  const choices = options(payload, catalog);
  const providerId = text(payload?.filters?.provider) || 'all';
  const accountId = text(payload?.filters?.account) || 'all';
  const selected = accounts.find(account => account.id === accountId)
    || accounts.find(account => account.id === payload?.summary?.activeCodexAccountId && hasHistory(account))
    || accounts.find(hasHistory) || accounts[0];
  const windows = array(selected?.windows);
  const chosenMetric = windows.find(window => window.key === metricKey)
    || windows.find(window => window.kind !== 'balance' && window.kind !== 'extra_usage' && metricValueKind(window) !== 'unavailable' && array(window.points).some(point => usedPercent(point) !== null))
    || windows.find(window => metricValueKind(window) !== 'unavailable') || windows[0];
  choices.metrics = metrics(windows);
  const selectedMetric = choices.metrics.find(row => row.id === chosenMetric?.key);
  const summary = payload?.summary || {};
  const history = payload?.history || {};
  const oldest = dateLabel(history.oldestSampleAt);
  const newest = dateLabel(history.newestSampleAt);
  const rangeValue = { '24h': 'Last 24 hours', '7d': 'Last 7 days', '30d': 'Last 30 days' }[payload?.range?.preset] || 'Last 7 days';
  const available = Number.isInteger(summary.availableAccounts) ? summary.availableAccounts : accounts.filter(account => ['ok', 'cached'].includes(account.status)).length;
  const samples = Number.isInteger(summary.sampleCount) ? summary.sampleCount : Number.isInteger(history.sampleCount) ? history.sampleCount : 0;
  const accountCount = Number.isInteger(summary.accountCount) ? summary.accountCount : accounts.length;
  const plot = buildQuotaPlot(chosenMetric, payload?.range, now);
  return {
    loading: false, error: '', updated: dateLabel(payload?.updatedAt, 'Updated ') || 'Analytics data unavailable',
    historyNote: [text(history.message), oldest ? `Observed ${oldest}${newest && newest !== oldest ? ` – ${newest}` : ''}` : 'History starts as CCS receives real account observations.', 'Quota observations are snapshots, not additive token or cost totals.'].filter(Boolean).join(' '),
    rangeValue, providerValue: choices.providers.find(row => row.id === providerId)?.label || 'All providers',
    accountValue: choices.accounts.find(row => row.id === accountId)?.label || 'All accounts', metricValue: selectedMetric?.label || 'No metrics available',
    providerOptions: choices.providers.map(row => row.label), accountOptions: choices.accounts.map(row => row.label), metricOptions: choices.metrics.map(row => row.label),
    summaries: [
      { label: 'Accounts', value: number(accountCount), note: providerId === 'all' ? 'Across your selected accounts' : choices.providers.find(row => row.id === providerId)?.label || '' },
      { label: 'Available accounts', value: number(available), note: 'Live or explicitly cached samples' },
      { label: 'Observed samples', value: number(samples), note: 'Within this time range' },
      { label: 'History retention', value: `${Number.isInteger(history.retentionDays) ? history.retentionDays : 30} days`, note: 'Collects actual samples from every provider' },
    ],
    providers: ANALYTICS_PROVIDERS.map(([id, label]) => {
      const provider = array(payload?.providers).find(row => row.provider === id);
      const outside = providerId !== 'all' && providerId !== id || accountId !== 'all' && !accounts.some(account => account.provider === id);
      if (outside) return { id, label, accounts: '—', availability: 'Outside filter', sample: 'Select to view this provider' };
      return { id, label, accounts: `${Number.isInteger(provider?.accountCount) ? provider.accountCount : accounts.filter(account => account.provider === id).length} accounts`, availability: `${Number.isInteger(provider?.availableAccounts) ? provider.availableAccounts : accounts.filter(account => account.provider === id && ['ok', 'cached'].includes(account.status)).length} available`, sample: dateLabel(provider?.latestSampleAt) || 'No observations' };
    }),
    accounts: accounts.map(account => ({ id: text(account.id), label: text(account.email) || text(account.label) || 'Account identity unavailable', provider: text(account.providerLabel) || ANALYTICS_PROVIDERS.find(row => row[0] === account.provider)?.[1] || 'Provider', status: status(account.status), platform: text(account.platform), source: text(account.source), plan: text(account.plan), active: account.provider === 'codex' && account.isActive === true, samples: `${Number.isInteger(account.sampleCount) ? account.sampleCount : 0} observations` })),
    metrics: windows.map(window => ({ key: text(window.key), label: text(window.label) || 'Usage', amount: currentAmount(window), reset: dateLabel(window.resetAt, 'Resets ') || 'Reset not reported', expiration: dateLabel(window.expiresAt, 'Expires '), note: [window.enabled === false ? 'Disabled' : '', window.unlimited === true ? 'Unlimited' : '', text(window.unit), finite(window.windowMinutes) ? `${number(window.windowMinutes)} minute window` : ''].filter(Boolean).join(' · ') })),
    chartTitle: selected ? `${text(selected.email) || text(selected.label) || 'Account'} · ${selectedMetric?.label || 'Usage'}` : 'Account usage history',
    selectedAccountNote: selected ? [text(selected.providerLabel), text(selected.plan), text(selected.platform), status(selected.status), text(selected.source), text(selected.message)].filter(Boolean).join(' · ') : 'No account samples are available for this filter.',
    ...plot,
    ...activityView(payload?.activity, payload?.range),
    choices, selection: { providerId, accountId, metricKey: chosenMetric?.key || '' },
  };
}
