// The Model Scoreboard page (Leptos, /app/scoreboard): verdicts by model and effort. The real server reads the kit's
// fixture runs and catalog (scripts/scoreboard-fixture.mjs) in its temporary home; the `byModel` view comes from the
// golden fixture until the server sends it. Synthetic data only.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', name), 'utf8'));
const PHONE = { viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 };
const FORMATS = [
	{ name: 'desktop', w: 1440, h: 900, safe: '0,0,0,0', mobile: false },
	{ name: 'iphone', w: 402, h: 874, safe: '62,0,34,0', mobile: true },
	{ name: 'duo-folded', w: 466, h: 678, safe: '0,84,34,0', mobile: true },
	{ name: 'duo-portrait', w: 669, h: 951, safe: '82,0,34,0', mobile: true },
	{ name: 'duo-landscape', w: 951, h: 669, safe: '0,84,34,0', mobile: true },
];

async function stubBoard(t) {
	const view = { ...fixture('scoreboard-view.json'), byModel: fixture('scoreboard-by-model.json') };
	await t.page.route((u) => new URL(u).pathname === '/api/scoreboard', (r) => r.fulfill({ json: view }));
}

async function openBoard(t, query = '') {
	await t.app.signIn();
	await stubBoard(t);
	await t.goto(`/app/scoreboard${query}`, { settleMs: 300 });
	await t.page.waitForSelector('.picks');
}

export const tests = {
	// Against the real server's view, which has no byModel yet: an honest line, nothing guessed.
	async 'scoreboard without the by-model view'(t) {
		await t.viewport(1440, 900);
		await t.app.signIn();
		await t.goto('/app/scoreboard', { settleMs: 300 });
		await t.page.waitForSelector('.page');
		await t.text('.sb-unavail', /isn't available from this server yet/);
		await t.count('.picks, .sb-matrix', 0);
		await t.audit('scoreboard unavailable');
	},

	async 'scoreboard desktop'(t) {
		await t.viewport(1440, 900);
		await openBoard(t);
		await t.count('.pick', 5);
		await t.count('.sb-matrix tbody tr', 8);
		await t.text('.page-title', 'Model Scoreboard');
		await t.check('the switch shares the title line', async () =>
			t.page.evaluate(() => {
				const a = document.querySelector('.page-title').getBoundingClientRect();
				const b = document.querySelector('.sb-title .seg').getBoundingClientRect();
				return a.bottom > b.top && b.bottom > a.top;
			})
		);
		await t.check('no "Verdicts for" label', async () => t.page.evaluate(() => !/Verdicts for/.test(document.querySelector('.sb-head').textContent)));
		await t.text('.sb-matrix tbody tr:first-child', /Recommended/);
		await t.text('.detail-grid', /likely between 69% and 100%/);
		await t.audit('scoreboard 1440');
		await t.page.click('.seg button >> text=Puppet Master users');
		await t.page.waitForFunction(() => /Recommended/.test(document.querySelector('.sb-matrix tbody')?.textContent || ''));
		await t.page.click('.sb-matrix tbody tr:nth-child(2)');
		await t.count('.sb-matrix tbody tr.sel', 1);
		await t.shot('scoreboard-desktop', 'Scoreboard at 1440 px: picks, matrix by model and effort, the selected model report');
	},

	async 'scoreboard layout in each format'(t) {
		for (const f of FORMATS) {
			await t.newContext({ viewport: { width: f.w, height: f.h }, isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1 });
			await openBoard(t, `?safe=${f.safe}`);
			await t.audit(`${f.name} ${f.w}x${f.h}`);
			await t.check(`${f.name}: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
			await t.check(`${f.name}: the switch is on the title line`, await t.page.evaluate(() => {
				const title = document.querySelector('.page-title');
				const seg = document.querySelector('.sb-title .seg').getBoundingClientRect();
				const head = document.querySelector('.sb-head').getBoundingClientRect();
				if (getComputedStyle(title).display === 'none') return seg.top - head.top < 12;
				const a = title.getBoundingClientRect();
				return a.bottom > seg.top && seg.bottom > a.top;
			}));
			await t.check(`${f.name}: verdict notes break only between their parts`, await t.page.evaluate(() =>
				[...document.querySelectorAll('.vsub .nw')].every((n) => n.getClientRects().length === 1)
			));
		}
	},

	async 'scoreboard phone sheet'(t) {
		await t.newContext(PHONE);
		await openBoard(t);
		await t.audit('scoreboard phone');
		await t.page.click('.sbm >> nth=0');
		await t.page.waitForSelector('.sheet .job');
		await t.text('.sheet .sheet-title', /Opus 5\.5/);
		await t.shot('scoreboard-phone-sheet', 'a model report in a sheet on a 402 px phone');
		await t.page.click('.sheet-close');
		await t.count('.sheet', 0);
	},
};
