// The Status page (Leptos, /app/status) served by this checkout's dist/ui: panels, the question deck, the media viewer
// and the review-and-send flow, on desktop and on a phone-sized touch screen.
// Synthetic data only: GET /api/status answers with web-dashboard/leptos/fixtures/*.json (made-up threads), card files
// come from fixtures/media, and POST /api/status/answers is stubbed in the page, so a test tap never reaches an answer
// store, real or temporary. The stub keeps what the page sent so the check can assert on it.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', name), 'utf8'));
const TYPES = { '.jpg': 'image/jpeg', '.png': 'image/png', '.mp4': 'video/mp4', '.webp': 'image/webp' };
// iPhone 17 Pro's CSS viewport. Touch and DPR 2; WebKit itself is only tested on the simulator.
const PHONE = { viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 };
const FORMATS = [
	{ name: 'desktop', w: 1440, h: 900, safe: '0,0,0,0', mobile: false, chrome: 'desk' },
	{ name: 'iphone', w: 402, h: 874, safe: '62,0,34,0', mobile: true, chrome: 'phone' },
	{ name: 'duo-folded', w: 466, h: 678, safe: '0,84,34,0', mobile: true, chrome: 'rail' },
	{ name: 'duo-portrait', w: 669, h: 951, safe: '82,0,34,0', mobile: true, chrome: 'band' },
	{ name: 'duo-landscape', w: 951, h: 669, safe: '0,84,34,0', mobile: true, chrome: 'rail' },
	// The Duo open on its side while iOS reports no strip (a rotated open app): still the rail (owner, 2026-11-10).
	{ name: 'open-window', w: 951, h: 669, safe: '0,0,0,0', mobile: true, chrome: 'rail' },
];

// Every status call the page makes, answered here. Returns the answer sets the page sent.
async function stubStatus(t, view) {
	const sent = [];
	await t.page.route((u) => new URL(u).pathname === '/api/status', (r) => r.fulfill({ json: view }));
	await t.page.route((u) => new URL(u).pathname === '/api/status/answers', async (r) => {
		const body = JSON.parse(r.request().postData() || '{}');
		sent.push(body);
		const results = (body.answers || []).map((a) => ({ cardId: a.cardId, outcome: 'recorded' }));
		await r.fulfill({ json: { results } });
	});
	await stubMedia(t);
	return sent;
}

// Card files from the fixture's media folder (with byte ranges for video).
async function stubMedia(t) {
	// Card files: the fixture's media by file name (option-a.jpg, option-b.mp4, ...), with byte ranges for video. A
	// file the fixture folder doesn't have (the server view's writer images) gets a stand-in of the same kind.
	await t.page.route((u) => new URL(u).pathname.startsWith('/api/status/files/'), async (r) => {
		let name = path.basename(new URL(r.request().url()).pathname);
		if (!fs.existsSync(path.join(LEPTOS, 'fixtures', 'media', name))) name = /\.(mp4|webm)$/i.test(name) ? 'option-b.mp4' : 'option-a.jpg';
		const file = path.join(LEPTOS, 'fixtures', 'media', name);
		const body = fs.readFileSync(file);
		const type = TYPES[path.extname(name)] || 'application/octet-stream';
		const range = /bytes=(\d+)-(\d*)/.exec(r.request().headers().range || '');
		if (!range) return r.fulfill({ status: 200, body, headers: { 'content-type': type, 'accept-ranges': 'bytes' } });
		const start = Number(range[1]);
		const end = range[2] ? Math.min(Number(range[2]), body.length - 1) : body.length - 1;
		await r.fulfill({
			status: 206,
			body: body.subarray(start, end + 1),
			headers: { 'content-type': type, 'accept-ranges': 'bytes', 'content-range': `bytes ${start}-${end}/${body.length}` },
		});
	});
}

async function openStatus(t, view, query = '') {
	await t.app.signIn();
	const sent = await stubStatus(t, view);
	await t.goto(`/app/status${query}`, { settleMs: 300 });
	await t.page.waitForSelector('#cards');
	return sent;
}

const SECTIONS = ['.sec-questions', '.sec-progress', '.sec-pinned', '.sec-machines', '.sec-phones', '.sec-spend'];
// The deck's "Next" (the last card's reads Review).
const NEXT = '.q-send > button:nth-of-type(2)';
const CARD = 'article.q-card';

// Signed answers the server wrote to the kit's temporary reports folder (never the share).
function signedAnswers(t) {
	const dir = path.join(t.state.reports, 'v1', 'answers');
	return fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith('.json')) : [];
}

export const tests = {
	// No stubs: the real server reads the kit's fixture reports folder, serves card media with byte ranges, records the
	// answer in the temporary home's store, signs it into the temporary reports folder, and rings nobody (doorbell off).
	async 'status end to end on the server'(t) {
		await t.viewport(1440, 900);
		await t.app.signIn();
		await t.goto('/app/status', { settleMs: 300 });
		await t.page.waitForSelector(CARD);
		await t.text('.sec-questions .plate-meta', /9 open/);
		await t.check('card video plays from the server (byte ranges)', async () =>
			t.page.evaluate(async () => {
				const v = [...document.querySelectorAll('.media video')][0];
				if (!v) return 'the first card has no video';
				v.muted = true;
				try { await v.play(); v.pause(); return true; } catch (e) { return String(e); }
			})
		);
		// Answer the first question card in the deck, then send from the review.
		let found = false;
		for (let i = 0; i < 9 && !found; i++) {
			found = await t.page.evaluate(() => {
				const card = document.querySelector('article.q-card');
				return !!card && ['question', ''].includes(card.dataset.kind || '') && !!card.querySelector('.opt-pick:not([disabled])');
			});
			if (!found) await t.page.click(NEXT);
		}
		await t.check('a question card to answer', found);
		const before = signedAnswers(t).length;
		await t.page.click(`${CARD} .opt-pick >> nth=0`);
		await t.page.click('.q-send .primary');
		await t.page.waitForSelector('.review');
		await t.text('.review .q-send .primary', /Send 1 answer/);
		await t.page.click('.review .q-send .primary');
		await t.page.waitForSelector('.sent');
		await t.text('.sent .ok', /Sent 1 at/);
		await t.equal('one signed answer written to the test reports folder', signedAnswers(t).length, before + 1);
		await t.until('the answered card leaves the deck', () => /8 open/.test(document.querySelector('.sec-questions .plate-meta')?.textContent || ''), null, { timeout: 15000 });
		await t.audit('end to end 1440');
	},

	async 'status desktop'(t) {
		await t.viewport(1440, 900);
		await openStatus(t, fixture('status-view.json'));
		for (const section of SECTIONS) await t.count(section, 1);
		await t.count('#usage, #threads, .sec-usage, .sec-threads', 0);
		await t.page.waitForSelector(CARD);
		await t.text('.sec-questions .plate-meta', /6 open/);
		await t.audit('status 1440');
		await t.shot('status-desktop', 'Status page at 1440 px: panel grid, question deck, no clipped text');
		for (const width of [1024, 1920]) {
			await t.viewport(width, 900);
			await t.page.waitForTimeout(200);
			await t.audit(`status ${width}`);
			await t.snap(`status-${width}`);
		}
	},

	// The view the server branch builds from scripts/status-fixture.mjs (--with-kinds --with-problems): nulls, damaged
	// report files and unreachable machines must read plainly, not break the page.
	async 'status from a server view'(t) {
		await t.viewport(1440, 900);
		await openStatus(t, fixture('server-view.json'));
		await t.page.waitForSelector(CARD);
		await t.text('.sec-questions .plate-meta', /9 open/);
		await t.text('.sec-machines', /ssh refused/);
		await t.text('.sec-machines', /under the 50 GB floor/);
		await t.text('.sec-questions .plate-note', /unreadable/i);
		await t.check('nothing reads as zero for a machine with no reading', async () =>
			t.page.evaluate(() => {
				const nas = [...document.querySelectorAll('.mach-table .row')].find((m) => /Nas1/.test(m.textContent));
				return !!nas && !/\b0(\.0)? ?GB free/.test(nas.textContent);
			})
		);
		// The threads' card additions: a verdict filter, a judges' split, a sample that sends nothing, verbatim quotes.
		await t.count('.q-seg .seg button', 2); // All and Adapt, the fixture's one verdict
		await t.page.click('.q-seg .seg button >> text=Adapt');
		await t.count('.q-list .q-item.sel[data-label="F-Q4"]', 1);
		await t.visible('text=The judges split', 'split judges are flagged');
		await t.page.click('.q-seg .seg button >> text=All');
		await t.page.click('.q-list .q-item[data-label="C-S1"]');
		await t.visible('.sample-banner', 'a sample card says no answer is wanted');
		await t.check('a sample card cannot be answered', async () => t.page.evaluate(() => [...document.querySelectorAll('article.q-card .opt-pick')].every((b) => b.disabled)));
		await t.page.click('.q-list .q-item[data-label="ST-Q2"]');
		await t.count(`${CARD} .quote`, 1);
		await t.count(`${CARD} .plan-change`, 1);
		await t.audit('server view 1440');
		await t.crop('status-machines', '.sec-machines', 'unreachable machines say why, and show no invented readings');
	},

	// The design's formats, standalone (the page's own chrome), with the insets measured on the simulators
	// (DUO-MEASURED.md) given through ?safe=. WebKit itself is checked on the simulators.
	async 'status layout in each format'(t) {
		const view = { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json') };
		for (const f of FORMATS) {
			await t.newContext({ viewport: { width: f.w, height: f.h }, isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1 });
			await openStatus(t, view, `?safe=${f.safe}`);
			await t.page.waitForSelector(CARD);
			await t.audit(`${f.name} ${f.w}x${f.h}`);
			await t.equal(`${f.name}: chrome`, await t.page.evaluate(() => document.documentElement.dataset.chrome), f.chrome);
			await t.check(`${f.name}: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
			await t.check(`${f.name}: the content is as wide as its window, not its widest child`, await t.page.evaluate(() => document.querySelector('.content').getBoundingClientRect().width <= innerWidth));
			await t.count('#usage, #threads', 0);
			// Line breaks: "Recommended" stays with its label, and each row's percent with its name.
			await t.check(`${f.name}: "Recommended" is never alone on a line`, await t.page.evaluate(() => {
				const rec = document.querySelector('.opt-rec');
				if (!rec) return true;
				const rects = rec.getClientRects();
				const label = rec.closest('.opt-label');
				const line = document.createRange();
				line.selectNodeContents(label.firstChild);
				const words = [...line.getClientRects()];
				return rects.length === 1 && words.some((w) => Math.abs(w.bottom - rects[0].bottom) < 4);
			}));
			if (f.chrome === 'band') {
				await t.count('.band-info a, .band-info button', 0);
				await t.check('band: the tabs sit below the 82 px band', await t.page.evaluate(() => document.querySelector('.band-tabs').getBoundingClientRect().top >= 82));
			}
			if (f.chrome === 'rail') {
				await t.check(`${f.name}: the rail is inside the 84 px strip, below the clock`, await t.page.evaluate(() => {
					const rail = document.querySelector('.rail').getBoundingClientRect();
					const first = document.querySelector('.rail a').getBoundingClientRect();
					return Math.round(rail.width) === 84 && first.top >= parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--rail-reserve'));
				}));
			}
			if (f.name === 'duo-landscape') {
				await t.page.click('.plate-tools.only-land .btn >> nth=1'); // the second question has choices
				await t.page.waitForSelector('.opts .opt');
				await t.check('landscape: the question card lies across the screen', await t.page.evaluate(() => {
					const opts = [...document.querySelectorAll('.opts .opt')].map((o) => o.getBoundingClientRect());
					return opts.length === 3 && Math.abs(opts[0].top - opts[1].top) < 2 && opts[1].left > opts[0].right - 2;
				}));
				await t.count('.plate-tools.only-land .btn', 3);
			}
		}
	},

	// The chrome follows the window, not the screen. The kit leaves the screen at the window size, so every plain
	// 951x669 context reads as the Duo once the page tells the device from its screen: a small desktop window on a
	// 1440x900 screen keeps the desktop header (review 9702412d F4), and an iPhone on its side draws the rail and
	// pads its page by the 62 px left inset (review 9702412d F1).
	async 'status chrome follows the window, not the screen'(t) {
		const view = { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json') };

		await t.newContext({ viewport: { width: 951, height: 669 }, screen: { width: 1440, height: 900 } });
		await openStatus(t, view);
		await t.page.waitForSelector(CARD);
		await t.equal('a 951x669 window on a 1440x900 screen', await t.page.evaluate(() => document.documentElement.dataset.chrome), 'desk');
		await t.check('the desktop header is drawn', await t.page.evaluate(() => !!document.querySelector('header.chrome') && getComputedStyle(document.querySelector('header.chrome')).display !== 'none'));
		await t.count('.rail, .topbar, .tabbar, .band-head', 0);

		// iPhone 17 Pro on its side: the notch insets land left and right, the rail takes the right strip.
		await t.newContext({ viewport: { width: 874, height: 402 }, screen: { width: 874, height: 402 }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 });
		await openStatus(t, view, '?safe=0,62,21,62');
		await t.page.waitForSelector(CARD);
		await t.equal('an iPhone on its side', await t.page.evaluate(() => document.documentElement.dataset.chrome), 'rail');
		await t.check('the page pads by the 62 px left inset', await t.page.evaluate(() => Math.round(document.querySelector('.content').getBoundingClientRect().left) >= 62));
	},

	// In the shell's iframe the page draws no chrome and keeps no room for one.
	async 'status inside the iframe keeps no chrome'(t) {
		await t.newContext({ viewport: { width: 466, height: 678 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1 });
		await openStatus(t, fixture('status-design-view.json'), '?embed=1&safe=0,84,34,0');
		await t.equal('no chrome inside the iframe', await t.page.evaluate(() => document.documentElement.dataset.chrome), 'none');
		await t.count('.rail, .band-head, .tabbar, .topbar, header.chrome', 0);
		await t.check('nothing is reserved on the right', await t.page.evaluate(() => getComputedStyle(document.querySelector('.content')).width === `${innerWidth}px`));
	},

	// Fix round 1, R1: at the Duo's landscape size the deck's own Back, Next and Review row is replaced by the plate head,
	// but the review screen's Send is the only way to send, so it has to stay.
	async 'status review can be sent at the Duo landscape size'(t) {
		for (const [name, query] of [['with the rail', '?safe=0,84,34,0'], ['with no insets', '?safe=0,0,0,0'], ['in the iframe', '?embed=1']]) {
			await t.newContext({ viewport: { width: 951, height: 669 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1 });
			const sent = await openStatus(t, { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json') }, query);
			await t.page.waitForSelector(CARD);
			await t.page.locator('.plate-tools.only-land .btn', { hasText: /^Next$/ }).click();
			await t.page.waitForSelector('.opts .opt');
			await t.page.locator(`${CARD} .opt-pick`).nth(1).click();
			await t.page.locator('.plate-tools.only-land .primary').click();
			await t.page.waitForSelector('.review');
			const send = t.page.locator('.review .q-send .primary');
			const shown = await send.isVisible();
			await t.check(`${name}: the final Send is on screen`, shown);
			await t.check(`${name}: Back to the questions is on screen`, await t.page.locator('.review .q-send button', { hasText: /Back to the questions/ }).isVisible());
			await t.check(`${name}: the head has no Next or Review during the review`, !(await t.page.locator('.plate-tools.only-land .primary').isVisible()));
			if (!shown) continue; // a click on a hidden button would only time out
			await t.text('.review .q-send .primary', /Send 1 answer/);
			await send.click();
			await t.page.waitForSelector('.sent');
			await t.text('.sent .ok', /Sent 1 at/);
			await t.equal(`${name}: one send`, sent.length, 1);
		}
	},

	// Fix round 1, R2: fixtures/pinned-composer-cases.json is the real output of native/aac-status's composer.
	async 'status says when a pinned report cannot be read'(t) {
		const cases = fixture('pinned-composer-cases.json');
		const withPins = (pinned) => ({ ...fixture('status-design-view.json'), pinned });
		const NOTE = /Could not be read since 8:33 PM\./;
		const opacity = (selector) =>
			t.page.evaluate((s) => {
				let value = 1;
				for (let e = document.querySelector(s); e; e = e.parentElement) value *= parseFloat(getComputedStyle(e).opacity);
				return value;
			}, selector);

		await t.viewport(1440, 900);
		await openStatus(t, withPins(cases.unreadableWithTasks));
		await t.page.waitForSelector('.pin-grid .pin');
		await t.text('.pin-grid .pin', NOTE);
		await t.text('.pin-grid .pin', /cached/i);
		await t.check('card: the cached figures are dimmed', (await opacity('.pin-grid .pin-pct .big')) < 0.6);
		await t.check('card: no line says it was just updated', !/Updated 1 min ago/.test(await t.page.locator('.pin-grid .pin').textContent()));

		await t.newContext(PHONE);
		await openStatus(t, withPins(cases.unreadableWithTasks));
		await t.page.waitForSelector('.pin-list .pin-row');
		await t.text('.pin-list .pin-row', NOTE);
		await t.text('.pin-list .pin-row', /cached/i);
		await t.check('row: the cached percent is dimmed', (await opacity('.pin-list .pin-row .pr-pct')) < 0.6);
		await t.check('row: no cached state reads as current', !/Looping|Updated|min ago/.test(await t.page.locator('.pin-list .pin-row .pr-line').textContent()));
		await t.page.locator('.pin-list .pin-row').click();
		await t.page.waitForSelector('.sheet .pin');
		await t.text('.sheet .pin', NOTE);
		await t.text('.sheet .pin', /cached/i);
		await t.check('sheet: the cached figures are dimmed', (await opacity('.sheet .pin-pct .big')) < 0.6);

		// The composer's own test case, which has no task list behind it.
		await t.newContext(PHONE);
		await openStatus(t, withPins(cases.unreadable));
		await t.page.waitForSelector('.pin-list .pin-row');
		await t.text('.pin-list .pin-row', NOTE);
		await t.text('.pin-list .pin-row', /cached/i);
		await t.page.locator('.pin-list .pin-row').click();
		await t.page.waitForSelector('.sheet .pin');
		await t.text('.sheet .pin', NOTE);
		await t.text('.sheet .pin', /cached/i);
	},

	// Fix round 1, R3: the server doesn't say which task is looping, and a blocked task can sit ahead of it.
	async 'status pinned row names no task the server did not'(t) {
		const cases = fixture('pinned-composer-cases.json');
		await t.newContext(PHONE);
		await openStatus(t, { ...fixture('status-design-view.json'), pinned: cases.loopingBehindBlocked });
		await t.page.waitForSelector('.pin-list .pin-row');
		const line = await t.page.locator('.pin-list .pin-row .pr-line').textContent();
		await t.check('row: says Looping', /^Looping\b/.test(line), line);
		await t.check('row: names no task as the looping one', !/Looping on/.test(line), line);
		await t.page.locator('.pin-list .pin-row').click();
		await t.page.waitForSelector('.sheet .pin');
		await t.text('.sheet .pin', /F-07 has gone back for fixes 3 times/);
	},

	// Data the server doesn't send yet is said so, never guessed.
	async 'status says what it cannot show'(t) {
		await t.viewport(1440, 900);
		await openStatus(t, fixture('status-view.json'));
		await t.text('.sec-spend', /isn't available from this server yet/);
		await t.text('.sec-pinned', /unavailable/);
		const down = { ...fixture('status-design-view.json'), pinned: { unavailable: "Pinned threads unavailable: T3's state file changed shape." }, spendByStream: { state: 'unavailable', unavailable: 'Spend by stream is unavailable: quota-watch has not written a report this server can read.', periods: [] } };
		await t.page.unroute((u) => new URL(u).pathname === '/api/status');
		await t.page.route((u) => new URL(u).pathname === '/api/status', (r) => r.fulfill({ json: down }));
		await t.goto('/app/status', { settleMs: 300 });
		await t.page.waitForSelector('#cards');
		await t.text('.sec-pinned', /changed shape/);
		await t.text('.sec-spend', /has not written a report/);
		await t.check('no percentages invented', await t.page.evaluate(() => !/\d%/.test(document.querySelector('.sec-spend').textContent)));
	},

	async 'status deck on a phone'(t) {
		await t.newContext(PHONE);
		const sent = await openStatus(t, fixture('status-view.json'));
		await t.page.waitForSelector(CARD);
		await t.audit('phone status');
		await t.shot('phone-card', 'first card on a 402 px phone: card fills the width, option media, readable spacing');

		// A wide screenshot opens full screen turned sideways on an upright phone; Rotate, full size and Close work.
		await t.page.click('.media-open >> nth=0');
		await t.page.waitForSelector('.viewer img');
		await t.until('wide screenshot opens sideways', () => document.querySelector('.viewer')?.dataset.rotated === '1');
		await t.shot('phone-viewer', 'screenshot full screen, sideways: fills the screen, bar reachable');
		await t.page.click('.viewer-frame img');
		await t.until('tap shows full size', () => document.querySelector('.viewer')?.dataset.actual === '1');
		await t.page.click('.viewer-btn >> text=Rotate');
		await t.until('Rotate turns it upright', () => !document.querySelector('.viewer')?.dataset.rotated);
		await t.page.click('.viewer-btn.close');
		await t.count('.viewer', 0);

		// The video plays inline (byte ranges served) and opens in the viewer muted, with the system player one tap away.
		await t.check('inline video ready', async () =>
			t.page.evaluate(() => {
				const v = document.querySelector('.media video');
				return !!v && v.controls && v.playsInline && !!v.poster;
			})
		);
		await t.page.click('.media[data-kind="video"] button >> text=Full screen');
		await t.page.waitForSelector('.viewer video');
		await t.check('viewer video is muted and inline', async () =>
			t.page.evaluate(() => {
				const v = document.querySelector('.viewer video');
				return v.muted && v.playsInline && v.autoplay;
			})
		);
		await t.visible('.viewer-btn >> text=System player', 'system player button');
		await t.page.click('.viewer-btn.close');

		// Walk the deck: answer, object with words, mark an action done; then review and send once.
		await t.page.click('.opt-pick >> nth=0');
		await t.page.click(NEXT); // S-Q1 (Scout comparison)
		await t.page.waitForSelector('.block[data-type="compare"]');
		await t.page.click(NEXT); // C-Q5a (exception)
		await t.visible('.q-rule', 'an exception to a standing rule shows first');
		await t.page.click(NEXT); // C-Q5b (objection)
		await t.page.click('.q-actions .btn');
		await t.visible('text=An objection needs a reason', 'an objection asks for words');
		await t.page.fill('textarea.note', 'Keep whole-file hashes for the audit folder only.');
		await t.page.click(NEXT); // C-Q5c
		await t.page.click(NEXT); // T-A1 (action)
		await t.page.click('.q-actions .btn');
		await t.text('.q-actions .btn', /Marked done/);
		await t.equal('nothing sent before the review', sent.length, 0);
		await t.page.click(NEXT); // Review
		await t.page.waitForSelector('.review');
		await t.text('.review .q-send .primary', /Send 3 answers/);
		await t.shot('phone-review', 'review screen: each card says what will be sent');
		await t.page.click('.review .q-send .primary');
		await t.page.waitForSelector('.sent');
		await t.text('.sent .ok', /Sent 3 at/);
		const entries = sent[0]?.answers || [];
		await t.equal('one send', sent.length, 1);
		await t.check('the send carries its id (a content hash, so a retry is recorded once)', /^[0-9a-f]{32}$/.test(sent[0]?.sendId || ''));
		await t.equal('kinds sent', entries.map((a) => a.kind).sort().join(','), 'answer,done,object');
		await t.check('every entry names the exact card version', entries.every((a) => a.cardId && a.cardSha256 && Number.isInteger(a.cardSeq)));
		await t.check('the objection carries its words', entries.some((a) => a.kind === 'object' && /audit folder/.test(a.note)));
	},

	// The note box follows the draft without moving the cursor on a re-read, and a send's result outlives a read that
	// empties the cards while it is on its way (the review stays; "No questions waiting" waits for the result).
	async 'status typing and a send that outlives the cards'(t) {
		await t.viewport(1440, 900);
		await t.app.signIn();
		const full = fixture('status-view.json');
		let current = full;
		let gets = 0;
		let release;
		const held = new Promise((resolve) => (release = resolve));
		await t.page.route((u) => new URL(u).pathname === '/api/status', (r) => {
			gets += 1;
			return r.fulfill({ json: current });
		});
		await t.page.route((u) => new URL(u).pathname === '/api/status/answers', async (r) => {
			const body = JSON.parse(r.request().postData() || '{}');
			await held;
			await r.fulfill({ json: { results: (body.answers || []).map((a) => ({ cardId: a.cardId, outcome: 'recorded' })) } });
		});
		await stubMedia(t);
		await t.goto('/app/status', { settleMs: 300 });
		await t.page.waitForSelector(`${CARD} textarea.note`);
		const reread = async () => {
			const before = gets;
			await t.page.evaluate(() => window.dispatchEvent(new Event('online')));
			for (let i = 0; i < 50 && gets === before; i++) await t.page.waitForTimeout(100);
			await t.page.waitForTimeout(300);
			return gets > before;
		};
		await t.page.fill('textarea.note', 'abc def');
		await t.page.evaluate(() => document.querySelector('textarea.note').setSelectionRange(3, 3));
		await t.page.keyboard.type('X');
		await t.check('a re-read happened while typing', await reread());
		await t.check('typing survives a re-read: same words, cursor where it was', async () =>
			t.page.evaluate(() => {
				const a = document.querySelector('textarea.note');
				return a.value === 'abcX def' && a.selectionStart === 4 && document.activeElement === a;
			})
		);
		await t.page.keyboard.type('Y');
		await t.check('the next keystroke lands at the cursor', async () =>
			t.page.evaluate(() => document.querySelector('textarea.note').value === 'abcXY def')
		);
		// Send that note; while the POST is held, a read finds no cards.
		await t.page.click('.q-send .primary');
		await t.page.waitForSelector('.review .q-send .primary');
		await t.page.click('.review .q-send .primary');
		current = { ...full, cards: { ...full.cards, open: [], batches: [] } };
		await t.check('a re-read happened mid-send', await reread());
		await t.check('the review stays while the send is on its way', async () =>
			t.page.evaluate(() => !!document.querySelector('.review') && !/No questions waiting/.test(document.querySelector('.sec-questions')?.textContent || ''))
		);
		release();
		await t.page.waitForSelector('.sent', { timeout: 10000 });
		await t.text('.sent .ok', /Sent 1 at/);
		await t.page.click('.sent button >> text=Back to the questions');
		await t.text('.sec-questions .q-empty', /No questions waiting/);
	},

	// A send that fails after a read emptied the cards keeps its review and says nothing is left to send; an idle
	// review whose cards closed gives way, and a question that arrives later opens on the deck.
	async 'status a failed send and an idle review when the cards empty'(t) {
		await t.viewport(1440, 900);
		await t.app.signIn();
		const full = fixture('status-view.json');
		const empty = { ...full, cards: { ...full.cards, open: [], batches: [] } };
		let current = full;
		let gets = 0;
		let release;
		const held = new Promise((resolve) => (release = resolve));
		await t.page.route((u) => new URL(u).pathname === '/api/status', (r) => {
			gets += 1;
			return r.fulfill({ json: current });
		});
		await t.page.route((u) => new URL(u).pathname === '/api/status/answers', async (r) => {
			await held;
			await r.fulfill({ status: 500, json: { error: 'Boom.', code: 'answers_failed' } });
		});
		await stubMedia(t);
		await t.goto('/app/status', { settleMs: 300 });
		await t.page.waitForSelector(`${CARD} textarea.note`);
		const reread = async () => {
			const before = gets;
			await t.page.evaluate(() => window.dispatchEvent(new Event('online')));
			for (let i = 0; i < 50 && gets === before; i++) await t.page.waitForTimeout(100);
			await t.page.waitForTimeout(300);
			return gets > before;
		};
		// an idle review: the cards close under it, then a new question arrives
		await t.page.click('.q-send .primary');
		await t.page.waitForSelector('.review');
		current = empty;
		await t.check('re-read (cards gone)', await reread());
		await t.text('.sec-questions .q-empty', /No questions waiting/);
		current = full;
		await t.check('re-read (a question back)', await reread());
		await t.check('the question opens on the deck, not the old review', async () =>
			t.page.evaluate(() => !!document.querySelector('article.q-card') && !document.querySelector('.review'))
		);
		// a send that fails after the cards emptied
		await t.page.fill('textarea.note', 'one note');
		await t.page.click('.q-send .primary');
		await t.page.click('.review .q-send .primary');
		current = empty;
		await t.check('re-read mid-send', await reread());
		release();
		await t.page.waitForSelector('.review [role="alert"]', { timeout: 10000 });
		await t.text('.review [role="alert"]', /nothing is left to send/);
		await t.check('the failure says nothing about sending again', async () =>
			t.page.evaluate(() => !/Sending the same answers again/.test(document.querySelector('.review [role="alert"]').textContent))
		);
		await t.page.click('.review .q-send button >> text=Back');
		await t.text('.sec-questions .q-empty', /No questions waiting/);
		// the 500 this test answered on purpose is the only console error allowed
		await t.noConsoleErrors('console (only the stubbed 500 on /api/status/answers)', { allow: [/status of 500 .*\/api\/status\/answers/] });
	},
};
