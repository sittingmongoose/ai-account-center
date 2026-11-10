// Made-up Analytics data (GET /api/accounts/analytics, schemaVersion 1) for the device checks, the browser kit and
// the Leptos page's fixtures. Invented accounts and numbers only, shaped like
// src/web-server/services/account-analytics-types.ts. Deterministic for a given `now` (a seeded generator), and every
// model's cost is its tokens at the page's own rates, so the cost split reconciles.
//   analyticsFixture({ range: '7d', now: Date.now(), accounts })   accounts: the dashboard accounts to give history
import { STATIC_RATES } from '../../../public/model-rates.mjs';

const H = 3600_000;
const D = 24 * H;
const iso = (ms) => new Date(ms).toISOString().replace('.000Z', 'Z');

function seeded(seed) {
	let s = seed >>> 0 || 1;
	return () => {
		s ^= s << 13; s >>>= 0;
		s ^= s >>> 17;
		s ^= s << 5; s >>>= 0;
		return s / 4294967296;
	};
}

// provider, model, tool, share of the day's tokens, cache-read factor
const MODELS = [
	['claude', 'claude-opus-4-7', 'claude', 0.30, 9],
	['claude', 'claude-sonnet-4-6', 'claude', 0.12, 7],
	['claude', 'claude-haiku-4-5', 'claude', 0.04, 4],
	['codex', 'gpt-5-codex', 'codex', 0.22, 6],
	['codex', 'gpt-5', 'codex', 0.06, 5],
	['zai', 'glm-5.3', 'zcode', 0.08, 5],
	['qwen', 'qwen3-coder-plus', 'omp', 0.05, 3],
	['kimi-code', 'kimi-k3', 'omp', 0.05, 4],
	['muse', 'muse-spark', 'muse', 0.03, 2],
	['other', 'local-vllm-qwen', 'omp', 0.02, 0],
];
const HOSTS = ['ubuntu', 'mac', 'windows', 'nas1'];
const TOOLS = ['claude', 'codex', 'omp', 'muse', 'zcode', 'cursor'];

function rates(model) {
	const r = STATIC_RATES[model];
	return r ? { inputPerMillion: r[0], outputPerMillion: r[1], cacheCreationPerMillion: r[2], cacheReadPerMillion: r[3], source: 'static' } : null;
}
function cost(model, t) {
	const r = STATIC_RATES[model];
	if (!r) return 0;
	return (t.inputTokens * r[0] + t.outputTokens * r[1] + t.cacheCreationTokens * r[2] + t.cacheReadTokens * r[3]) / 1e6;
}
function zero() {
	return { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0, estimatedCostUsd: 0, fallbackCostUsd: 0 };
}
function add(into, t) {
	into.inputTokens += t.inputTokens; into.outputTokens += t.outputTokens; into.cacheCreationTokens += t.cacheCreationTokens;
	into.cacheReadTokens += t.cacheReadTokens; into.estimatedCostUsd += t.estimatedCostUsd; into.fallbackCostUsd += t.fallbackCostUsd || 0;
	return into;
}
const totalsOut = (t) => ({ ...t, costByType: null, costByTypeReconciled: false });

/** Hour-of-day activity: quiet nights, a working-day hump, lighter weekends. */
function intensity(ms, rand) {
	const date = new Date(ms);
	const h = date.getUTCHours();
	const day = date.getUTCDay();
	const work = Math.exp(-((h - 15) ** 2) / 18) + 0.35 * Math.exp(-((h - 21) ** 2) / 6);
	const weekend = day === 0 || day === 6 ? 0.45 : 1;
	return Math.max(0, work * weekend * (0.55 + rand() * 0.9));
}

export function analyticsFixture({ range = '7d', now = Date.now(), accounts = [] } = {}) {
	const span = range === '24h' ? D : range === '30d' ? 30 * D : 7 * D;
	const to = Math.floor(now / H) * H + H;
	const from = to - span;
	const rand = seeded(Math.floor(now / D));
	const byHour = [];
	const modelTotals = new Map();
	const providerTotals = new Map();
	const dayModel = new Map();
	const dayProvider = new Map();
	for (let t = from; t < to; t += H) {
		const k = intensity(t, rand);
		if (k < 0.04) continue;
		const perProvider = new Map();
		for (const [provider, model, tool, share, cacheK] of MODELS) {
			if (rand() > 0.82) continue;
			const base = 2.4e6 * k * share * (0.6 + rand() * 0.8);
			if (base < 2000) continue;
			const tok = {
				inputTokens: Math.round(base),
				outputTokens: Math.round(base * (0.06 + rand() * 0.08)),
				cacheCreationTokens: provider === 'claude' ? Math.round(base * 0.35) : 0,
				cacheReadTokens: Math.round(base * cacheK * (0.7 + rand() * 0.6)),
				estimatedCostUsd: 0,
				fallbackCostUsd: 0,
			};
			tok.estimatedCostUsd = cost(model, tok);
			if (!STATIC_RATES[model]) tok.fallbackCostUsd = 0;
			const mk = `${provider}|${model}`;
			const mt = modelTotals.get(mk) || { provider, model, tool, ...zero() };
			add(mt, tok);
			modelTotals.set(mk, mt);
			add(perProvider.get(provider) || perProvider.set(provider, zero()).get(provider), tok);
			const day = iso(t).slice(0, 10);
			const dk = `${day}|${provider}|${model}`;
			add(dayModel.get(dk) || dayModel.set(dk, { date: day, provider, model, ...zero() }).get(dk), tok);
		}
		for (const [provider, tok] of perProvider) {
			byHour.push({ hour: iso(t), provider, ...totalsOut(tok), requestCount: Math.max(1, Math.round(tok.inputTokens / 40000)) });
			add(providerTotals.get(provider) || providerTotals.set(provider, zero()).get(provider), tok);
			const day = iso(t).slice(0, 10);
			const dk = `${day}|${provider}`;
			const row = dayProvider.get(dk) || dayProvider.set(dk, { date: day, provider, ...zero(), requestCount: 0 }).get(dk);
			add(row, tok); row.requestCount += Math.max(1, Math.round(tok.inputTokens / 40000));
		}
	}
	const totals = zero();
	for (const t of providerTotals.values()) add(totals, t);
	const order = ['claude', 'codex', 'antigravity', 'cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go', 'other'];
	const label = { claude: 'Claude', codex: 'Codex', zai: 'Z.ai Coding Plan', qwen: 'Qwen Token Plan', 'kimi-code': 'Kimi Code', muse: 'Muse Code', other: 'Other', cursor: 'Cursor' };
	const toolsOf = (provider) => [...new Set(MODELS.filter((m) => m[0] === provider).map((m) => m[2]))];
	const providers = [...providerTotals.entries()]
		.sort((a, b) => order.indexOf(a[0]) - order.indexOf(b[0]))
		.map(([provider, t]) => ({ provider, label: label[provider] || provider, totals: totalsOut(t), usageEvents: Math.round(t.inputTokens / 30000), sessionCount: Math.max(1, Math.round(t.inputTokens / 4e6)), tools: toolsOf(provider) }));
	const models = [...modelTotals.values()]
		.sort((a, b) => b.estimatedCostUsd - a.estimatedCostUsd)
		.map(({ provider, model, tool, ...t }) => ({ model, provider, ...totalsOut(t), rates: rates(model), tools: [tool] }));
	const sessions = [];
	for (let i = 0; i < 14; i++) {
		const m = MODELS[i % MODELS.length];
		const base = 4e6 * (0.3 + rand() * 2);
		const t = { inputTokens: Math.round(base), outputTokens: Math.round(base * 0.08), cacheCreationTokens: m[0] === 'claude' ? Math.round(base * 0.3) : 0, cacheReadTokens: Math.round(base * m[4]), fallbackCostUsd: 0 };
		t.estimatedCostUsd = cost(m[1], t);
		sessions.push({ key: (0x1000_0000_0000 + i * 7919).toString(16).padStart(16, '0'), provider: m[0], lastActivity: iso(to - (i * span) / 16 - rand() * H), models: [m[1]], target: null, ...totalsOut(t) });
	}
	const sources = [];
	for (const tool of TOOLS) for (const host of HOSTS) {
		const pick = rand();
		const state = tool === 'cursor' ? 'not_installed'
			: host === 'ubuntu' ? 'ok'
				: pick < 0.55 ? 'ok' : pick < 0.7 ? 'no_usage' : pick < 0.8 ? 'cached' : pick < 0.9 ? 'not_installed' : pick < 0.95 ? 'scanning' : 'unavailable';
		sources.push({
			tool, host, state,
			lastScanAt: ['ok', 'cached', 'no_usage'].includes(state) ? iso(now - (2 + rand() * 50) * 60_000) : null,
			rowCount: state === 'ok' ? Math.round(200 + rand() * 4000) : 0,
			detail: tool === 'cursor' ? 'Cursor keeps no local usage log.' : state === 'unavailable' ? 'The computer did not answer' : state === 'cached' ? 'The last scan ran out of time' : null,
		});
	}
	const historyAccounts = accounts.map((account) => ({
		...account,
		switchable: ['codex', 'antigravity'].includes(account.provider),
		hidden: false,
		firstSampleAt: iso(from),
		lastSampleAt: iso(now - 2 * 60_000),
		sampleCount: 0,
		windows: (account.windows || []).map((window) => {
			const points = [];
			const minutes = window.windowMinutes || 10080;
			const resetEvery = minutes * 60_000;
			const resetAt = Date.parse(window.resetAt) || now + resetEvery / 2;
			for (let t = from; t <= now; t += Math.max(H, span / 160)) {
				// the window runs to its reset and starts again from zero after it
				let next = resetAt;
				while (next - resetEvery > t) next -= resetEvery;
				const through = 1 - (next - t) / resetEvery;
				const used = Math.min(100, Math.max(0, window.usedPercent * Math.min(1, Math.max(0, through / Math.max(0.05, 1 - (resetAt - now) / resetEvery))) * (0.92 + rand() * 0.1)));
				const gap = rand() < 0.02;
				points.push({
					sampledAt: iso(t), observedAt: iso(t), usedPercent: gap ? null : Math.round(used * 10) / 10, remainingPercent: gap ? null : Math.round((100 - used) * 10) / 10,
					used: null, limit: null, remaining: null, resetAt: iso(next), expiresAt: null, status: gap ? 'unavailable' : 'ok', source: 'fixture', platform: 'ubuntu', isActive: account.isActive === true,
				});
			}
			return { ...window, points };
		}),
	}));
	for (const account of historyAccounts) account.sampleCount = account.windows.reduce((n, w) => n + w.points.length, 0);
	const providerIds = [...new Set([...historyAccounts.map((a) => a.provider), ...providers.map((p) => p.provider).filter((p) => p !== 'other')])];
	return {
		schemaVersion: 1,
		updatedAt: iso(now),
		range: { preset: range, from: iso(from), to: iso(to), bucketMinutes: 60, tz: 'UTC', dayBucketTz: 'UTC', availableFrom: iso(now - 31 * D) },
		filters: { platform: 'mac', provider: 'all', account: 'all' },
		history: { status: 'ok', retentionDays: 90, collectedSince: iso(now - 60 * D), oldestSampleAt: iso(from), newestSampleAt: iso(now - 2 * 60_000), sampleCount: historyAccounts.reduce((n, a) => n + a.sampleCount, 0), message: '' },
		summary: { accountCount: historyAccounts.length, availableAccounts: historyAccounts.length, activeCodexAccountId: historyAccounts.find((a) => a.provider === 'codex' && a.isActive)?.id ?? null, activeAccountIds: { codex: historyAccounts.find((a) => a.provider === 'codex' && a.isActive)?.id ?? null }, sampleCount: 0 },
		providers: providerIds.sort((a, b) => order.indexOf(a) - order.indexOf(b)).map((provider, i) => ({
			provider, label: label[provider] || provider, order: i, visible: true,
			accountCount: historyAccounts.filter((a) => a.provider === provider).length, availableAccounts: historyAccounts.filter((a) => a.provider === provider).length,
			latestSampleAt: iso(now - 2 * 60_000), hasQuotaHistory: historyAccounts.some((a) => a.provider === provider), hasActivity: providerTotals.has(provider),
		})),
		accounts: historyAccounts,
		activity: {
			status: 'ok', refreshing: false, scope: 'multi-host-cli', timezone: 'UTC', accountAttribution: 'unavailable', costBasis: 'estimated-api-equivalent',
			fetchedAt: iso(now - 90_000), message: '', totals: totalsOut(totals), providers,
			byDay: [...dayProvider.values()].map((row) => totalsOut(row)),
			byHour, models,
			byDayModel: [...dayModel.values()].map((row) => totalsOut(row)),
			sessions: { total: 38, sample: sessions, truncated: true },
			anomalies: { thresholds: { costSpikeMultiplier: 2, highInputTokens: 10_000_000, highIoRatio: 100, highCacheReadTokens: 1_000_000_000 }, items: [], summary: { totalAnomalies: 0, highInputDays: 0, highIoRatioDays: 0, costSpikeDays: 0, highCacheReadDays: 0 } },
			sources,
		},
	};
}
