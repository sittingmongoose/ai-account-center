// Side-by-side images for review: the Slint Analytics page (the classic app, its canvas scrolled screen by screen and
// stacked) beside the native Analytics page in the app frame (one whole-page shot), on the same made-up data
// (../device/analytics-fixture.mjs), at 1024, 1440 and 2560 wide and in the phone and Duo formats, light and dark.
// Writes slint-*, leptos-* and side-*.png into `--set out=<folder>`. Test data only: invented names.
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

const FORMATS = [
	{ name: '1024', width: 1024, height: 800, safe: '0,0,0,0', mobile: false },
	{ name: '1440', width: 1440, height: 900, safe: '0,0,0,0', mobile: false },
	{ name: '2560', width: 2560, height: 1300, safe: '0,0,0,0', mobile: false },
	{ name: 'iphone', width: 402, height: 874, safe: '62,0,34,0', mobile: true },
	{ name: 'duo-folded', width: 466, height: 678, safe: '0,84,34,0', mobile: true },
	{ name: 'duo-portrait', width: 669, height: 951, safe: '82,0,34,0', mobile: true },
	{ name: 'duo-landscape', width: 951, height: 669, safe: '0,84,34,0', mobile: true },
];
const UNPIN = `html { scrollbar-width: none; }
.rail, .tabbar, .topbar, .chrome, .band-head { position: static !important; }
.rail { height: auto !important; align-self: stretch !important; }`;

async function prepare(t, f, theme) {
	await t.newContext({ viewport: { width: f.width, height: f.height }, isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
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

/** The Slint page: the canvas scrolls inside itself, so each screen is shot and they are stacked. */
async function slint(t, f, theme) {
	await prepare(t, f, theme);
	await t.goto(`/analytics?e2e${f.mobile ? `&safe=${f.safe}` : ''}`, { settleMs: 300 });
	await t.until('Slint analytics drawn', () => !!window.__aacLastAnalytics?.usage?.ready && !!document.querySelector('#loading')?.hidden, null, { timeout: 60000, hard: true });
	await t.page.waitForTimeout(3000);
	const shots = [];
	await t.page.mouse.move(f.width / 2, f.height * 0.6);
	for (let i = 0; i < 8; i++) {
		shots.push(await t.page.screenshot());
		const before = await t.page.screenshot({ clip: { x: 0, y: f.height * 0.3, width: f.width, height: f.height * 0.4 } });
		await t.page.mouse.wheel(0, Math.round(f.height * 0.7));
		await t.page.waitForTimeout(900);
		const after = await t.page.screenshot({ clip: { x: 0, y: f.height * 0.3, width: f.width, height: f.height * 0.4 } });
		if (Buffer.compare(before, after) === 0) break;
	}
	return shots;
}

/** One image: Slint's screens stacked on the left, the native page on the right, each under its name. */
async function side(t, f, theme, slintShots, leptosShot, file) {
	const img = (b) => `<img src="data:image/png;base64,${b.toString('base64')}">`;
	await t.page.setViewportSize({ width: f.width * 2 + 72, height: 600 });
	await t.page.setContent(`<!doctype html><style>
		body { margin: 0; background: ${theme === 'dark' ? '#05080B' : '#9AA6B1'}; font: 600 22px system-ui; color: ${theme === 'dark' ? '#E6ECF2' : '#15202B'}; }
		.row { display: flex; gap: 24px; padding: 24px; align-items: flex-start; }
		figure { margin: 0; display: grid; gap: 8px; } img { display: block; width: ${f.width}px; } .stack { display: grid; gap: 6px; }
	</style><div class="row"><figure><figcaption>Slint (classic), ${f.name} ${theme}: ${slintShots.length} screens of its scrolling canvas</figcaption><div class="stack">${slintShots.map(img).join('')}</div></figure>
	<figure><figcaption>Leptos (app frame), ${f.name} ${theme}: whole page</figcaption>${img(leptosShot)}</figure></div>`);
	await t.page.waitForTimeout(300);
	await t.page.screenshot({ path: file, fullPage: true });
}

export const tests = Object.fromEntries(
	FORMATS.flatMap((f) =>
		['light', 'dark'].map((theme) => [
			`analytics ${f.name} ${theme}`,
			async (t) => {
				const out = t.args.out || path.join(process.cwd(), 'analytics-images-out');
				fs.mkdirSync(out, { recursive: true });
				const native = await leptos(t, f, theme);
				fs.writeFileSync(path.join(out, `leptos-${f.name}-${theme}.png`), native);
				const screens = await slint(t, f, theme);
				screens.forEach((b, i) => fs.writeFileSync(path.join(out, `slint-${f.name}-${theme}-${i + 1}.png`), b));
				const file = path.join(out, `side-${f.name}-${theme}.png`);
				await side(t, f, theme, screens, native, file);
				await t.check(`${f.name} ${theme}: side by side saved`, fs.statSync(file).size > 20000);
			},
		])
	)
);
