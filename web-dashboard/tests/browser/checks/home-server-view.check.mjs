// The native Home route and the browser's view-model.mjs render the same inputs.
// Use `--set rustHome=1` for the worker-free route and the separate fixture renderer.
//   - switch: off, the dashboard offers nothing and the route is the unknown-endpoint 404; on, it is offered and the
//     real front door answers it with a view the real Rust service built.
//   - real wiring (on): a reload of a page that was offered the view reads only /api/views/home; the view is drawn,
//     and this browser's own view-model.mjs builds the same JSON from the same answers and context.
//   - fixture (on): every kind of row and card (home-fixture.mjs), built by the kit's Rust service from the stubbed
//     data; the same JSON as this browser's JS view, the same Home DOM and pixels as the JS draw, desktop and phone.
// Made-up data only; the page clock is fixed so both draws read the same "ago" words.
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';

const NOW = Math.floor(Date.now() / 60_000) * 60_000;

/** The config derives rustHome from rustHomeFed; the checks read the same implication, not the raw arg. */
const rustHomeOn = (t) => [t.args.rustHome, t.args.rustHomeFed, t.args.cut].some((value) => !!value && value !== '0');
const fixtureBuilderOn = (t) => [t.args.rustHome, t.args.rustHomeFed].some((value) => !!value && value !== '0');

/** The next answer of GET /api/views/home, with the context header the page sent. */
function nextHomeView(t) {
	return t.page.waitForResponse((r) => new URL(r.url()).pathname === '/api/views/home', { timeout: 60000 }).then(async (r) => ({
		status: r.status(),
		etag: r.headers().etag ?? null,
		body: await r.json().catch(() => null),
		context: (() => { try { return JSON.parse(decodeURIComponent(r.request().headers()['x-aac-home-context'] || '')); } catch { return null; } })(),
	}));
}

/** This browser's view-model.mjs on the same inputs and context (null when it throws). */
function jsView(t, reply, context) {
	return t.page.evaluate(async ({ reply, context }) => {
		const vm = await import('/view-model.mjs');
		const tf = await import('/time-format.mjs');
		const zone = tf.getDisplayTimeZone();
		try {
			tf.setDisplayTimeZone(context.timeZone);
			return JSON.stringify(vm.dashboardViewModel(reply.dashboard, {
				now: context.now, profiles: reply.profiles, platform: context.platform, antigravityInventory: reply.antigravity,
				intervalSeconds: context.intervalSeconds, username: context.username, host: context.host,
			}));
		} catch { return null; } finally { tf.setDisplayTimeZone(zone); }
	}, { reply, context });
}

/** The drawn Home, for a draw-to-draw comparison. */
const homeDom = (t) => t.page.evaluate(() => document.querySelector('.route:not([hidden]) .home')?.outerHTML ?? '');

async function homeIn(t, { width, height, mobile, offer }) {
	await t.newContext({ viewport: { width, height }, isMobile: mobile, hasTouch: mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
	await t.page.clock.setFixedTime(new Date(NOW));
	await t.app.signIn();
	const accounts = homeAccounts(NOW - 90_000);
	if (offer) {
		// the kit's stub offers the view and builds it with the kit's Rust service (`--set rustHome=1`)
		await t.app.stub('/api/accounts/dashboard', accounts);
		await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
	} else {
		// the same answers, never offering it: the page's own view
		await t.page.route((u) => new URL(u).pathname === '/api/accounts/dashboard', (r) => r.fulfill({ json: accounts }));
		await t.page.route((u) => new URL(u).pathname === '/api/claude/desktop-profiles', (r) => r.fulfill({ json: homeProfiles() }));
		await t.page.route((u) => new URL(u).pathname === '/api/views/home', (r) => r.fulfill({ status: 404, json: { error: 'API endpoint was not found.' } }));
	}
	const reply = offer ? nextHomeView(t) : null;
	await t.goto('/app/home?e2e', { settleMs: 300 });
	await t.until('Home is drawn', () => document.querySelectorAll('.home .hrow').length >= 8 && document.querySelectorAll('.hcard').length >= 6, null, { timeout: 60000, hard: true });
	await t.page.waitForTimeout(400);
	return reply ? await reply : null;
}

export const tests = {
	async 'home server view: switch'(t) {
		await t.app.open({ page: 'home' });
		// asked from here with the page's session, not from the page: a 404 there is a console error by design
		const cookie = (await t.page.context().cookies(t.run.baseURL)).map((c) => `${c.name}=${c.value}`).join('; ');
		const get = (p, headers = {}) => fetch(new URL(p, t.run.baseURL), { headers: { cookie, ...headers } });
		const dash = await (await get('/api/accounts/dashboard')).json();
		const home = await get('/api/views/home', { 'X-AAC-Home-Context': 'x' });
		const answer = { views: dash.server?.views ?? null, status: home.status, body: await home.json().catch(() => null) };
		if (rustHomeOn(t)) {
			await t.check('on: the dashboard offers Home', JSON.stringify(answer.views) === '["home"]', JSON.stringify(answer.views));
			// an unreadable context still answers the data, with no view: the page builds its own
			await t.check('on: the route answers the data and view null for an unreadable context', answer.status === 200 && answer.body?.schemaVersion === 1 && answer.body.view === null, `${answer.status} ${JSON.stringify(answer.body)?.slice(0, 160)}`);
		} else {
			await t.check('off: nothing is offered', answer.views === null, JSON.stringify(answer.views));
			await t.check('off: the route is the unknown-endpoint 404', answer.status === 404 && answer.body?.error === 'API endpoint was not found.', `${answer.status} ${JSON.stringify(answer.body)}`);
		}
	},

	async 'home server view: real wiring'(t) {
		if (!rustHomeOn(t)) { await t.check('switch off: nothing to wire (run with --set rustHome=1 or rustHomeFed=1)', true); return; }
		await t.app.open({ page: 'home' });
		// the first load learned the offer; a reload's first read asks the real front door, which asks the real Rust service
		await t.until('the offer is remembered', () => localStorage.getItem('aac.homeServerView') === '1', null, { timeout: 30000, hard: true });
		// the first read has answered (its sections are drawn), so the reload cuts no request off
		await t.until('Home is drawn from its first read', () => !!document.querySelector('.route:not([hidden]) .home .hsec'), null, { timeout: 30000, hard: true });
		const reply = nextHomeView(t);
		await t.page.reload();
		const r = await reply;
		await t.check('the real route answers 200 with a Rust view', r.status === 200 && r.body?.view?.version === 3, `${r.status} view=${JSON.stringify(r.body?.view)?.slice(0, 120)}`, { hard: true });
		await t.check('the native view has a SHA-256 ETag', /^W\/"[a-f0-9]{64}"$/.test(r.etag ?? ''), String(r.etag));
		const js = await jsView(t, r.body, r.context);
		await t.check('this browser\'s view-model.mjs builds the same JSON', js === JSON.stringify(r.body.view), firstDifference(js, JSON.stringify(r.body.view)));
		await t.until('Home is drawn', () => !!document.querySelector('.route:not([hidden]) .home'), null, { timeout: 30000, hard: true });
		await t.audit('home server view (real wiring)');
	},

	async 'home server view: fixture, desktop and phone'(t) {
		if (!fixtureBuilderOn(t)) { await t.check('the fixture builder runs with --set rustHome=1', true); return; }
		for (const [label, size] of [['desktop 1440', { width: 1440, height: 900, mobile: false }], ['phone 390', { width: 390, height: 844, mobile: true }]]) {
			const r = await homeIn(t, { ...size, offer: true });
			await t.check(`${label}: the kit's Rust service built the view`, r?.status === 200 && r.body?.view?.version === 3, JSON.stringify(t.app.rustHome()), { hard: true });
			const js = await jsView(t, r.body, r.context);
			await t.check(`${label}: this browser's view-model.mjs builds the same JSON`, js === JSON.stringify(r.body.view), firstDifference(js, JSON.stringify(r.body.view)));
			const gravity = r.body.view.sections.find((section) => section.id === 'antigravity');
			await t.equal(`${label}: Home renders only the T3 quota pools`, gravity.columns.map((column) => column.key), ['cli-gemini-weekly', 'cli-3p-weekly']);
			const rustDom = await homeDom(t);
			const rustPixels = await t.page.screenshot({ fullPage: true });
			await t.shot(`rust-${label.replace(' ', '-')}`, `Home drawn from the Rust view (${label}, fixture data)`);
			await homeIn(t, { ...size, offer: false });
			const jsDom = await homeDom(t);
			const jsPixels = await t.page.screenshot({ fullPage: true });
			await t.shot(`js-${label.replace(' ', '-')}`, `Home drawn from the page's own view (${label}, fixture data)`);
			await t.check(`${label}: the same Home DOM as the page's own draw`, rustDom.length > 1000 && rustDom === jsDom, firstDifference(jsDom, rustDom));
			await t.check(`${label}: the same pixels as the page's own draw`, rustPixels.equals(jsPixels), `${rustPixels.length} vs ${jsPixels.length} bytes`);
			await t.audit(`home server view (fixture ${label})`);
		}
		const stats = t.app.rustHome();
		await t.check('the kit\'s Rust service refused nothing it was given', stats.refused === 0 && stats.failed === 0 && stats.noContext === 0, JSON.stringify(stats));
	},
};

function firstDifference(a, b) {
	if (a === b) return 'equal';
	if (typeof a !== 'string' || typeof b !== 'string') return `${typeof a} vs ${typeof b}`;
	let i = 0;
	while (i < a.length && a[i] === b[i]) i++;
	return `at ${i}: js ${JSON.stringify(a.slice(Math.max(0, i - 60), i + 80))} / rust ${JSON.stringify(b.slice(Math.max(0, i - 60), i + 80))}`;
}
