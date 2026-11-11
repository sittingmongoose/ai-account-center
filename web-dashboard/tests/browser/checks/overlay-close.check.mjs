// Every overlay's close control must stay inside the viewport minus the safe insets, off the rail,
// and close on a tap (review J1). Runs in every REAL-DEVICE-INFO pose: the iPhone portrait and
// both landscapes, and the Duo folded, folded turned both ways, open upright and open landscape.
// NOT YET RUN: browsers were reserved for PM until 12:30Z on 2026-11-11; the device pass after
// runs this first. Screen option as frames/checks/frame.check.mjs: Duo poses report 466x678.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { analyticsFixture } from '../device/analytics-fixture.mjs';
import { fakeServer, serve } from '../accounts-fixture.mjs';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../../leptos');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', name), 'utf8'));
const TYPES = { '.jpg': 'image/jpeg', '.png': 'image/png', '.mp4': 'video/mp4', '.webp': 'image/webp' };
const DUO_SCREEN = { width: 466, height: 678 };

const POSES = [
	{ name: 'iphone-portrait', w: 402, h: 812, safe: '0,0,34,0', mobile: true, duo: false },
	{ name: 'iphone-landscape', w: 812, h: 402, safe: '0,62,21,62', mobile: true, duo: false },
	{ name: 'iphone-landscape-2', w: 874, h: 402, safe: '0,62,21,62', mobile: true, duo: false },
	{ name: 'duo-folded', w: 466, h: 678, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-right', w: 678, h: 466, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-left', w: 678, h: 466, safe: '0,0,34,84', mobile: true, duo: true },
	{ name: 'duo-open-upright', w: 669, h: 869, safe: '0,0,34,0', mobile: true, duo: true },
	{ name: 'duo-open-landscape', w: 951, h: 669, safe: '0,84,34,0', mobile: true, duo: true },
];

async function poseContext(t, p) {
	await t.newContext({ viewport: { width: p.w, height: p.h }, ...(p.duo ? { screen: DUO_SCREEN } : {}), isMobile: p.mobile, hasTouch: p.mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
}

/** Asserts the close rect is inside minus safe, off the rail, then taps it and waits for `gone` to match.
 * `skipBottom` for bottom sheets (` .pop` in sheet mode): the panel background touches the bottom
 * by design, so only top/left/right are checked (the grabber position is the panel top). */
async function assertClose(t, label, closeSel, goneSel, safe, closeClick = 'click', opts = {}) {
	const s = await t.page.evaluate((sel) => {
		const b = (el) => { const r = el.getBoundingClientRect(); return { x: r.x, y: r.y, r: r.right, b: r.bottom }; };
		const el = document.querySelector(sel);
		if (!el) return null;
		const cs = getComputedStyle(el);
		if (cs.display === 'none' || cs.visibility === 'hidden') return null;
		const railEl = document.querySelector('.rail');
		const rail = railEl && getComputedStyle(railEl).display !== 'none' ? b(railEl) : null;
		const st = getComputedStyle(document.documentElement);
		const num = (v) => parseFloat(v) || 0;
		return { close: b(el), rail, w: innerWidth, h: innerHeight, st: num(st.getPropertyValue('--safe-top')), sr: num(st.getPropertyValue('--safe-right')), sb: num(st.getPropertyValue('--safe-bottom')), sl: num(st.getPropertyValue('--safe-left')) };
	}, closeSel);
	await t.check(`${label}: close control is on screen`, !!s, `missing ${closeSel}`);
	if (!s) return;
	const c = s.close;
	const inside = c.x >= s.sl - 0.5 && c.r <= s.w - s.sr + 0.5 && c.y >= s.st - 0.5 && (opts.skipBottom || c.b <= s.h - s.sb + 0.5);
	await t.check(`${label}: close inside minus safe`, inside, `close ${Math.round(c.x)},${Math.round(c.y)} ${Math.round(c.r)},${Math.round(c.b)} in ${s.w}x${s.h} safe ${s.st},${s.sr},${s.sb},${s.sl}`);
	if (s.rail) {
		const overlap = c.x < s.rail.r - 0.5 && c.r > s.rail.x + 0.5 && c.y < s.rail.b - 0.5 && c.b > s.rail.y + 0.5;
		await t.check(`${label}: close off the rail`, !overlap, `close ${Math.round(c.x)}-${Math.round(c.r)} rail ${Math.round(s.rail.x)}-${Math.round(s.rail.r)}`);
	}
	if (closeClick === 'escape') await t.page.keyboard.press('Escape');
	else await t.page.click(closeSel);
	await t.until(`${label}: tap closes`, () => !document.querySelector(goneSel), null, { timeout: 5000 });
}

async function stubMedia(t) {
	await t.page.route((u) => new URL(u).pathname.startsWith('/api/status/files/'), async (r) => {
		let name = path.basename(new URL(r.request().url()).pathname);
		if (!fs.existsSync(path.join(LEPTOS, 'fixtures', 'media', name))) name = /\.(mp4|webm)$/i.test(name) ? 'option-b.mp4' : 'option-a.jpg';
		const body = fs.readFileSync(path.join(LEPTOS, 'fixtures', 'media', name));
		const type = TYPES[path.extname(name)] || 'application/octet-stream';
		const range = /bytes=(\d+)-(\d*)/.exec(r.request().headers().range || '');
		if (!range) return r.fulfill({ status: 200, body, headers: { 'content-type': type, 'accept-ranges': 'bytes' } });
		const start = Number(range[1]);
		const end = range[2] ? Math.min(Number(range[2]), body.length - 1) : body.length - 1;
		await r.fulfill({ status: 206, body: body.subarray(start, end + 1), headers: { 'content-type': type, 'accept-ranges': 'bytes', 'content-range': `bytes ${start}-${end}/${body.length}` } });
	});
}

async function openStatus(t, view, safe) {
	await t.app.signIn();
	await t.page.route((u) => new URL(u).pathname === '/api/status', (r) => r.fulfill({ json: view }));
	await t.page.route((u) => new URL(u).pathname === '/api/status/answers', (r) => r.fulfill({ json: { results: [] } }));
	await stubMedia(t);
	await t.goto(`/app/status?e2e&safe=${safe}`, { settleMs: 300 });
	await t.page.waitForSelector('#cards');
}

async function openBoard(t, safe) {
	await t.app.signIn();
	const view = { ...fixture('scoreboard-view.json'), byModel: fixture('scoreboard-by-model.json') };
	await t.page.route((u) => new URL(u).pathname === '/api/scoreboard', (r) => r.fulfill({ json: view }));
	await t.goto(`/app/scoreboard?e2e&safe=${safe}`, { settleMs: 300 });
	await t.page.waitForSelector('.picks');
}

const at = (minutes) => new Date(Date.now() + minutes * 60_000).toISOString();
const quota = (key, label, used, minutes, reset) => ({ key, label, usedPercent: used, remainingPercent: 100 - used, resetAt: at(reset), windowMinutes: minutes });
const account = (id, provider, email, plan, windows, extra = {}) => ({
	id, provider, providerLabel: provider, label: email, email, plan, platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok', message: null,
	fetchedAt: at(-2), sampledAt: at(-2), isActive: false, hidden: false, trayHidden: false, windows, capabilities: {}, signInNeeded: [], ...extra,
});
const ACCOUNTS = [
	account('claude-1', 'claude', 'a.very.long.example.name@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 9, 300, 160), quota('seven_day', 'Weekly usage', 93, 10080, 3500)]),
	account('codex-1', 'codex', 'codex.primary.workspace@example.invalid', 'pro', [quota('five_hour', '5h', 55, 300, 130), quota('seven_day', 'week', 44, 10080, 2300)], { isActive: true }),
];
const DASHBOARD = { schemaVersion: 1, updatedAt: at(-1), accounts: ACCOUNTS, providers: [], settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] }, codexAutoSwitch: { enabled: true, thresholdPercent: 10 } };

async function openAnalytics(t, safe) {
	await t.app.signIn();
	await t.app.stub('/api/accounts/dashboard', DASHBOARD);
	await t.page.route((u) => new URL(u).pathname === '/api/accounts/analytics', (r) => r.fulfill({ json: analyticsFixture({ range: new URL(r.request().url()).searchParams.get('range') || '7d', accounts: ACCOUNTS }) }));
	await t.goto(`/app/analytics?e2e&safe=${safe}`, { settleMs: 300 });
	await t.until('Analytics drawn', () => document.querySelectorAll('.ax-page .kpi').length === 5, null, { timeout: 60000, hard: true });
}

async function openAccounts(t, safe) {
	const server = fakeServer();
	await t.app.signIn();
	await serve(t, server);
	await t.goto(`/app/accounts?e2e${safe ? `&safe=${safe}` : ''}`, { settleMs: 300 });
	await t.until('Accounts drawn', () => document.querySelectorAll('.prov[data-provider]').length >= 9, null, { timeout: 45000, hard: true });
	return server;
}

async function accountButton(t) {
	const chrome = await t.page.evaluate(() => document.documentElement.dataset.chrome);
	if (chrome === 'rail') return '.rail button[aria-label="Account"]';
	if (chrome === 'phone' || chrome === 'band') return '.topbar .icon-btn[aria-label="Account"]';
	return '.chrome .select-btn[aria-label="Account"]';
}

export const tests = {
	async 'overlay-close: the Status pin sheet closes in every pose'(t) {
		const design = { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json'), generatedAt: new Date().toISOString() };
		for (const p of POSES) {
			await t.step(p.name, async () => {
				await poseContext(t, p);
				await openStatus(t, design, p.safe);
				const label = `${p.name} pin sheet`;
				const pin = await t.page.$('.pin-row');
				await t.check(`${label}: a pinned thread to open`, !!pin);
				if (!pin) return;
				await t.page.click('.pin-row');
				await t.page.waitForSelector('.sheet-host .sheet', { timeout: 5000 });
				await assertClose(t, label, '.sheet-host .sheet-close', '.sheet-host .sheet', p.safe);
			});
		}
	},

	async 'overlay-close: the Scoreboard model sheet closes in every pose'(t) {
		for (const p of POSES) {
			await t.step(p.name, async () => {
				await poseContext(t, p);
				await openBoard(t, p.safe);
				const label = `${p.name} model sheet`;
				await t.page.click('.sbm >> nth=0');
				await t.page.waitForSelector('.sheet-host .sheet', { timeout: 5000 });
				await assertClose(t, label, '.sheet-host .sheet-close', '.sheet-host .sheet', p.safe);
			});
		}
	},

	async 'overlay-close: the Analytics popover closes in every pose'(t) {
		for (const p of POSES) {
			await t.step(p.name, async () => {
				await poseContext(t, p);
				await openAnalytics(t, p.safe);
				const label = `${p.name} popover`;
				await t.page.click('.prov-btn');
				await t.page.waitForSelector('.pop', { timeout: 5000 });
				// Sheet mode (no inline left): measure the panel itself; its background touches the
				// bottom by design, so only top/left/right are checked (the grab position is the top).
				const sheet = await t.page.evaluate(() => !(document.querySelector('.pop').getAttribute('style') || '').includes('left'));
				await assertClose(t, label, '.pop', '.pop', p.safe, 'escape', { skipBottom: sheet });
			});
		}
	},

	async 'overlay-close: Details, the switch approval and the account menu close in every pose'(t) {
		for (const p of POSES) {
			await t.step(p.name, async () => {
				await poseContext(t, p);
				const server = await openAccounts(t, p.safe);
				const row = '.acc-item[data-row="codex:two"] .acc-email';
				await t.page.click(row);
				await t.until('Details shows the account', () => document.querySelector('.details .dt-title')?.textContent === 'codex.second@example.invalid', null, { timeout: 10000 });
				await assertClose(t, `${p.name} details`, '[data-probe="details-close"]', '.details', p.safe);
				await t.page.click('.acc-item[data-row="codex:two"] [data-probe="activate:codex:two"]');
				await t.until('the approval shows', () => !!document.querySelector('.adialog'), null, { timeout: 15000 });
				await assertClose(t, `${p.name} approval`, '.adialog .a-btns .btn:last-of-type', '.adialog', p.safe);
				const btn = await accountButton(t);
				await t.page.click(btn);
				await t.page.waitForSelector('.acct-menu', { timeout: 5000 });
				await assertClose(t, `${p.name} account menu`, '.acct-menu .menu-item:last-of-type', '.acct-menu', p.safe, 'escape');
				await t.page.click(btn);
				await t.page.waitForSelector('.acct-menu', { timeout: 5000 });
				await t.page.click('.menu-scrim');
				await t.until(`${p.name} account menu: scrim tap closes`, () => !document.querySelector('.acct-menu'), null, { timeout: 5000 });
			});
		}
	},

	async 'overlay-close: the image viewer closes in every pose'(t) {
		const view = { ...fixture('status-view.json'), spendByStream: fixture('spend-by-stream.json') };
		for (const p of POSES) {
			await t.step(p.name, async () => {
				await poseContext(t, p);
				await openStatus(t, view, p.safe);
				await t.page.click('.media-open >> nth=0');
				await t.page.waitForSelector('.viewer', { timeout: 5000 });
				await assertClose(t, `${p.name} viewer`, '.viewer-btn.close', '.viewer', p.safe);
			});
		}
	},
};

