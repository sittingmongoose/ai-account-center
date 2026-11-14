// The app frame with each tab, in each format and theme: <tab>-<format>-<theme>.png in `--set out=<folder>`.
// Every tab is a whole page (the chrome unpinned so it sits once at its place, like the mockups' "Whole page" view):
// the dashboard only feeds Home, Analytics and Accounts from its hidden iframe.
// Synthetic data only: the Leptos design fixtures for Status and Scoreboard, and made-up accounts below.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { analyticsFixture } from '../device/analytics-fixture.mjs';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', name), 'utf8'));

/** Status and Scoreboard from the design fixtures (made-up data), as the earlier review images were drawn. */
async function stubPages(t) {
	const status = { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json'), generatedAt: new Date().toISOString() };
	const now = Date.now();
	status.cards.open = status.cards.open.map((c) => ({ ...c, askedAt: new Date(now - (c.askedAgo ?? 0) * 1000).toISOString(), ageSeconds: c.askedAgo ?? 0 }));
	const board = { ...fixture('scoreboard-view.json'), byModel: fixture('scoreboard-by-model.json') };
	await t.page.route((u) => new URL(u).pathname === '/api/status', (r) => r.fulfill({ json: status }));
	await t.page.route((u) => new URL(u).pathname === '/api/scoreboard', (r) => r.fulfill({ json: board }));
}

const DUO_SCREEN = { width: 466, height: 678 };
const FORMATS = [
	{ name: 'desktop', width: 1440, height: 900, safe: '0,0,0,0', mobile: false },
	{ name: 'iphone', width: 402, height: 812, safe: '0,0,34,0', mobile: true },
	{ name: 'duo-folded', width: 466, height: 678, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-open-upright', width: 669, height: 869, safe: '0,0,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-right', width: 678, height: 466, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-left', width: 678, height: 466, safe: '0,0,34,84', mobile: true, duo: true },
	{ name: 'duo-landscape', width: 951, height: 669, safe: '0,84,34,0', mobile: true, duo: true },
];
const TABS = ['status', 'scoreboard', 'home', 'analytics', 'accounts'];
const UNPIN = `html { scrollbar-width: none; }
.rail, .tabbar, .topbar, .chrome, .band-head { position: static !important; }
.rail { height: auto !important; align-self: stretch !important; }`;

const now = Date.now();
const at = (minutes) => new Date(now + minutes * 60_000).toISOString();
const quota = (key, label, used, minutes, reset) => ({ key, label, usedPercent: used, remainingPercent: 100 - used, resetAt: at(reset), windowMinutes: minutes });
const account = (id, provider, email, plan, windows, extra = {}) => ({
	id, provider, providerLabel: provider, label: email, email, plan, platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok', message: null,
	fetchedAt: at(-2), sampledAt: at(-2), isActive: false, hidden: false, trayHidden: false, windows, capabilities: {}, signInNeeded: [], ...extra,
});
const ACCOUNTS = {
	schemaVersion: 1,
	updatedAt: at(-1),
	accounts: [
		account('claude-1', 'claude', 'one@example.invalid', 'max', [quota('five_hour', 'Five-hour usage', 41, 300, 95), quota('seven_day', 'Weekly usage', 22, 10080, 3100)]),
		account('claude-2', 'claude', 'two@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 61, 300, 40), quota('seven_day', 'Weekly usage', 33, 10080, 5200)]),
		account('codex-1', 'codex', 'one@example.invalid', 'plus', [quota('five_hour', '5h', 55, 300, 130), quota('seven_day', 'week', 44, 10080, 2300)], { isActive: true }),
		account('codex-2', 'codex', 'two@example.invalid', 'pro', [quota('five_hour', '5h', 12, 300, 250), quota('seven_day', 'week', 31, 10080, 7000)]),
		account('cursor-1', 'cursor', 'one@example.invalid', 'pro', [quota('monthly', 'Monthly usage', 18, 43200, 20000)]),
	],
};

export const tests = Object.fromEntries(
	TABS.flatMap((tab) =>
		FORMATS.flatMap((f) =>
			['light', 'dark'].map((theme) => [
				`${tab} ${f.name} ${theme}`,
				async (t) => {
					await t.newContext({ viewport: { width: f.width, height: f.height }, ...(f.duo ? { screen: DUO_SCREEN } : {}), isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
					await t.page.addInitScript((mode) => { try { localStorage.setItem('aac-theme', mode); } catch {} }, theme);
					await t.app.signIn();
					await t.app.stub('/api/accounts/dashboard', ACCOUNTS);
					await stubPages(t);
					// Analytics (drawn by the frame): made-up usage logs and quota history for the same accounts
					await t.page.route((u) => new URL(u).pathname === '/api/accounts/analytics', (r) => r.fulfill({ json: analyticsFixture({ range: new URL(r.request().url()).searchParams.get('range') || '7d', accounts: ACCOUNTS.accounts }) }));
					await t.goto(`/app/${tab}?e2e&safe=${f.safe}`, { settleMs: 300 });
					await t.until(`${tab} shown`, (tab) => {
						const shown = [...document.querySelectorAll('.route')].filter((r) => !r.hidden);
						if (shown.length !== 1) return false;
						return !!shown[0].querySelector({ status: '.q-card', accounts: '.ac-page .prov', analytics: '.ax-page .kpi', home: '.home .hsec' }[tab] || '.picks');
					}, tab, { timeout: 60000, hard: true });
					// the dashboard draws its readings a moment after it is signed in
					await t.page.waitForTimeout(tab === 'analytics' ? 2500 : 400);
					await t.page.evaluate(() => document.activeElement?.blur?.());
					await t.page.addStyleTag({ content: UNPIN });
					await t.page.waitForTimeout(250);
					const out = t.args.out || path.join(process.cwd(), 'frame-images-out');
					fs.mkdirSync(out, { recursive: true });
					const file = path.join(out, `${tab}-${f.name}-${theme}.png`);
					await t.page.screenshot({ path: file, fullPage: true });
					await t.check(`${tab} ${f.name} ${theme}: saved`, fs.statSync(file).size > 5000);
					await t.check(`${tab} ${f.name} ${theme}: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
				},
			])
		)
	)
);
