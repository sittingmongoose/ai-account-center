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

async function stubBoard(t, change = (view) => view) {
	const view = change({ ...fixture('scoreboard-view.json'), byModel: fixture('scoreboard-by-model.json') });
	await t.page.route((u) => new URL(u).pathname === '/api/scoreboard', (r) => r.fulfill({ json: view }));
}

async function openBoard(t, query = '', change) {
	await t.app.signIn();
	await stubBoard(t, change);
	await t.goto(`/app/scoreboard${query}`, { settleMs: 300 });
	await t.page.waitForSelector('.picks');
}

// The fixture's runs not scored (leptos/fixtures/scoreboard-view.json `unscored`): 4 + 12 + 3 + 23 + 6.
const UNSCORED_LINE = '48 runs not scored: 4 unreadable, 12 unknown model, 3 unknown task type, 23 no task type, 6 no model';
const lineText = (t) => t.page.evaluate(() => document.querySelector('.sb-unscored summary')?.textContent.replace(/\s+/g, ' ').trim() ?? null);
/** The list under the line: whether it shows, floats (a hover) or sits in the flow (open), and its box. */
const listState = (t) => t.page.evaluate(() => {
	const list = document.querySelector('.sb-unscored .sbu-list');
	const cs = getComputedStyle(list);
	const r = list.getBoundingClientRect();
	return { shown: cs.display !== 'none' && list.innerText.trim() !== '', floating: cs.position === 'absolute', open: document.querySelector('.sb-unscored > details').open, left: r.left, right: r.right, top: r.top, bottom: r.bottom };
});
const stackTop = (t) => t.page.evaluate(() => document.querySelector('.sb-stack').getBoundingClientRect().top);

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

	// Owner 2026-11-13: runs the server read but could not score are named under the head, never hidden.
	async 'scoreboard runs not scored'(t) {
		await t.viewport(1440, 900);
		await openBoard(t);
		await t.equal('the note names every reason with its runs', await lineText(t), UNSCORED_LINE);
		await t.check('the note sits under the head', await t.page.evaluate(() => {
			const note = document.querySelector('.sb-unscored');
			const sub = document.querySelector('.sb-head .page-sub').getBoundingClientRect();
			return document.querySelector('.sb-head').contains(note) && note.getBoundingClientRect().top >= sub.bottom - 1;
		}));
		await t.check('the note is one line on a desktop', await t.page.evaluate(() => {
			const note = document.querySelector('.sb-unscored');
			return note.getBoundingClientRect().height < parseFloat(getComputedStyle(note).lineHeight) * 1.5;
		}));
		await t.check('the line is a control at least 24 px tall', await t.page.evaluate(() => document.querySelector('.sb-unscored summary').getBoundingClientRect().height >= 24));
		await t.check('the note carries no state colour', await t.page.evaluate(() => {
			const probe = document.createElement('span');
			document.body.append(probe);
			const tone = (v) => { probe.style.color = `var(${v})`; return getComputedStyle(probe).color; };
			const [ink3, ink2] = [tone('--ink-3'), tone('--ink-2')];
			probe.remove();
			return getComputedStyle(document.querySelector('.sb-unscored summary')).color === ink3 && getComputedStyle(document.querySelector('.sb-unscored summary b')).color === ink2;
		}));
		await t.equal('the list is closed at first', (await listState(t)).shown, false);
		await t.audit('scoreboard runs not scored');
		const before = await stackTop(t);
		// The hover preview is for devices that hover (a phone's tap would leave a sticky :hover behind). Headless
		// Chrome answers (hover: none), so the check takes the shipped rule out of its media block, as it is, to
		// exercise it; it first asserts the rule is there and gated.
		const gate = await t.page.evaluate(() => {
			for (const sheet of document.styleSheets) {
				let rules;
				try { rules = sheet.cssRules; } catch { continue; }
				for (const rule of rules) {
					if (!(rule instanceof CSSMediaRule) || rule.conditionText.replace(/\s/g, '') !== '(hover:hover)') continue;
					for (const inner of rule.cssRules) {
						if (inner.selectorText !== '.sb-unscored:hover > details:not([open]) + .sbu-list') continue;
						const headless = !matchMedia('(hover: hover)').matches;
						if (headless) sheet.insertRule(inner.cssText, sheet.cssRules.length);
						return { headless };
					}
				}
			}
			return null;
		});
		await t.check('the hover preview is gated to devices that hover', gate !== null);
		// A mouse hover shows the list floating under the line, over the page; it goes when the pointer leaves.
		const box = await t.page.locator('.sb-unscored summary').boundingBox();
		await t.page.mouse.move(box.x + 20, box.y + box.height / 2);
		await t.until('a hover shows the list', () => document.querySelector('.sbu-list').innerText.includes('48 runs not scored'), null, { timeout: 3000 });
		let list = await listState(t);
		await t.check('the hovered list floats and moves nothing', list.floating && (await stackTop(t)) === before);
		await t.check('the hovered list hangs under the line, inside the window', list.top >= box.y + box.height - 8 && list.top - (box.y + box.height) < 16 && list.right <= 1440);
		await t.text('.sbu-list .sbu-head', '48 runs not scored');
		const lines = (sel) => t.page.$$eval(sel, (l) => l.map((e) => e.textContent.trim()));
		const why = await lines('.sbu-list .sbu-why');
		await t.equal('five reasons, each with its runs', why.map((w) => w.split(' · ')[0]), ['Unreadable: 4 runs', 'Unknown model: 12 runs', 'Unknown task type: 3 runs', 'No task type: 23 runs', 'No model: 6 runs']);
		await t.equal('the unknown ids with their runs', await lines('.sbu-list .sbu-id'), [
			'example-vendor/example-model-9-preview7 runs',
			'builtin:example-coding-plan\\Example-4.23 runs',
		]);
		await t.text('.sbu-list .sbu-more', '2 more runs under other ids.');
		// The pointer may move onto the list without it going.
		const hovered = await t.page.locator('.sbu-list').boundingBox();
		await t.page.mouse.move(hovered.x + 10, hovered.y + hovered.height - 10, { steps: 4 });
		await t.equal('the list stays under the pointer', (await listState(t)).shown, true);
		await t.shot('scoreboard-unscored-desktop', 'runs not scored at 1440 px: the line under the head and its list on a hover');
		await t.page.mouse.move(box.x + box.width / 2, 880);
		await t.until('the list goes when the pointer leaves', () => getComputedStyle(document.querySelector('.sbu-list')).display === 'none', null, { timeout: 3000 });
		// A click keeps it open, in the flow under the line, after the pointer leaves; another click closes it.
		await t.page.click('.sb-unscored summary');
		await t.page.mouse.move(box.x + box.width / 2, 880);
		list = await listState(t);
		await t.check('a click keeps the list open in the flow', list.open && list.shown && !list.floating && (await stackTop(t)) > before);
		await t.page.click('.sb-unscored summary');
		await t.equal('another click closes it', (await listState(t)).open, false);
		await t.page.mouse.move(box.x + box.width / 2, 880);
		await t.until('closed, it goes when the pointer leaves', () => getComputedStyle(document.querySelector('.sbu-list')).display === 'none', null, { timeout: 3000 });
		// The keyboard opens it too.
		await t.page.focus('.sb-unscored summary');
		await t.page.keyboard.press('Enter');
		await t.equal('Enter opens it', (await listState(t)).open, true);
		await t.page.keyboard.press('Enter');

		// None not scored, or an older server without the field: no line at all.
		for (const [name, change] of [
			['none not scored', (v) => ({ ...v, unscored: { total: 0, noModel: 0, noTaskType: 0, unknownModel: 0, unknownModels: [], examples: [] } })],
			['no unscored field', (v) => { const { unscored, ...rest } = v; return rest; }],
		]) {
			await t.newContext({ viewport: { width: 1440, height: 900 } });
			await openBoard(t, '', change);
			await t.check(`${name}: no line under the head`, (await lineText(t)) === null);
			await t.count('.sb-unscored', 0);
		}
	},

	// Review 1, M3: the chevron never stands alone on a line. The reviewer's sweep: the note's box from 120 to 520 px,
	// at the page's own size and at 13 px, with the fixture's words and with the live share's words of that day.
	async 'scoreboard runs not scored: the chevron never stands alone'(t) {
		const texts = [
			['fixture', (v) => v],
			['share 2026-11-13', (v) => ({ ...v, unscored: { total: 46, unreadable: 0, noModel: 11, noTaskType: 35, unknownModel: 0, unknownTaskType: 0, unknownModels: [], examples: [] } })],
			['share with refusals', (v) => ({ ...v, unscored: { total: 752, unreadable: 706, noModel: 11, noTaskType: 35, unknownModel: 0, unknownTaskType: 0, unknownModels: [], examples: [] } })],
			['share with scan limit', (v) => ({ ...v, unscored: { total: 1250, scanLimit: 498, unreadable: 706, noModel: 11, noTaskType: 35, unknownModel: 0, unknownTaskType: 0, unknownModels: [], examples: [] } })],
		];
		for (const [name, change] of texts) {
			await t.newContext({ viewport: { width: 1440, height: 900 } });
			await openBoard(t, '', change);
			const sweep = await t.page.evaluate(() => {
				const note = document.querySelector('.sb-unscored');
				const chev = note.querySelector('.chev');
				const tail = note.querySelector('summary .nw');
				const bad = [];
				for (const size of ['', '13px']) {
					note.style.fontSize = size;
					for (let w = 120; w <= 520; w++) {
						note.style.width = `${w}px`;
						const words = tail.getClientRects();
						const c = chev.getBoundingClientRect();
						const w0 = words[words.length - 1];
						const alone = words.length !== 1 || Math.abs(c.top + c.height / 2 - (w0.top + w0.height / 2)) > w0.height / 2;
						if (alone) bad.push(`${size || 'own'}@${w}`);
					}
				}
				note.style.fontSize = '';
				note.style.width = '';
				return { bad: bad.slice(0, 12), count: bad.length };
			});
			await t.check(`${name}: the chevron stays with the last words at every width from 120 to 520 px`, sweep.count === 0, `alone at ${sweep.bad.join(', ')}${sweep.count > 12 ? ' ...' : ''}`);
		}
	},

	async 'scoreboard runs not scored in each format'(t) {
		for (const f of FORMATS) {
			await t.newContext({ viewport: { width: f.w, height: f.h }, isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1 });
			await openBoard(t, `?safe=${f.safe}`);
			await t.equal(`${f.name}: the line`, await lineText(t), UNSCORED_LINE);
			const shape = await t.page.evaluate(() => {
				const summary = document.querySelector('.sb-unscored summary');
				const note = document.querySelector('.sb-unscored');
				const lh = parseFloat(getComputedStyle(note).lineHeight);
				// Each count (b) and its words (the span after it) sit on one line: one box each, at one height.
				const whole = [...summary.querySelectorAll('b')].every((b) => {
					const words = b.nextElementSibling.getClientRects();
					return b.getClientRects().length === 1 && words.length === 1 && Math.abs(words[0].top - b.getClientRects()[0].top) < 2;
				});
				const head = document.querySelector('.sb-head').getBoundingClientRect();
				const n = note.getBoundingClientRect();
				return { lines: Math.round(n.height / lh), whole, inside: n.left >= head.left - 1 && n.right <= head.right + 1, h: n.height };
			});
			await t.check(`${f.name}: each count stays with its words`, shape.whole);
			await t.check(`${f.name}: two lines at most`, shape.lines <= 2, `${shape.lines} lines (${shape.h} px)`);
			await t.check(`${f.name}: inside the head`, shape.inside);
			await t.audit(`${f.name} runs not scored`);
			// A tap opens the list in the flow under the line, inside the content; another tap closes it.
			await t.page.click('.sb-unscored summary');
			const list = await listState(t);
			const content = await t.page.evaluate(() => {
				const r = document.querySelector('.sb-head').getBoundingClientRect();
				return { left: r.left, right: r.right };
			});
			await t.check(`${f.name}: a tap opens the list in the flow`, list.open && list.shown && !list.floating);
			const lineBottom = await t.page.evaluate(() => document.querySelector('.sb-unscored > details').getBoundingClientRect().bottom);
			await t.check(`${f.name}: the open list sits under the line, not beside it`, list.top >= lineBottom - 1, `list top ${Math.round(list.top)}, line bottom ${Math.round(lineBottom)}`);
			await t.check(`${f.name}: the open list stays inside the content`, list.left >= content.left - 1 && list.right <= content.right + 1, `${Math.round(list.left)}-${Math.round(list.right)} in ${Math.round(content.left)}-${Math.round(content.right)}`);
			await t.check(`${f.name}: no sideways scroll with it open`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
			await t.count('.sbu-list .sbu-id', 2);
			await t.text('.sbu-list .sbu-why', /^Unreadable: 4 runs · /);
			if (f.name === 'iphone' || f.name === 'duo-landscape') await t.shot(`scoreboard-unscored-${f.name}`, `runs not scored on ${f.name} (${f.w}x${f.h}): the list open under the line`);
			await t.page.click('.sb-unscored summary');
			await t.equal(`${f.name}: another tap closes it`, (await listState(t)).shown, false);
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
