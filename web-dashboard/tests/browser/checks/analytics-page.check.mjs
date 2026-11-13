// The native Analytics page in the app frame (PLAN step 7, web-dashboard/leptos/src/analytics/): the frame reads its own
// data (public/frame-analytics.mjs, no Slint dashboard), keeps the page state and builds the view with bridge.js's
// builders (ANALYTICS_VIEW_VERSION 7); the page draws it and sends its action kinds back. These check every block is drawn from made-up data
// (../device/analytics-fixture.mjs), that the controls reach bridge.js and come back drawn, that the charts' labels
// never overlap (DOM measurement), and the layout in each format with no sideways scroll.
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
	account('zai-1', 'zai', 'zai.coding.plan.example@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 27, 300, 120)]),
];
const DASHBOARD = { schemaVersion: 1, updatedAt: at(-1), accounts: ACCOUNTS, providers: [], settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] }, codexAutoSwitch: { enabled: true, thresholdPercent: 10 } };

const FORMATS = [
	{ name: 'desktop-1024', width: 1024, height: 800, safe: '0,0,0,0', mobile: false },
	{ name: 'desktop-1440', width: 1440, height: 900, safe: '0,0,0,0', mobile: false },
	{ name: 'desktop-2560', width: 2560, height: 1300, safe: '0,0,0,0', mobile: false },
	{ name: 'iphone', width: 402, height: 812, safe: '0,0,34,0', mobile: true },
	{ name: 'duo-folded', width: 466, height: 678, safe: '0,84,34,0', mobile: true },
	{ name: 'duo-open-upright', width: 669, height: 869, safe: '0,0,34,0', mobile: true },
	{ name: 'duo-folded-turned', width: 678, height: 466, safe: '0,84,34,0', mobile: true },
	{ name: 'duo-landscape', width: 951, height: 669, safe: '0,84,34,0', mobile: true },
];

async function stubData(t) {
	await t.app.stub('/api/accounts/dashboard', DASHBOARD);
	await t.page.route((u) => new URL(u).pathname === '/api/accounts/analytics', (r) => {
		const range = new URL(r.request().url()).searchParams.get('range') || '7d';
		return r.fulfill({ json: analyticsFixture({ range, accounts: ACCOUNTS }) });
	});
}

/** The Analytics tab drawn natively, with its first view. */
async function openAnalytics(t, safe = '') {
	await t.goto(`/app/analytics?e2e${safe ? `&safe=${safe}` : ''}`, { settleMs: 300 });
	await t.until('the frame is drawn, signed in', () => !!document.querySelector('.shell') && !document.querySelector('.signin-page'), null, { timeout: 30000, hard: true });
	await t.until('Analytics drawn from its view', () => {
		const page = document.querySelector('.ax-page');
		return !!page && page.querySelectorAll('.kpi').length === 5 && !!page.querySelector('.trend-svg path[d^="M"]') && page.querySelectorAll('.qh-group').length > 0;
	}, null, { timeout: 60000, hard: true });
}

/** Text boxes that overlap inside each chart (SVG and HTML labels), measured in the DOM. */
async function overlaps(t) {
	return t.page.evaluate(() => {
		const groups = {
			trend: '.trend-card svg.axes text',
			donut: '.donut-svg text',
			focus: '.focus-box svg text',
			heat: '.heat-grid .hh',
			daily: '.daily-box .db-l, .daily-box .dy .ax-t',
			quota: '.qh-ticks .cap',
			heads: '.sl-head .cap, .sp-head .cap, .rw-head .cap, .cbm-cols .cap, .qh-colhead > .cap, .pick-head .cap, .cal-wd',
		};
		const out = [];
		for (const [name, sel] of Object.entries(groups)) {
			const boxes = [...document.querySelectorAll(sel)]
				.filter((el) => el.textContent.trim() && getComputedStyle(el).visibility !== 'hidden')
				// the text's own extent, not its box (a grid cell's box spans its column)
				.map((el) => { const range = document.createRange(); range.selectNodeContents(el); return { text: el.textContent.trim(), r: range.getBoundingClientRect() }; })
				.filter((b) => b.r.width > 0);
			for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) {
				const a = boxes[i].r, b = boxes[j].r;
				const x = Math.min(a.right, b.right) - Math.max(a.left, b.left);
				const y = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
				if (x > 0.5 && y > 0.5) out.push(`${name}: "${boxes[i].text}" overlaps "${boxes[j].text}"`);
			}
		}
		return out;
	});
}

// the frame's controller keeps the last view it built (?e2e)
const viewState = (t) => t.page.evaluate(() => window.__aacLastAnalytics?.state ?? null);

// The first session's Analytics module load is held for 4 s, as a slow phone might hold it, while the page signs out and
// in again: when that late load lands, it must not touch the new session's controller (review L4).
const SLOW_FIRST_LOAD = `(() => {
	let wrapped = null, calls = 0;
	Object.defineProperty(window, 'aacLoadAnalytics', {
		configurable: true,
		get: () => wrapped,
		set: (load) => {
			wrapped = () => {
				const loaded = load();
				return calls++ === 0 ? new Promise((done) => setTimeout(() => done(loaded), 4000)) : loaded;
			};
		},
	});
})();`;

export const tests = {
	async "analytics: a late load from a session that ended never stops the next session's page"(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.context.addInitScript(SLOW_FIRST_LOAD);
		await t.app.signIn();
		await stubData(t);
		await t.goto('/app/analytics?e2e', { settleMs: 0 });
		await t.until('the first session opens Analytics (its load held)', () => !!document.querySelector('.ax-page'), null, { timeout: 30000, hard: true });
		const started = Date.now();
		await t.page.evaluate(() => window.aacFrameSession.signOut());
		await t.until('signed out', () => !!document.querySelector('.signin-page'), null, { timeout: 15000, hard: true });
		await t.app.signIn();
		await t.page.evaluate(() => window.aacFrameSession.recheck());
		await t.until('the next session draws Analytics', () => document.querySelectorAll('.ax-page .kpi').length === 5, null, { timeout: 30000, hard: true });
		// the held load lands now
		await t.page.waitForTimeout(Math.max(0, 4800 - (Date.now() - started)));
		const state = await t.page.evaluate(() => window.aacFrameAnalytics?.state?.() ?? null);
		await t.check('the next session\'s controller still runs, shown', !!state && state.stopped === false && state.shown === true, JSON.stringify(state));
		await t.page.click('.ax-tools-row .seg button:has-text("30D")');
		await t.until('and the page still answers its controls', () => window.__aacLastAnalytics?.state?.range === '30d', null, { timeout: 10000 });
	},

	async 'analytics: every block is drawn from the view, and the controls come back drawn'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		await stubData(t);
		await openAnalytics(t);
		await t.check('no Slint dashboard is loaded for the tab', await t.page.evaluate(() => !document.querySelector('iframe.slint-frame')));
		const counts = await t.page.evaluate(() => ({
			cbm: document.querySelectorAll('.cbm-row').length,
			arcs: document.querySelectorAll('.donut-svg .arcs path').length,
			heat: document.querySelectorAll('.heat-grid .hc').length,
			bars: document.querySelectorAll('.daily-bars .db').length,
			stats: document.querySelectorAll('.stats3 .stat').length,
			agenda: document.querySelectorAll('.ag-line').length,
			quota: document.querySelectorAll('button.qh-row').length,
		}));
		await t.check(`cost by model lists the models (${counts.cbm})`, counts.cbm >= 8);
		await t.check(`the donut has its arcs (${counts.arcs})`, counts.arcs >= 4);
		await t.equal('the heatmap has 7 x 24 cells', counts.heat, 168);
		await t.check(`daily cost has its bars (${counts.bars})`, counts.bars >= 7);
		await t.equal('session stats has three numbers', counts.stats, 3);
		await t.check(`the agenda lists resets (${counts.agenda})`, counts.agenda >= 4);
		await t.check(`quota history lists the accounts (${counts.quota})`, counts.quota >= 4);
		await t.equal('no chart label overlaps another', await overlaps(t), []);
		await t.audit('analytics 1440');

		await t.step('range 30D: the page reads the range and the trend is laid out again', async () => {
			await t.page.click('.ax-tools-row .seg button:has-text("30D")');
			await t.until('30D drawn', () => document.querySelector('.ax-tools-row .seg button.on')?.textContent === '30D', null, { timeout: 15000 });
			await t.equal('the controller holds the range', (await viewState(t))?.range, '30d');
		});
		await t.step('token types: the trend stacks by type', async () => {
			await t.page.click('.trend-card .switch:has-text("Token types")');
			await t.until('stacked', () => document.querySelector('.trend-data')?.classList.contains('split'), null, { timeout: 10000 });
		});
		await t.step('model usage by cost and cost by model by tokens', async () => {
			await t.page.click('.donut-card .seg button:has-text("Cost")');
			await t.until('donut by cost', () => document.querySelector('.donut-card .seg button.on')?.textContent === 'Cost', null, { timeout: 10000 });
			await t.page.click('.cbm-card .seg button:has-text("Tokens")');
			await t.until('sorted by tokens', () => document.querySelector('.cbm-card .seg button.on')?.textContent === 'Tokens', null, { timeout: 10000 });
		});
		await t.step('a model row opens its detail; Escape closes it', async () => {
			await t.page.click('.cbm-row >> nth=0');
			await t.visible('.pop .mdetail', 'model detail open');
			await t.page.keyboard.press('Escape');
			await t.hidden('.pop .mdetail', 'model detail closed');
		});
		await t.step('the provider picker narrows every block to one provider', async () => {
			await t.page.click('.prov-btn');
			await t.visible('.pick-list', 'picker open');
			await t.page.click('.pick-row:has-text("Codex")');
			await t.until('Codex picked', () => document.querySelector('.prov-btn')?.textContent.includes('Codex'), null, { timeout: 15000 });
			await t.equal('the controller holds the provider', (await viewState(t))?.prov, 'codex');
			await t.page.click('.prov-btn');
			await t.page.click('.pick-row:has-text("All providers")');
			await t.until('all again', () => document.querySelector('.prov-btn')?.textContent.includes('All providers'), null, { timeout: 15000 });
		});
		await t.step('a custom range from the calendar', async () => {
			await t.page.click('.date-btn');
			await t.visible('.cal', 'calendar open');
			await t.page.click('.cal-quick button:has-text("Last 3 days")');
			await t.page.click('.cal-acts .btn.primary');
			await t.until('custom range drawn', () => document.querySelector('.date-btn .on') !== null, null, { timeout: 15000 });
			await t.equal('the controller holds a custom range', (await viewState(t))?.range, 'custom');
		});
		await t.step('a quota row opens its focus chart, laid out in its own box', async () => {
			await t.page.click('button.qh-row >> nth=0');
			await t.until('focus chart drawn', () => !!document.querySelector('.focus-box svg path'), null, { timeout: 15000 });
			await t.equal('no focus label overlaps another', await overlaps(t), []);
		});
		await t.step('hovering the trend shows a reading', async () => {
			await t.page.locator('.ax-tools-row .seg button:has-text("7D")').click();
			const box = await t.page.locator('.trend-hit').boundingBox();
			await t.page.mouse.move(box.x + box.width * 0.6, box.y + box.height / 2);
			// a fine pointer reads it in the floating card; a touch screen in the readout row
			await t.until('a reading is shown', () => {
				const card = document.querySelector('.trend-hit .ro-card');
				const bar = document.querySelector('.trend-card .ro-bar .ro-time');
				return (card && card.getBoundingClientRect().width > 0 && /\d/.test(card.textContent)) || (bar && /\d/.test(bar.textContent));
			}, null, { timeout: 5000 });
			await t.page.mouse.move(box.x + box.width * 0.6, box.y - 40);
		});
	},

	async 'analytics: each format lays the page out with nothing sideways and no overlapping labels'(t) {
		for (const f of FORMATS) {
			await t.step(f.name, async () => {
				await t.newContext({ viewport: { width: f.width, height: f.height }, isMobile: f.mobile, hasTouch: f.mobile, serviceWorkers: 'block' });
				await t.app.signIn();
				await stubData(t);
				await openAnalytics(t, f.safe);
				await t.until('charts laid out in their boxes', () => {
					const svg = document.querySelector('.trend-card svg.axes');
					const box = document.querySelector('.trend-box');
					return !!svg && !!box && Math.abs(svg.getBoundingClientRect().width - box.getBoundingClientRect().width) < 2;
				}, null, { timeout: 15000 });
				const wide = await t.page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
				await t.equal(`${f.name}: no sideways scroll`, wide, 0);
				await t.equal(`${f.name}: no chart label overlaps another`, await overlaps(t), []);
				await t.audit(`analytics ${f.name}`);
			});
		}
	},
};
