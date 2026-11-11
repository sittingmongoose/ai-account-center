// A pinned thread's sheet held open while its report changes underneath it. The sheet follows the current view: a report
// that turns unreadable, goes quiet or moves on shows up in the open sheet, and a pin that leaves closes it.
// The first test is the replay from the blind review of 6503fa70. fixtures/pinned-composer-cases.json is the real output
// of native/aac-status's composer; every case is the same thread id, as one pin's report changing over time.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', name), 'utf8'));
const PHONE = { viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1 };
const ROW = '.c2-row';
const SHEET = '.c2-sheet-host .c2-sheet';

const cases = () => fixture('pinned-composer-cases.json');
const withPins = (pinned) => ({ ...fixture('status-design-view.json'), pinned });

/** Opens the first pin's sheet on `first`. `change(view, ready)` serves `view` and re-reads until `ready` holds in the page. */
async function openSheet(t, first) {
	await t.newContext(PHONE);
	await t.app.signIn();
	let current = first;
	await t.page.route((u) => new URL(u).pathname === '/api/status', (r) => r.fulfill({ json: current }));
	// Things to open in the C2 fixture: the read-only route, stubbed (never the share).
	await t.page.context().route((u) => new URL(u).pathname === '/api/status/deliverable', (r) =>
		r.fulfill({ contentType: 'image/jpeg', body: fs.readFileSync(path.join(LEPTOS, 'fixtures', 'media', 'option-a.jpg')) }));
	await t.goto('/app/status', { settleMs: 300 });
	await t.page.locator(ROW).first().click();
	await t.page.waitForSelector(SHEET);
	return {
		async change(view, ready, arg) {
			current = view;
			await t.page.evaluate(() => window.dispatchEvent(new Event('online')));
			await t.page.waitForFunction(ready, arg, { timeout: 10000 });
		},
	};
}

const rowSays = ([selector, source]) => new RegExp(source).test(document.querySelector(selector)?.textContent || '');
const opacity = (t, selector) =>
	t.page.evaluate((s) => {
		let value = 1;
		for (let e = document.querySelector(s); e; e = e.parentElement) value *= parseFloat(getComputedStyle(e).opacity);
		return value;
	}, selector);

export const tests = {
	async 'status open pin sheet follows an unreadable report'(t) {
		const c = cases();
		const sheet = await openSheet(t, withPins(c.loopingBehindBlocked));
		await sheet.change(withPins(c.unreadableWithTasks), rowSays, [ROW, 'cached copy']);
		await t.check('background row reports unreadable', /Could not be read/.test(await t.page.locator(ROW).textContent()));
		await t.text(SHEET, /Could not be read since 8:33 PM/);
		await t.text(SHEET, /cached/i);
		await t.text(SHEET, /1 of 2 tasks done/);
		await t.check('the cached figures are dimmed', (await opacity(t, '.c2-sheet-host .c2-cols')) < 0.6);
		const text = await t.page.locator(SHEET).textContent();
		await t.check('no line says it was just updated', !/Updated 3 min ago/.test(text), text);
		await t.check('the old task list is gone', !/F-00/.test(text), text);
	},

	async 'status open pin sheet follows a report that goes stale or stops'(t) {
		const c = cases();
		const sheet = await openSheet(t, withPins(c.loopingBehindBlocked));
		await sheet.change(withPins(c.stale), rowSays, [ROW, 'No status update']);
		await t.text(SHEET, /No status update for 25 days\./);
		await t.text(SHEET, /The last summary\./);
		await t.count('.c2-sheet-host .fx', 0);
		await t.check('stale: the old looping sentence is gone', !/F-07 has gone back/.test(await t.page.locator(SHEET).textContent()));
		// a pin with no report has no row of its own: it is named in the closing line (D4), and its open sheet says why
		await sheet.change(withPins(c.none), rowSays, ['.c2-silent', 'no report yet']);
		await t.text(SHEET, /This thread doesn't report its status yet\./);
		await t.check('none: the last summary is gone', !/The last summary/.test(await t.page.locator(SHEET).textContent()));
	},

	async 'status open pin sheet closes when the pin goes away'(t) {
		const c = cases();
		const full = withPins(c.loopingBehindBlocked);
		const other = { ...c.loopingBehindBlocked.updates[0], id: 'other-pin', title: 'Another pinned thread' };
		const sheet = await openSheet(t, full);
		await sheet.change(withPins({ ...c.loopingBehindBlocked, updates: [other] }), rowSays, [ROW, 'Another pinned thread']);
		await t.count('.sheet', 0);
		await sheet.change(full, rowSays, [ROW, 'Thread t1']);
		await t.count('.sheet', 0);
		// A panel that becomes unavailable takes its sheet with it.
		await t.page.locator(ROW).first().click();
		await t.page.waitForSelector(SHEET);
		await sheet.change(withPins({ unavailable: "Pinned threads unavailable: T3's state file changed shape." }), rowSays, ['.sec-pinned', 'changed shape']);
		await t.count('.sheet', 0);
	},

	async 'status open pin sheet shows newer figures and keeps its place'(t) {
		// The C2 fixture's first row (Needs you) has a long sheet: facts, things to open, stages and tasks.
		const c2 = fixture('status-c2-pins.json');
		const view = { ...fixture('status-design-view.json'), pinned: c2.pinned, projects: c2.projects };
		const newer = structuredClone(view);
		newer.pinned.updates.find((u) => u.id === 'c2-ready').reported.now = 'The second round-2 review started.';
		const sheet = await openSheet(t, view);
		const scroller = t.page.locator('.sheet');
		await scroller.evaluate((el) => { el.scrollTop = 200; });
		const before = await scroller.evaluate((el) => el.scrollTop);
		await t.check('the sheet is long enough to scroll', before > 0, `scrollTop ${before}`);
		await sheet.change(newer, rowSays, [SHEET, 'The second round-2 review started']);
		await t.text(SHEET, /The second round-2 review started\./);
		await t.equal('the sheet stays where it was scrolled', await scroller.evaluate((el) => el.scrollTop), before);
		await t.page.locator('.sheet-close').click();
		await t.count('.sheet', 0);
	},

	// D12 / review c4b4bc96 L5: a downward drag of the grabber closes the pin sheet.
	async 'status pin sheet closes on a drag of its grabber'(t) {
		await openSheet(t, withPins(fixture('status-design-view.json').pinned));
		const grab = await t.page.locator('.sheet .grabber-zone').boundingBox();
		const x = grab.x + grab.width / 2;
		const y = grab.y + grab.height / 2;
		await t.page.mouse.move(x, y);
		await t.page.mouse.down();
		for (let i = 1; i <= 10; i++) { await t.page.mouse.move(x, y + 16 * i); await t.page.waitForTimeout(30); }
		await t.page.mouse.up();
		await t.count('.sheet', 0);
	},
};
