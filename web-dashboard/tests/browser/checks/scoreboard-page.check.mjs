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
	// The installed app's real poses (review U7): no top inset on the iPhone or the open Duo, the folded Duo turned.
	{ name: 'iphone', w: 402, h: 812, safe: '0,0,34,0', mobile: true },
	{ name: 'duo-folded', w: 466, h: 678, safe: '0,84,34,0', mobile: true },
	{ name: 'duo-folded-turned', w: 678, h: 466, safe: '0,84,34,0', mobile: true },
	{ name: 'duo-open-upright', w: 669, h: 869, safe: '0,0,34,0', mobile: true },
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
		// The server's own answer without `byModel`: a server whose aac-status native module is built sends it
		// (scoreboard-service.ts), one without the module does not, and this is the page's word for the second.
		await t.page.route((u) => new URL(u).pathname === '/api/scoreboard', async (r) => {
			const response = await r.fetch();
			const json = await response.json();
			delete json.byModel;
			await r.fulfill({ response, json });
		});
		await t.goto('/app/scoreboard', { settleMs: 300 });
		await t.page.waitForSelector('.sb-unavail');
		await t.text('.sb-unavail', /isn't available from this server yet/);
		await t.count('.picks, .sb-matrix', 0);
		await t.audit('scoreboard unavailable');
	},

	async 'scoreboard desktop'(t) {
		await t.viewport(1440, 900);
		await openBoard(t);
		// Select explicitly: the page remembers the last view across visits.
		await t.page.click('.seg button >> text=Our pipeline');
		await t.page.waitForSelector('.seg button[aria-selected="true"]:has-text("Our pipeline")');
		// f071ed486 regenerated the golden from the builder (no Review job); a8fa9814e
		// excludes rows below the review bar. 822a060e9 updated the Rust fixture test;
		// this browser expectation must follow the same three jobs and seven models.
		await t.count('.pick', 3);
		await t.count('.sb-matrix tbody tr', 7);
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
		await t.page.waitForSelector('.seg button[aria-selected="true"]:has-text("Puppet Master users")');
		await t.count('.pick', 3);
		await t.count('.sb-matrix tbody tr', 7);
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
				[...document.querySelectorAll('.vsub .nw')].every((n) => n.getBoundingClientRect().height < 24)
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

	// Review c4b4bc96 L1/L3/L5: the open sheet follows a refresh without being redrawn from scratch, its close button
	// sits above the drag zone, and a drag of the grabber closes it (a short one springs back).
	async 'scoreboard phone sheet follows a refresh and closes on a drag'(t) {
		await t.newContext(PHONE);
		await t.app.signIn();
		let view = { ...fixture('scoreboard-view.json'), byModel: fixture('scoreboard-by-model.json') };
		await t.page.route((u) => new URL(u).pathname === '/api/scoreboard', (r) => r.fulfill({ json: view }));
		await t.goto('/app/scoreboard', { settleMs: 300 });
		await t.page.waitForSelector('.picks');
		await t.page.click('.sbm >> nth=0');
		await t.page.waitForSelector('.sheet .job');
		await t.page.click('.sheet .sb-sheet-runs .disclosure');
		await t.page.waitForSelector('.sheet ul.sb-how li, .sheet p.sb-how');
		await t.page.locator('.sheet').evaluate((el) => { el.dataset.kitMark = 'kept'; });
		const newer = structuredClone(view);
		for (const v of newer.byModel.views) for (const m of v.models) if (m.key === view.byModel.views[0].models[0].key) m.sheetHead = 'Claude Code · 29 runs · 9 blind reviews';
		view = newer;
		await t.page.evaluate(() => window.dispatchEvent(new Event('online')));
		await t.page.waitForFunction(() => /29 runs/.test(document.querySelector('.sheet .sheet-title')?.textContent || ''), null, { timeout: 10000 });
		await t.equal('the sheet was not mounted again', await t.page.locator('.sheet').getAttribute('data-kit-mark'), 'kept');
		await t.equal('its runs stay open', await t.page.locator('.sheet .sb-sheet-runs .disclosure').getAttribute('aria-expanded'), 'true');
		const closeHit = await t.page.locator('.sheet-close').evaluate((b) => {
			const r = b.getBoundingClientRect();
			return b.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2));
		});
		await t.check('the close button is above the drag zone', closeHit);
		const grab = await t.page.locator('.sheet .grabber-zone').boundingBox();
		const x = grab.x + grab.width / 2;
		const y = grab.y + grab.height / 2;
		const drag = async (dy) => {
			await t.page.mouse.move(x, y);
			await t.page.mouse.down();
			for (let i = 1; i <= 10; i++) { await t.page.mouse.move(x, y + (dy * i) / 10); await t.page.waitForTimeout(30); }
			await t.page.mouse.up();
		};
		await drag(16);
		await t.page.waitForTimeout(300);
		await t.count('.sheet', 1);
		await t.equal('a short drag springs back', await t.page.locator('.sheet').evaluate((el) => el.style.transform), '');
		await drag(160);
		await t.count('.sheet', 0);
	},
};
