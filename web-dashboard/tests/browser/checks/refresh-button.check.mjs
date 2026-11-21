// The header's Refresh (owner report 2026-11-20: "The refresh button needs to have visual feedback when you click it,
// especially in the pwa. It should also animate while it's refreshing."). chrome.rs RefreshButton, refreshing.rs.
// - A press shows at once: `.pressed` (scaled, tinted) from the pointer's down, with a mouse on the desktop header and a
//   real touch on the phone's top bar, and it lets go on the pointer's up.
// - While the refresh it started runs, the icon turns (a CSS animation on the icon), the button is aria-busy and says
//   "Refreshing", and it stops when that refresh's reads answer: Status' own read, or the dashboard controller's on Home.
//   A fast refresh still shows for about 600 ms. A second press while it runs sends nothing more.
// - With reduced motion the icon pulses instead of turning.
// - The phone top bar and the Duo rail stay dark in the light theme: the press tint is their dark one (review M1).
// - A drag off the button or a cancelled touch takes the press back and starts nothing; a refresh that fails (HTTP 500)
//   stops turning too and the page says it failed (review L4).
// Every test opens its own context with reduced motion off (`no-preference`), and the one that turns it on restores it,
// so no other check inherits it (review M2: browser-check keeps the last emulated media for later pages).
// Real test server and sign-in (the kit's temporary home). Status and Home are answered in the page with made-up data
// (leptos/fixtures, home-fixture.mjs), so no provider is asked for anything.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos');
const STATUS_VIEW = JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', 'status-design-view.json'), 'utf8'));

const DESKTOP = { viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 };
// iPhone 17 Pro's CSS viewport, touch.
const PHONE = { viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 };
const BUTTON = { desktop: '.chrome .refresh-btn', phone: '.topbar .refresh-btn' };

/**
 * Answers `pathname` with `json`, counting the requests whose query matches `counted`; `hold()` keeps the next ones
 * waiting until `release()`.
 */
async function answered(t, pathname, json, counted = () => true) {
	const s = { count: 0, held: [], holding: false, failing: false };
	await t.page.route((u) => new URL(u).pathname === pathname, async (route) => {
		const isCounted = counted(new URL(route.request().url()));
		if (isCounted) s.count++;
		if (s.holding) await new Promise((resolve) => s.held.push(resolve));
		const answer = s.failing && isCounted ? { status: 500, json: { error: 'Synthetic failure for the kit.' } } : { json };
		await route.fulfill(answer).catch(() => {});
	});
	return {
		get count() { return s.count; },
		get waiting() { return s.held.length; },
		/** The counted requests answer HTTP 500 from now on. */
		fail(on = true) { s.failing = on; },
		hold() { s.holding = true; },
		release() { s.holding = false; for (const go of s.held.splice(0)) go(); },
	};
}

async function openPage(t, page, profile, { reducedMotion = 'no-preference', theme, query = '' } = {}) {
	await t.newContext({ ...profile, serviceWorkers: 'block', reducedMotion });
	if (theme) await t.page.addInitScript((mode) => { try { localStorage.setItem('aac-theme', mode); } catch {} }, theme);
	await t.app.signIn();
	let source;
	if (page === 'status') {
		source = await answered(t, '/api/status', STATUS_VIEW);
	} else {
		await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
		source = await answered(t, '/api/accounts/dashboard', homeAccounts(), (u) => u.searchParams.get('refresh') === 'true');
	}
	await t.goto(`/app/${page}?e2e${query ? `&${query}` : ''}`, { settleMs: 300 });
	await t.until(`${page} is drawn, signed in, with its first reading`, () => !!document.querySelector('.shell') && !document.querySelector('.boot-splash')
		&& !!document.querySelector('.status-line b')?.textContent, null, { timeout: 60000, hard: true });
	return source;
}

/** The button and its icon as the page has them now. */
const look = (t, selector) => t.page.evaluate((sel) => {
	const b = document.querySelector(sel);
	if (!b) return null;
	const icon = b.querySelector('svg');
	const style = getComputedStyle(b);
	return {
		pressed: b.classList.contains('pressed'),
		refreshing: b.classList.contains('refreshing'),
		busy: b.getAttribute('aria-busy'),
		label: b.getAttribute('aria-label'),
		title: b.getAttribute('title'),
		transform: style.transform,
		background: style.backgroundColor,
		iconAnimation: getComputedStyle(icon).animationName,
		running: icon.getAnimations().filter((a) => a.playState === 'running').map((a) => a.animationName),
	};
}, selector);

const center = async (t, selector) => {
	const box = await t.page.locator(selector).boundingBox();
	return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
};

/** Polls `read` in the test until `ok`, for up to `ms`; returns the last value and how long it took. */
async function eventually(t, read, ok, ms = 8000) {
	const start = Date.now();
	let value;
	for (;;) {
		value = await read();
		if (ok(value) || Date.now() - start > ms) return { value, ms: Date.now() - start };
		await t.page.waitForTimeout(50);
	}
}

const idle = (s) => s && !s.refreshing && s.busy === null && s.label === 'Refresh' && s.running.length === 0;
const spinning = (s) => s && s.refreshing && s.busy === 'true' && s.label === 'Refreshing' && s.title === 'Refreshing'
	&& s.iconAnimation === 'ac-refresh-turn' && s.running.includes('ac-refresh-turn');

/** Press and hold with the mouse: `.pressed` at once; let go: released, and the click starts the refresh. */
async function pressWithMouse(t, selector) {
	const { x, y } = await center(t, selector);
	const rest = await look(t, selector);
	await t.page.mouse.move(x, y);
	await t.page.mouse.down();
	const down = await look(t, selector);
	await t.check('pressed: .pressed from the pointer\'s down, scaled and tinted', down.pressed && down.transform !== 'none' && down.background !== rest.background, JSON.stringify({ rest, down }));
	await t.page.mouse.up();
	const up = await look(t, selector);
	await t.check('let go: .pressed is gone with the pointer\'s up', !up.pressed, JSON.stringify(up));
	return up;
}

/** The same with a real touch (Chrome's touch input, so pointer and click events come as a finger's). */
async function pressWithFinger(t, selector) {
	const { x, y } = await center(t, selector);
	const cdp = await t.page.context().newCDPSession(t.page);
	try {
		await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] });
		const down = await look(t, selector);
		await t.check('touched: .pressed while the finger is down', down.pressed && down.transform !== 'none', JSON.stringify(down));
		await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
	} finally {
		await cdp.detach().catch(() => {});
	}
	const up = await eventually(t, () => look(t, selector), (s) => !s.pressed, 2000);
	await t.check('lifted: .pressed is gone', !up.value.pressed, JSON.stringify(up.value));
	return up.value;
}

/** Press, see it spin until the held read is let go, then stop. A second press while it runs sends nothing more. */
async function spinsUntilAnswered(t, profile, page, source, press) {
	const selector = BUTTON[profile];
	await t.check(`${profile} ${page}: idle before a press`, idle(await look(t, selector)), JSON.stringify(await look(t, selector)));
	const before = source.count;
	source.hold();
	await press(t, selector);
	const started = await eventually(t, () => look(t, selector), spinning, 2000);
	await t.check(`${profile} ${page}: the icon turns, aria-busy, "Refreshing"`, spinning(started.value), JSON.stringify(started.value));
	await eventually(t, async () => source.waiting, (n) => n >= 1, 8000);
	await t.check(`${profile} ${page}: its read is out (held here)`, source.waiting >= 1, `waiting ${source.waiting}`);
	// a second press while it runs starts nothing
	await t.page.click(selector);
	await t.page.waitForTimeout(1200);
	const held = await look(t, selector);
	await t.check(`${profile} ${page}: still turning while the read is out (past the minimum)`, spinning(held), JSON.stringify(held));
	await t.equal(`${profile} ${page}: one refresh read for two presses`, source.count - before, 1);
	source.release();
	const stopped = await eventually(t, () => look(t, selector), idle, 5000);
	await t.check(`${profile} ${page}: it stops once the read has answered`, idle(stopped.value) && stopped.ms < 3000, `${stopped.ms} ms: ${JSON.stringify(stopped.value)}`);
	// a fast refresh still shows: the read answers at once, the icon turns for about 600 ms
	await t.page.click(selector);
	const at = Date.now();
	const quick = await eventually(t, () => look(t, selector), idle, 5000);
	await t.check(`${profile} ${page}: a fast refresh shows for the minimum, then stops`, idle(quick.value) && Date.now() - at >= 500 && quick.ms < 3000, `${Date.now() - at} ms`);
}

/** Reduced motion, desktop and phone: the icon pulses its opacity and never turns, and stops when the read answers. */
async function reducedMotionPulses(t) {
	for (const [profile, setup] of [['desktop', DESKTOP], ['phone', PHONE]]) {
		const source = await openPage(t, 'status', setup, { reducedMotion: 'reduce' });
		const selector = BUTTON[profile];
		source.hold();
		await t.page.click(selector);
		const on = await eventually(t, () => look(t, selector), (s) => s.refreshing, 2000);
		await t.check(`${profile}: reduced motion: the icon pulses, never turns`, on.value.busy === 'true' && on.value.iconAnimation === 'ac-refresh-pulse'
			&& on.value.running.includes('ac-refresh-pulse') && !on.value.running.includes('ac-refresh-turn'), JSON.stringify(on.value));
		source.release();
		const off = await eventually(t, () => look(t, selector), idle, 5000);
		await t.check(`${profile}: reduced motion: and stops when the read answers`, idle(off.value), JSON.stringify(off.value));
	}
}

/** The computed colour of `var(name)` where `selector` is (the bars override the page's tokens). */
const tokenColour = (t, selector, name) => t.page.evaluate(([sel, token]) => {
	const probe = document.createElement('span');
	probe.style.color = `var(${token})`;
	document.querySelector(sel).append(probe);
	const colour = getComputedStyle(probe).color;
	probe.remove();
	return colour;
}, [selector, name]);

/**
 * Watches `selector` for `.refreshing` from now on (a request count can't say it: Status also reads on its own, from
 * its socket and poll). `seen()` says whether it ever turned.
 */
async function watchRefreshing(t, selector) {
	await t.page.evaluate((sel) => {
		const b = document.querySelector(sel);
		window.__kitRefreshSeen = b.classList.contains('refreshing');
		window.__kitRefreshWatch?.disconnect();
		window.__kitRefreshWatch = new MutationObserver(() => { if (b.classList.contains('refreshing')) window.__kitRefreshSeen = true; });
		window.__kitRefreshWatch.observe(b, { attributes: true, attributeFilter: ['class', 'aria-busy'] });
	}, selector);
	return { seen: () => t.page.evaluate(() => window.__kitRefreshSeen === true) };
}

/** Touch down on `selector` (CDP touch input); returns a session to end or cancel the touch with. */
async function touchDown(t, selector) {
	const { x, y } = await center(t, selector);
	const cdp = await t.page.context().newCDPSession(t.page);
	await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] });
	return cdp;
}

export const tests = {
	async 'refresh: desktop Status press feedback, spins until its read answers'(t) {
		const source = await openPage(t, 'status', DESKTOP);
		await spinsUntilAnswered(t, 'desktop', 'status', source, pressWithMouse);
		await t.noConsoleErrors('console: clean');
	},

	async 'refresh: desktop Home spins until the dashboard controller has read'(t) {
		const source = await openPage(t, 'home', DESKTOP);
		await spinsUntilAnswered(t, 'desktop', 'home', source, pressWithMouse);
		await t.noConsoleErrors('console: clean');
	},

	async 'refresh: phone Status, a finger shows the press and the top bar spins'(t) {
		const source = await openPage(t, 'status', PHONE);
		await t.equal('the phone chrome is drawn', await t.page.evaluate(() => document.documentElement.dataset.chrome), 'phone');
		await spinsUntilAnswered(t, 'phone', 'status', source, pressWithFinger);
		await t.noConsoleErrors('console: clean');
	},

	async 'refresh: phone Home, a finger shows the press and the top bar spins'(t) {
		const source = await openPage(t, 'home', PHONE);
		await spinsUntilAnswered(t, 'phone', 'home', source, pressWithFinger);
		await t.noConsoleErrors('console: clean');
	},

	async 'refresh: reduced motion pulses instead of turning'(t) {
		try {
			await reducedMotionPulses(t);
		} finally {
			// reduced motion is this test's alone (review M2)
			await t.emulate({ reducedMotion: 'no-preference' });
		}
	},

	async 'refresh: light and dark themes, pressed and refreshing'(t) {
		for (const theme of ['light', 'dark']) {
			const source = await openPage(t, 'status', DESKTOP, { theme });
			const selector = BUTTON.desktop;
			await t.equal(`${theme}: the theme applies`, await t.page.evaluate(() => document.documentElement.dataset.theme), theme);
			const rest = await look(t, selector);
			const { x, y } = await center(t, selector);
			await t.page.mouse.move(x, y);
			await t.page.mouse.down();
			const down = await look(t, selector);
			await t.check(`${theme}: pressed is tinted apart from rest`, down.pressed && down.background !== rest.background, JSON.stringify({ rest: rest.background, down: down.background }));
			await t.crop(`refresh-pressed-${theme}`, '.chrome .chrome-right', `${theme}: the Refresh button pressed (scaled, tinted)`);
			source.hold();
			await t.page.mouse.up();
			const turning = await eventually(t, () => look(t, selector), spinning, 2000);
			await t.check(`${theme}: the icon turns, aria-busy, "Refreshing"`, spinning(turning.value), JSON.stringify(turning.value));
			const colours = await t.page.evaluate((sel) => {
				const probe = document.createElement('span');
				probe.style.color = 'var(--accent)';
				document.body.append(probe);
				const accent = getComputedStyle(probe).color;
				probe.remove();
				return { accent, icon: getComputedStyle(document.querySelector(sel).querySelector('svg')).color };
			}, selector);
			await t.equal(`${theme}: while refreshing the icon is in the theme's accent colour`, colours.icon, colours.accent);
			await t.page.waitForTimeout(250);
			// the shot is a frame of the turn: the icon is rotated, not at rest
			const angle = await t.page.evaluate((sel) => {
				const m = getComputedStyle(document.querySelector(sel).querySelector('svg')).transform;
				const v = /matrix\(([^)]+)\)/.exec(m)?.[1].split(',').map(Number);
				return v ? Math.round((Math.atan2(v[1], v[0]) * 180) / Math.PI) : null;
			}, selector);
			await t.check(`${theme}: mid-turn when shot (icon rotated ${angle} deg)`, angle !== null && angle !== 0, String(angle));
			await t.crop(`refresh-turning-${theme}`, '.chrome .chrome-right', `${theme}: Refresh while it refreshes (icon in the accent colour, mid-turn)`);
			source.release();
			await eventually(t, () => look(t, selector), idle, 5000);
		}
	},

	async 'refresh: light theme phone top bar and Duo rail press with their own dark tint'(t) {
		for (const [name, setup, query, selector, group] of [
			['phone', PHONE, '', '.topbar .refresh-btn', '.topbar .tools'],
			['duo rail', { viewport: { width: 466, height: 678 }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 }, 'safe=0,84,34,0', '.rail .refresh-btn', '.rail .rail-tools'],
		]) {
			const source = await openPage(t, 'status', setup, { theme: 'light', query });
			await t.equal(`${name}: the light theme applies`, await t.page.evaluate(() => document.documentElement.dataset.theme), 'light');
			const bar = selector.split(' ')[0];
			const darkSoft = await tokenColour(t, bar, '--accent-soft');
			await t.equal(`${name}: the bar's own (dark) accent tint`, darkSoft, 'rgb(26, 39, 66)');
			const watch = await watchRefreshing(t, selector);
			const cdp = await touchDown(t, selector);
			try {
				const down = await look(t, selector);
				await t.check(`${name}: pressed in the light theme is the bar's dark tint, not the page's light one`, down.pressed && down.background === darkSoft && down.transform !== 'none', JSON.stringify(down));
				await t.crop(`refresh-pressed-${name.replace(' ', '-')}-light`, group, `${name}, light theme: Refresh pressed inside the dark bar (dark tint, light-blue icon)`);
				await cdp.send('Input.dispatchTouchEvent', { type: 'touchCancel', touchPoints: [] });
			} finally {
				await cdp.detach().catch(() => {});
			}
			// review L4: a cancelled touch takes the press back and starts nothing
			const after = await eventually(t, () => look(t, selector), (s) => !s.pressed, 2000);
			await t.page.waitForTimeout(700);
			await t.check(`${name}: a cancelled touch lets go and starts no refresh`, !after.value.pressed && idle(await look(t, selector)) && !(await watch.seen()), JSON.stringify(after.value));
		}
	},

	async 'refresh: dragging off the button lets go and starts nothing'(t) {
		await openPage(t, 'status', DESKTOP);
		const selector = BUTTON.desktop;
		const watch = await watchRefreshing(t, selector);
		const { x, y } = await center(t, selector);
		await t.page.mouse.move(x, y);
		await t.page.mouse.down();
		await t.check('pressed on the button', (await look(t, selector)).pressed);
		await t.page.mouse.move(x, y + 200, { steps: 4 });
		await t.check('dragged off: .pressed is gone at once', !(await look(t, selector)).pressed);
		await t.page.mouse.up();
		await t.page.waitForTimeout(700);
		await t.check('let go off the button: no refresh starts', idle(await look(t, selector)) && !(await watch.seen()));
	},

	async 'refresh: a refresh that fails stops turning and the page says so'(t) {
		for (const page of ['status', 'home']) {
			const source = await openPage(t, page, DESKTOP);
			const selector = BUTTON.desktop;
			source.fail();
			source.hold();
			await t.page.click(selector);
			const on = await eventually(t, () => look(t, selector), spinning, 2000);
			await t.check(`${page}: turning while the failing read is out`, spinning(on.value), JSON.stringify(on.value));
			await eventually(t, async () => source.waiting, (n) => n >= 1, 8000);
			source.release();
			const off = await eventually(t, () => look(t, selector), idle, 5000);
			await t.check(`${page}: it stops once the read has failed`, idle(off.value) && off.ms < 3000, `${off.ms} ms: ${JSON.stringify(off.value)}`);
			const said = await eventually(t, () => t.page.evaluate((p) => p === 'status'
				? document.querySelector('.route:not([hidden]) .banner')?.textContent || ''
				: (window.__aacToasts || []).map((x) => x.title).join(' | '), page), (text) => /answered 500|Synthetic failure|could not|Refresh failed/i.test(text), 5000);
			await t.check(`${page}: the page says the refresh failed`, /answered 500|Synthetic failure|could not|Refresh failed/i.test(said.value), said.value);
			await t.noConsoleErrors(`${page}: console: only the failed read`, { allow: [/status of 500/] });
		}
	},
};
