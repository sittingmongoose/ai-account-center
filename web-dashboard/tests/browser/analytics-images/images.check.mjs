// Images for review: the Analytics page in the app frame (one whole-page shot) on made-up data
// (../device/analytics-fixture.mjs), at 1024, 1440 and 2560 wide and in the phone and Duo formats, light and dark.
// Writes leptos-*.png into `--set out=<folder>`. (Until the classic Slint app was retired, its page was shot and set
// beside each one.) Test data only: invented names.
import fs from 'node:fs';
import path from 'node:path';
import { analyticsFixture } from '../device/analytics-fixture.mjs';

const at = (minutes) => new Date(Date.now() + minutes * 60_000).toISOString();
const quota = (key, label, used, minutes, reset) => ({ key, label, usedPercent: used, remainingPercent: 100 - used, resetAt: at(reset), windowMinutes: minutes });
const account = (id, provider, email, plan, windows, extra = {}) => ({
	id, provider, providerLabel: provider, label: email, email, plan, platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok', message: null,
	fetchedAt: at(-2), sampledAt: at(-2), isActive: false, hidden: false, trayHidden: false, windows, capabilities: {}, signInNeeded: [], ...extra,
});
const ACCOUNTS = [
	account('claude-1', 'claude', 'a.very.long.example.name@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 9, 300, 160), quota('seven_day', 'Weekly usage', 93, 10080, 3500)]),
	account('claude-2', 'claude', 'second.claude.account@example.invalid', 'max', [quota('five_hour', 'Five-hour usage', 100, 300, 210), quota('seven_day', 'Weekly usage', 7, 10080, 9000)]),
	account('codex-1', 'codex', 'codex.primary.workspace@example.invalid', 'pro', [quota('five_hour', '5h', 55, 300, 130), quota('seven_day', 'week', 44, 10080, 2300)], { isActive: true }),
	account('codex-2', 'codex', 'codex.second@example.invalid', 'plus', [quota('five_hour', '5h', 12, 300, 250), quota('seven_day', 'week', 31, 10080, 7000)]),
	account('kimi-1', 'kimi-code', 'kimi.code.example@example.invalid', 'pro', [quota('weekly', 'Weekly usage', 63, 10080, 4000)]),
	account('zai-1', 'zai', 'zai.coding.plan.example@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 27, 300, 120)]),
];
const DASHBOARD = { schemaVersion: 1, updatedAt: at(-1), accounts: ACCOUNTS, providers: [], settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] }, codexAutoSwitch: { enabled: true, thresholdPercent: 10 } };

const DUO_SCREEN = { width: 466, height: 678 };
const FORMATS = [
	{ name: '1024', width: 1024, height: 800, safe: '0,0,0,0', mobile: false },
	{ name: '1440', width: 1440, height: 900, safe: '0,0,0,0', mobile: false },
	{ name: '2560', width: 2560, height: 1300, safe: '0,0,0,0', mobile: false },
	{ name: 'iphone', width: 402, height: 812, safe: '0,0,34,0', mobile: true },
	{ name: 'duo-folded', width: 466, height: 678, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-open-upright', width: 669, height: 869, safe: '0,0,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-right', width: 678, height: 466, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-left', width: 678, height: 466, safe: '0,0,34,84', mobile: true, duo: true },
	{ name: 'duo-landscape', width: 951, height: 669, safe: '0,84,34,0', mobile: true, duo: true },
];
const UNPIN = `html { scrollbar-width: none; }
.rail, .tabbar, .topbar, .chrome, .band-head { position: static !important; }
.rail { height: auto !important; align-self: stretch !important; }`;

async function prepare(t, f, theme) {
	await t.newContext({ viewport: { width: f.width, height: f.height }, ...(f.duo ? { screen: DUO_SCREEN } : {}), isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
	await t.page.addInitScript((mode) => { try { localStorage.setItem('aac-theme', mode); } catch {} }, theme);
	await t.app.signIn();
	await t.app.stub('/api/accounts/dashboard', DASHBOARD);
	await t.page.route((u) => new URL(u).pathname === '/api/accounts/analytics', (r) => {
		const range = new URL(r.request().url()).searchParams.get('range') || '7d';
		return r.fulfill({ json: analyticsFixture({ range, accounts: ACCOUNTS }) });
	});
}

/** The native page, whole: the chrome unpinned so it sits once at its place. */
async function leptos(t, f, theme) {
	await prepare(t, f, theme);
	await t.goto(`/app/analytics?e2e&safe=${f.safe}`, { settleMs: 300 });
	await t.until('analytics drawn', () => document.querySelectorAll('.ax-page .kpi').length === 5 && !!document.querySelector('.trend-svg path[d^="M"]'), null, { timeout: 60000, hard: true });
	await t.page.waitForTimeout(2500);
	await t.page.addStyleTag({ content: UNPIN });
	await t.page.waitForTimeout(300);
	return t.page.screenshot({ fullPage: true });
}

export const tests = Object.fromEntries(
	FORMATS.flatMap((f) =>
		['light', 'dark'].map((theme) => [
			`analytics ${f.name} ${theme}`,
			async (t) => {
				const out = t.args.out || path.join(process.cwd(), 'analytics-images-out');
				fs.mkdirSync(out, { recursive: true });
				const file = path.join(out, `leptos-${f.name}-${theme}.png`);
				fs.writeFileSync(file, await leptos(t, f, theme));
				await t.check(`${f.name} ${theme}: saved`, fs.statSync(file).size > 20000);
				await t.check(`${f.name} ${theme}: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
			},
		])
	)
);
