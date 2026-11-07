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

async function openStatus(t, view) {
	await t.app.signIn();
	const sent = await stubStatus(t, view);
	await t.goto('/app/status', { settleMs: 300 });
	await t.page.waitForSelector('.panel#cards');
	return sent;
}

const PANELS = ['cards', 'progress', 'threads', 'machines', 'simlock', 'usage'];

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
		await t.page.waitForSelector('.deck .card');
		await t.text('.deck-sub', /9 open/);
		// The fixture's first card has word keys ("vm", "windows"): each fits its badge on one line.
		await t.check('option keys fit their badges', async () =>
			t.page.evaluate(() => [...document.querySelectorAll('.opt-key')].every((k) => k.scrollWidth <= k.clientWidth + 1 && k.getBoundingClientRect().height <= 27))
		);
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
				const card = document.querySelector('.deck .card');
				return !!card && ['question', ''].includes(card.dataset.kind || '') && !!card.querySelector('.opt-pick:not([disabled])');
			});
			if (!found) await t.page.click('.deck-nav .primary');
		}
		await t.check('a question card to answer', found);
		const before = signedAnswers(t).length;
		await t.page.click('.deck .card .opt-pick >> nth=0');
		await t.page.click('.jump button >> text=Review');
		await t.page.waitForSelector('.review');
		await t.text('.review-actions .primary', /Send 1 answer/);
		await t.page.click('.review-actions .primary');
		await t.page.waitForSelector('.sent');
		await t.text('.sent .ok', /Sent 1 at/);
		await t.equal('one signed answer written to the test reports folder', signedAnswers(t).length, before + 1);
		await t.until('the answered card leaves the deck', () => /8 open/.test(document.querySelector('.deck-sub')?.textContent || ''), null, { timeout: 15000 });
		await t.audit('end to end 1440');
	},

	async 'status desktop'(t) {
		await t.viewport(1440, 900);
		await openStatus(t, fixture('status-view.json'));
		for (const id of PANELS) await t.count(`.panel#${id}`, 1);
		await t.page.waitForSelector('.deck .card');
		await t.text('.deck-sub', /6 open/);
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
		await t.page.waitForSelector('.deck .card');
		await t.text('.deck-sub', /9 open/);
		await t.text('.panel#machines', /ssh refused/);
		await t.text('.panel#machines', /under the 50 GB floor/);
		await t.text('.panel#cards .src', /Unreadable/);
		await t.check('nothing reads as zero for a machine with no reading', async () =>
			t.page.evaluate(() => {
				const nas = [...document.querySelectorAll('.machine')].find((m) => /Nas1/.test(m.textContent));
				return !!nas && !/\b0(\.0)? ?GB free/.test(nas.textContent);
			})
		);
		// The threads' card additions: a verdict filter, a judges' split, a sample that sends nothing, verbatim quotes.
		await t.count('.verdicts .chip-btn', 2);
		await t.page.click('.verdicts .chip-btn[data-verdict="adapt"]');
		await t.text('.deck .card .card-label', 'F-Q4');
		await t.visible('text=The judges split', 'split judges are flagged');
		await t.page.click('.verdicts .chip-btn >> text=All');
		await t.page.click('.jump button >> text=C-S1');
		await t.visible('.sample-banner', 'a sample card says no answer is wanted');
		await t.check('a sample card cannot be answered', async () => t.page.evaluate(() => [...document.querySelectorAll('.deck .card .opt-pick')].every((b) => b.disabled)));
		await t.page.click('.jump button >> text=ST-Q2');
		await t.count('.deck .card .quote', 1);
		await t.count('.deck .card .plan-change', 1);
		await t.audit('server view 1440');
		await t.crop('status-machines', '.panel#machines', 'unreachable machines say why, and show no invented readings');
	},

	// The iPhone Duo's panels: closed (outer, 466x678), open (inner, 669x951) and open sideways. Layout only, in
	// Chromium: Simlock's Duo can't change its pose yet, so the open panel isn't tried on the simulator.
	async 'status at iPhone Duo sizes'(t) {
		for (const [w, h, name] of [[466, 678, 'closed'], [669, 951, 'open'], [951, 669, 'open sideways']]) {
			await t.newContext({ viewport: { width: w, height: h }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 });
			await openStatus(t, fixture('server-view.json'));
			await t.page.waitForSelector('.deck .card');
			await t.audit(`Duo ${name} ${w}x${h}`);
			await t.check(`Duo ${name}: Back and Next on screen`, async () =>
				t.page.evaluate(() => {
					const r = document.querySelector('.deck-nav')?.getBoundingClientRect();
					return !!r && r.bottom <= innerHeight + 1 && r.top >= 0 && r.right <= innerWidth + 1;
				})
			);
			await t.snap(`duo-${name.replace(' ', '-')}`);
		}
	},

	async 'status deck on a phone'(t) {
		await t.newContext(PHONE);
		const sent = await openStatus(t, fixture('status-view.json'));
		await t.page.waitForSelector('.deck .card');
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
		await t.page.click('.deck-nav .primary'); // S-Q1 (Scout comparison)
		await t.page.waitForSelector('.block[data-type="compare"]');
		await t.page.click('.deck-nav .primary'); // C-Q5a (exception)
		await t.visible('.exception', 'an exception to a standing rule shows first');
		await t.page.click('.deck-nav .primary'); // C-Q5b (objection)
		await t.page.click('.toggle');
		await t.visible('text=An objection needs a reason', 'an objection asks for words');
		await t.page.fill('.note textarea', 'Keep whole-file hashes for the audit folder only.');
		await t.page.click('.deck-nav .primary'); // C-Q5c
		await t.page.click('.deck-nav .primary'); // T-A1 (action)
		await t.page.click('.toggle');
		await t.text('.toggle', /Marked done/);
		await t.equal('nothing sent before the review', sent.length, 0);
		await t.page.click('.deck-nav .primary'); // Review
		await t.page.waitForSelector('.review');
		await t.text('.review-actions .primary', /Send 3 answers/);
		await t.shot('phone-review', 'review screen: each card says what will be sent');
		await t.page.click('.review-actions .primary');
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
		await t.page.waitForSelector('.deck .card .note textarea');
		const reread = async () => {
			const before = gets;
			await t.page.evaluate(() => window.dispatchEvent(new Event('online')));
			for (let i = 0; i < 50 && gets === before; i++) await t.page.waitForTimeout(100);
			await t.page.waitForTimeout(300);
			return gets > before;
		};
		await t.page.fill('.note textarea', 'abc def');
		await t.page.evaluate(() => document.querySelector('.note textarea').setSelectionRange(3, 3));
		await t.page.keyboard.type('X');
		await t.check('a re-read happened while typing', await reread());
		await t.check('typing survives a re-read: same words, cursor where it was', async () =>
			t.page.evaluate(() => {
				const a = document.querySelector('.note textarea');
				return a.value === 'abcX def' && a.selectionStart === 4 && document.activeElement === a;
			})
		);
		await t.page.keyboard.type('Y');
		await t.check('the next keystroke lands at the cursor', async () =>
			t.page.evaluate(() => document.querySelector('.note textarea').value === 'abcXY def')
		);
		// Send that note; while the POST is held, a read finds no cards.
		await t.page.click('button[role="tab"] >> text=Review');
		await t.page.waitForSelector('.review-actions .primary');
		await t.page.click('.review-actions .primary');
		current = { ...full, cards: { ...full.cards, open: [], batches: [] } };
		await t.check('a re-read happened mid-send', await reread());
		await t.check('the review stays while the send is on its way', async () =>
			t.page.evaluate(() => !!document.querySelector('.review') && !/No questions waiting/.test(document.querySelector('#cards')?.textContent || ''))
		);
		release();
		await t.page.waitForSelector('.sent', { timeout: 10000 });
		await t.text('.sent .ok', /Sent 1 at/);
		await t.page.click('.sent button >> text=Back to the questions');
		await t.text('#cards .empty', /No questions waiting/);
	},
};
