// Provider marks draw once (owner report 2026-11-28: "Many of the provider icons have a faint double image below them
// all throughout aac"). Marks with their own light artwork (Codex, Cursor, Qwen, Z.ai, OpenCode Go; icons.rs
// `has_light_art`) are two <img class="mk lt|dk"> layers and the theme hides one. `.pmark .mk { display: block }` used to
// win over `.mk.dk { display: none }`, so in the light theme the dark drawing showed as a faded copy under the mark.
// For Home, Accounts, Analytics and the Scoreboard, light and dark, at 402 and 1024 CSS pixels, every mark holder shows
// exactly one visible layer, and that layer sits inside its box (the box is its declared size, nothing hangs below).
// Real test server and sign-in (the kit's temporary home); every reading is made-up data answered in the page.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';
import { fakeServer, serve } from '../accounts-fixture.mjs';
import { analyticsFixture } from '../device/analytics-fixture.mjs';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', name), 'utf8'));

const WIDTHS = [
	{ name: '402', viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true },
	{ name: '1024', viewport: { width: 1024, height: 800 } },
];

const at = (minutes) => new Date(Date.now() + minutes * 60_000).toISOString();
const quota = (key, label, used, minutes, reset) => ({ key, label, usedPercent: used, remainingPercent: 100 - used, resetAt: at(reset), windowMinutes: minutes });
const account = (id, provider, email, plan, windows) => ({
	id, provider, providerLabel: provider, label: email, email, plan, platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok', message: null,
	fetchedAt: at(-2), sampledAt: at(-2), isActive: false, hidden: false, trayHidden: false, windows, capabilities: {}, signInNeeded: [],
});
const AX_ACCOUNTS = [
	account('claude-1', 'claude', 'claude.one@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 9, 300, 160)]),
	account('codex-1', 'codex', 'codex.one@example.invalid', 'pro', [quota('five_hour', '5h', 55, 300, 130)]),
	account('zai-1', 'zai', 'zai.one@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 27, 300, 120)]),
	account('cursor-1', 'cursor', 'cursor.one@example.invalid', 'pro', [quota('monthly', 'Included', 4, 43200, 6000)]),
];
const AX_DASHBOARD = { schemaVersion: 1, updatedAt: at(-1), accounts: AX_ACCOUNTS, providers: [], settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [] } };

const PAGES = {
	async home(t) {
		await t.app.stub('/api/accounts/dashboard', homeAccounts());
		await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
		await t.goto('/app/home', { settleMs: 300 });
		await t.until('Home is drawn', () => document.querySelectorAll('.home .hrow').length >= 8 && document.querySelectorAll('.hcard').length >= 6, null, { timeout: 60000, hard: true });
	},
	async accounts(t) {
		await serve(t, fakeServer());
		await t.goto('/app/accounts', { settleMs: 300 });
		await t.until('Accounts is drawn', () => document.querySelectorAll('.prov[data-provider]').length >= 9, null, { timeout: 60000, hard: true });
	},
	async analytics(t) {
		await t.app.stub('/api/accounts/dashboard', AX_DASHBOARD);
		await t.page.route((u) => new URL(u).pathname === '/api/accounts/analytics', (r) => {
			const range = new URL(r.request().url()).searchParams.get('range') || '7d';
			return r.fulfill({ json: analyticsFixture({ range, days: null, accounts: AX_ACCOUNTS, keptDays: 120, gapAll: false }) });
		});
		await t.goto('/app/analytics', { settleMs: 300 });
		await t.until('Analytics is drawn', () => document.querySelectorAll('.ax-page .kpi').length === 5 && document.querySelectorAll('.ax-page .qh-group').length > 0, null, { timeout: 60000, hard: true });
	},
	async scoreboard(t) {
		const view = { ...fixture('scoreboard-view.json'), byModel: fixture('scoreboard-by-model.json') };
		await t.page.route((u) => new URL(u).pathname === '/api/scoreboard', (r) => r.fulfill({ json: view }));
		await t.goto('/app/scoreboard', { settleMs: 300 });
		await t.until('the Scoreboard is drawn', () => !!document.querySelector('.picks .mk'), null, { timeout: 60000, hard: true });
	},
};

/** Every element holding mark layers, with how many layers show and whether the shown one stays inside a `.pmark` box. */
const markHolders = () => {
	const shown = (el) => {
		const cs = getComputedStyle(el);
		const r = el.getBoundingClientRect();
		return cs.display !== 'none' && cs.visibility !== 'hidden' && Number(cs.opacity) > 0 && r.width > 0 && r.height > 0;
	};
	// Holders inside a layout the page hides at this width (the Scoreboard's matrix or its phone list) are skipped
	const holders = new Set([...document.querySelectorAll('.route:not([hidden]) img.mk')].map((img) => img.parentElement)
		.filter((holder) => holder.checkVisibility({ visibilityProperty: true })));
	return [...holders].map((holder) => {
		const layers = [...holder.children].filter((c) => c.matches('img.mk'));
		const visible = layers.filter(shown);
		let spill = 0;
		if (holder.classList.contains('pmark')) {
			const box = holder.getBoundingClientRect();
			for (const img of visible) {
				const r = img.getBoundingClientRect();
				spill = Math.max(spill, Math.round(r.bottom - box.bottom), Math.round(box.top - r.top));
			}
		}
		return { where: holder.closest('[data-row],[data-provider],.hcard,.pick,tr,section')?.className || holder.className, layers: layers.length, paired: layers.some((l) => l.classList.contains('lt')), visible: visible.length, spill };
	}).filter((h) => h.layers > 0);
};

/** The first two Home cards whose mark has light artwork, as one viewport rect (scrolled into view). */
const cardHeads = () => {
	const heads = [...document.querySelectorAll('.hcard')].filter((c) => c.querySelector('.pmark .mk.lt')).slice(0, 2).map((c) => c.querySelector('.hcard-head'));
	heads[0].scrollIntoView({ block: 'start' });
	window.scrollBy(0, -12);
	const rs = heads.map((h) => h.getBoundingClientRect());
	const x = Math.min(...rs.map((r) => r.left)), y = Math.min(...rs.map((r) => r.top));
	const right = Math.max(...rs.map((r) => r.right)), bottom = Math.min(innerHeight, Math.max(...rs.map((r) => r.bottom)));
	return { x, y, width: right - x, height: bottom - y };
};

function caseFor(page, scheme, width) {
	return async (t) => {
		await t.newContext({ ...width, colorScheme: scheme, deviceScaleFactor: 2, serviceWorkers: 'block' });
		await t.app.signIn();
		await PAGES[page](t);
		await t.page.waitForTimeout(300);
		const holders = await t.page.evaluate(markHolders);
		const label = `${page} ${scheme} ${width.name}`;
		await t.check(`${label}: marks with light and dark artwork are drawn`, holders.some((h) => h.paired), `${holders.length} mark holders`);
		const doubled = holders.filter((h) => h.visible !== 1);
		await t.check(`${label}: every mark shows exactly one layer`, doubled.length === 0, JSON.stringify(doubled.slice(0, 4)));
		const spilled = holders.filter((h) => h.spill > 1);
		await t.check(`${label}: no mark hangs outside its box`, spilled.length === 0, JSON.stringify(spilled.slice(0, 4)));
		if (page === 'home') {
			const rect = await t.page.evaluate(cardHeads);
			await t.crop(`home-cards-${scheme}-${width.name}`, rect, `Home card heads, ${scheme}, ${width.name} px: each provider mark once, crisp, no faded copy under it`, { pad: 4 });
		}
		await t.noConsoleErrors(`${label}: console clean`);
	};
}

export const tests = {};
for (const page of Object.keys(PAGES)) {
	for (const scheme of ['light', 'dark']) {
		for (const width of WIDTHS) tests[`provider marks: ${page} ${scheme} ${width.name} shows each mark once`] = caseFor(page, scheme, width);
	}
}
