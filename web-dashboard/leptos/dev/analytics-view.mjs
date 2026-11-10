// Builds the Analytics view (ANALYTICS_VIEW_VERSION 7, exactly as bridge.js renderAnalytics does) from the made-up
// fixture in tests/browser/device/analytics-fixture.mjs, for the Leptos page's tests and dev server.
//   node web-dashboard/leptos/dev/analytics-view.mjs [range] [trendW,trendH] > view.json
import { analyticsFixture } from '../../tests/browser/device/analytics-fixture.mjs';
import { setDisplayTimeZone } from '../../public/time-format.mjs';

process.env.TZ ||= 'UTC';
setDisplayTimeZone('UTC');
const { usageView, trendPaths } = await import('../../public/analytics-usage.mjs');
const { quotaView, agendaView } = await import('../../public/analytics-quota.mjs');
const { analyticsSlintModel } = await import('../../public/analytics-data.mjs');

const at = (now, minutes) => new Date(now + minutes * 60_000).toISOString();
export function demoDashboard(now) {
	const quota = (key, label, used, minutes, reset) => ({ key, label, usedPercent: used, remainingPercent: 100 - used, resetAt: at(now, reset), windowMinutes: minutes });
	const account = (id, provider, email, plan, windows, extra = {}) => ({
		id, provider, providerLabel: provider, label: email, email, plan, platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok', message: null,
		fetchedAt: at(now, -2), sampledAt: at(now, -2), isActive: false, hidden: false, trayHidden: false, windows, capabilities: {}, signInNeeded: [], ...extra,
	});
	const five = (u, r) => quota('five_hour', 'Five-hour usage', u, 300, r);
	const week = (u, r) => quota('seven_day', 'Weekly usage', u, 10080, r);
	return {
		schemaVersion: 1, updatedAt: at(now, -1),
		accounts: [
			account('claude-1', 'claude', 'a.very.long.example.name@example.invalid', 'pro', [five(9, 160), week(93, 3500)]),
			account('claude-2', 'claude', 'second.claude.account@example.invalid', 'max', [five(100, 210), week(7, 9000)]),
			account('codex-1', 'codex', 'codex.primary.workspace@example.invalid', 'pro', [quota('five_hour', '5h', 55, 300, 130), quota('seven_day', 'week', 44, 10080, 2300)], { isActive: true }),
			account('codex-2', 'codex', 'codex.second@example.invalid', 'plus', [quota('five_hour', '5h', 12, 300, 250), quota('seven_day', 'week', 31, 10080, 7000)]),
			account('kimi-1', 'kimi-code', 'kimi.code.example@example.invalid', 'pro', [quota('weekly', 'Weekly usage', 63, 10080, 4000)]),
			account('zai-1', 'zai', 'zai.coding.plan.example@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 27, 300, 120)]),
		],
		codexAutoSwitch: { enabled: true, thresholdPercent: 10 },
	};
}

export function buildView({ range = '7d', now = Date.now(), sizes = {}, state = {} } = {}) {
	const dashboard = demoDashboard(now);
	const payload = analyticsFixture({ range: range === 'all' || range === 'month' ? '30d' : range, now, accounts: dashboard.accounts });
	const page = { range, from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', cbmSort: 'cost', open: new Set(), compare: new Set(), collapsed: new Set(), donutOpen: new Set(), sizes: { trend: null, daily: null, heat: null, focus: null, ...sizes }, ...state };
	const usage = usageView(payload, page, { now, sizes: page.sizes });
	const ctx = { now: Math.floor(now / 60000) * 60000, dashboard, open: page.open, compare: page.compare, collapsed: page.collapsed, focusWidth: page.sizes.focus?.w, focusHeight: page.sizes.focus?.h, thresholds: { codex: 90, antigravity: null } };
	const quota = quotaView(payload, ctx);
	const agenda = agendaView(payload, ctx);
	return analyticsSlintModel(null, { usage, quota, agenda, state: page, paths: trendPaths(usage.trend.geo) });
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const [range = '7d', trend = ''] = process.argv.slice(2);
	const [w, h] = trend.split(',').map(Number);
	const sizes = w ? { trend: { w, h }, daily: { w, h: 160 }, heat: { w: Math.min(w, 700), h: 200 }, focus: { w, h: 200 } } : {};
	process.stdout.write(JSON.stringify(buildView({ range, sizes, now: Date.parse("2026-11-10T18:00:00Z") })));
}
