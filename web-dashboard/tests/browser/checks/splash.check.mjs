// The loading splash (owner report 2026-11-20: "When the page first loads, it shows the empty login screen then that
// goes away and the page loads ... It should be some kind of loading splash screen"). Every animation frame from the
// navigation's start is sampled in the page:
//   - signed in: the sign-in form never exists in any frame, the splash is drawn from the first frame until the app
//     has its first data, and it hands over straight to the drawn page (desktop, and a phone in the installed app's
//     standalone display mode);
//   - sign-in required and no session: the splash, then the form, and never the splash again (no loop);
//   - Status, Analytics and the Scoreboard opened for the first time: the page's own splash in its content area, then
//     its content, and no "Reading..." words in the header's corner.
// `--set shots=1` also takes the splash screenshots for review (light and dark, desktop and phone, and a page splash).
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';
import { analyticsFixture } from '../device/analytics-fixture.mjs';

/** Samples what each frame shows from the first script on: form, splash, app (the frame with its page) or blank. */
function sampler() {
	const s = (window.__splash = { frames: 0, states: [], handoff: null, corner: [] });
	let last = '';
	const tick = () => {
		s.frames++;
		const boot = document.querySelector('.boot-splash:not(.boot-done)');
		const state = document.querySelector('.signin-page') ? 'form' : boot ? 'splash' : document.querySelector('.shell') ? 'app' : document.body ? 'blank' : 'parsing';
		if (state !== last) {
			s.states.push(state);
			if (last === 'splash' && state === 'app') {
				const page = document.querySelector('.route:not([hidden]) .page');
				s.handoff = { rows: document.querySelectorAll('.route:not([hidden]) .hrow, .route:not([hidden]) .hcard').length, pageSplash: !!page?.querySelector('.page-splash') };
			}
			last = state;
		}
		const corner = document.querySelector('.status-line')?.textContent?.trim();
		if (corner && /^(reading|loading)/i.test(corner) && !s.corner.includes(corner)) s.corner.push(corner);
		requestAnimationFrame(tick);
	};
	requestAnimationFrame(tick);
}

const read = (t) => t.page.evaluate(() => structuredClone(window.__splash));
/** The states in order, with the document's own parsing frames left out. */
const seq = (r) => r.states.filter((x) => x !== 'parsing');

async function stubHome(t) {
	await t.app.stub('/api/accounts/dashboard', homeAccounts());
	await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
}

/** A context with the session cookie of a real sign-in (the setup runs in the first). */
async function signedInContext(t, opts) {
	await t.newContext({ deviceScaleFactor: 1, serviceWorkers: 'block', ...opts });
	await t.page.addInitScript(sampler);
	await t.app.signIn();
}

/** The installed app's display mode. Chrome's own emulation (Emulation.setEmulatedMedia display-mode) is accepted but
 * not applied on HeadlessChrome 157 (browser-check gap c4d3c4), so the page is told what an installed app reads:
 * `(display-mode: standalone)` matches, `navigator.standalone` (iOS) is true. */
async function standalone(t) {
	await t.page.addInitScript(() => {
		const real = window.matchMedia.bind(window);
		window.matchMedia = (q) => {
			const m = /\(\s*display-mode\s*:\s*([a-z-]+)\s*\)/.exec(q);
			if (!m || q.trim() !== m[0]) return real(q);
			const list = new EventTarget();
			Object.defineProperties(list, { matches: { value: m[1] === 'standalone' }, media: { value: q } });
			return Object.assign(list, { onchange: null, addListener() {}, removeListener() {} });
		};
		Object.defineProperty(Navigator.prototype, 'standalone', { get: () => true, configurable: true });
	});
}

async function signedInFirstLoad(t, label, { phone = false, displayStandalone = false } = {}) {
	await signedInContext(t, phone
		? { viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true }
		: { viewport: { width: 1440, height: 900 } });
	if (displayStandalone) {
		await standalone(t);
	}
	await stubHome(t);
	await t.goto(`/app/home?e2e${phone ? '&safe=0,0,34,0' : ''}`, { settleMs: 0 });
	if (displayStandalone) await t.check(`${label}: the page is in standalone display mode`, await t.page.evaluate(() => matchMedia('(display-mode: standalone)').matches));
	await t.until(`${label}: Home drawn`, () => document.querySelectorAll('.home .hrow').length >= 8 && !document.querySelector('.boot-splash'), null, { timeout: 60000, hard: true });
	await t.page.waitForTimeout(500);
	const r = await read(t);
	const states = seq(r);
	await t.equal(`${label}: the sign-in form never appeared in any frame (of ${r.frames})`, states.filter((s) => s === 'form').length, 0);
	await t.equal(`${label}: the first painted state is the splash`, states.find((s) => s !== 'blank'), 'splash');
	await t.equal(`${label}: no blank frame between the document and the splash`, states.indexOf('blank') === -1 || states.indexOf('blank') > states.indexOf('splash') ? 'none' : states.join(' > '), 'none');
	await t.equal(`${label}: splash, then the app, once`, states.filter((s) => s !== 'blank').join(' > '), 'splash > app');
	await t.check(`${label}: the splash handed over to drawn rows`, (r.handoff?.rows ?? 0) > 0, JSON.stringify(r.handoff));
	await t.equal(`${label}: no "Reading" or "Loading" words in the corner`, r.corner.join(' | '), '');
}

/** Holds GET /api/auth/check until `release()`, so a screenshot can be taken of the splash. */
async function holdAuthCheck(t) {
	let release;
	const held = new Promise((r) => (release = r));
	await t.page.route((u) => new URL(u).pathname === '/api/auth/check', async (route) => {
		await held;
		await route.continue();
	});
	return () => release();
}

/** The answer a failed first read gets, and the words each page must then show in its banner. */
const FAILED = { status: 500, json: { error: 'Fixture: the first read failed.' } };

/** Opens a tab for the first time with its read held back, and checks its splash, then its content. With `fail`, the
 * held read answers 500 instead: the page must leave its splash for its banner with `fail`'s words. */
async function pageFirstLoad(t, { tab, api, ready, label, shot, fail = null }) {
	await signedInContext(t, { viewport: { width: 1440, height: 900 } });
	await stubHome(t);
	let release;
	const held = new Promise((r) => (release = r));
	await t.page.route((u) => new URL(u).pathname === api, async (route) => {
		await held;
		if (fail) await route.fulfill(FAILED);
		else if (api === '/api/accounts/analytics') await route.fulfill({ json: analyticsFixture({ range: new URL(route.request().url()).searchParams.get('range') || '7d' }) });
		else await route.continue();
	});
	await t.goto('/app/home?e2e', { settleMs: 0 });
	await t.until(`${label}: Home drawn first`, () => document.querySelectorAll('.home .hrow').length >= 8 && !document.querySelector('.boot-splash'), null, { timeout: 60000, hard: true });
	await t.page.click(`.chrome a[href="/app/${tab}"]`);
	await t.until(`${label}: the page's splash in its content area`, () => !!document.querySelector('.route:not([hidden]) .page-splash'), null, { timeout: 10000 });
	await t.visible('.route:not([hidden]) .page-splash .splash-bar', `${label}: the splash's bar is on screen`);
	const corner = await t.page.evaluate(() => document.querySelector('.status-line')?.textContent?.trim() ?? '');
	await t.check(`${label}: the corner has no loading words while the page loads`, !/^(reading|loading)/i.test(corner), `corner: "${corner}"`);
	if (shot) await t.shot(shot, `${label}: the page splash in the content area (desktop, light)`);
	release();
	if (fail) {
		await t.until(`${label}: the failure is said in the page's banner`, (words) => [...document.querySelectorAll('.route:not([hidden]) .banner')]
			.some((b) => !b.hidden && b.getClientRects().length > 0 && b.textContent.includes(words)), fail, { timeout: 30000 });
		await t.visible('.route:not([hidden]) .banner:not([hidden])', `${label}: the banner is on screen`);
	} else await t.until(`${label}: content drawn`, ready, null, { timeout: 30000 });
	await t.equal(`${label}: the splash is gone once ${fail ? 'the failure is shown' : 'content is drawn'}`, await t.page.evaluate(() => !!document.querySelector('.route:not([hidden]) .page-splash')), false);
	const r = await read(t);
	await t.equal(`${label}: no "Reading" or "Loading" words in the corner`, r.corner.join(' | '), '');
	// the answered 500 is the only console error a failed read may leave
	if (fail) await t.noConsoleErrors(`${label}: no console error but the answered 500`, { allow: [/status of 500/] });
}

export const tests = {
	async 'splash desktop signed in: no sign-in form before the app'(t) {
		await signedInFirstLoad(t, 'desktop');
	},

	async 'splash phone standalone signed in: no sign-in form before the app'(t) {
		await signedInFirstLoad(t, 'phone standalone', { phone: true, displayStandalone: true });
	},

	async 'splash signed out: the sign-in form follows the splash, no loop'(t) {
		// set up sign-in in one context, then open a fresh one with no session
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		await t.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1, serviceWorkers: 'block' });
		await t.page.addInitScript(sampler);
		await t.goto('/app/home?e2e', { settleMs: 0 });
		await t.until('the sign-in form is shown', () => !!document.querySelector('.signin-page input[name="username"]'), null, { timeout: 30000, hard: true });
		await t.page.waitForTimeout(3000);
		const states = seq(await read(t)).filter((s) => s !== 'blank');
		await t.equal('splash, then the form, and nothing after', states.join(' > '), 'splash > form');
		await t.equal('no splash left on the page', await t.page.evaluate(() => document.querySelectorAll('.boot-splash, .page-splash').length), 0);
		await t.visible('.signin-page button[type="submit"]', 'the sign-in button is on top');
	},

	async 'splash status first load: page splash, then content'(t) {
		await pageFirstLoad(t, { tab: 'status', api: '/api/status', label: 'status', shot: t.args.shots ? 'status-page-splash' : '',
			ready: () => !!document.querySelector('.route:not([hidden]) .status-grid') });
	},

	async 'splash analytics first load: page splash, then content'(t) {
		await pageFirstLoad(t, { tab: 'analytics', api: '/api/accounts/analytics', label: 'analytics', shot: t.args.shots ? 'analytics-page-splash' : '',
			ready: () => document.querySelectorAll('.ax-page .kpi').length >= 5 });
	},

	async 'splash scoreboard first load: page splash, then content'(t) {
		await pageFirstLoad(t, { tab: 'scoreboard', api: '/api/scoreboard', label: 'scoreboard',
			ready: () => !!document.querySelector('.route:not([hidden]) .sb-stack') });
	},

	// A failed first read: the page's splash gives way to its banner, never waits on (status/scoreboard: FetchError
	// sentence for the 500; Analytics: the server's own error words).
	async 'splash status failed first read: banner, no splash'(t) {
		await pageFirstLoad(t, { tab: 'status', api: '/api/status', label: 'status failed', fail: 'The server answered 500.' });
	},

	async 'splash analytics failed first read: banner, no splash'(t) {
		await pageFirstLoad(t, { tab: 'analytics', api: '/api/accounts/analytics', label: 'analytics failed', fail: FAILED.json.error });
	},

	async 'splash scoreboard failed first read: banner, no splash'(t) {
		await pageFirstLoad(t, { tab: 'scoreboard', api: '/api/scoreboard', label: 'scoreboard failed', fail: 'The server answered 500.' });
	},

	async 'splash screenshots: light and dark, desktop and phone'(t) {
		if (!t.args.shots) return t.check('screenshots only with --set shots=1', true);
		for (const scheme of ['light', 'dark']) {
			for (const phone of [false, true]) {
				const name = `splash-${phone ? 'phone' : 'desktop'}-${scheme}`;
				await signedInContext(t, { colorScheme: scheme, ...(phone ? { viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true } : { viewport: { width: 1440, height: 900 } }) });
				await stubHome(t);
				const release = await holdAuthCheck(t);
				await t.goto(`/app/home?e2e${phone ? '&safe=0,0,34,0' : ''}`, { settleMs: 0 });
				await t.until(`${name}: the frame started under the splash`, () => window.__aacFrame === true && !!document.querySelector('.boot-splash'), null, { timeout: 30000 });
				await t.page.waitForTimeout(400);
				await t.shot(name, `the loading splash, ${scheme}, ${phone ? 'phone 402 px' : 'desktop 1440 px'}: mark, name and bar centred, page colours of the theme`);
				release();
				await t.until(`${name}: Home drawn after`, () => document.querySelectorAll('.home .hrow').length >= 8 && !document.querySelector('.boot-splash'), null, { timeout: 30000 });
			}
		}
	},
};
