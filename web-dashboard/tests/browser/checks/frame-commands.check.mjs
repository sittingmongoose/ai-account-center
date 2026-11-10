// The app frame and the dashboard in its iframe (fix round 1 of the frame review, 54113ba2): the frame's commands
// reach the dashboard however early they are given, Sign out ends the session even while the dashboard is still
// loading, and a session the server ended brings the frame's sign-in with the right reason, whichever page noticed.
// Real test server and sign-in (the kit's temporary home); the update endpoint is answered in the page, so no app
// update ever runs.

const MENU = '.chrome .select-btn[aria-label="Account"]';

async function open(t, tab) {
	await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
	await t.app.signIn();
	await t.goto(`/app/${tab}?e2e`, { settleMs: 300 });
	await t.until(`${tab}: the frame is drawn, signed in`, () => !!document.querySelector('.shell') && !document.querySelector('.signin-card'), null, { timeout: 30000, hard: true });
}

/** Whether the server still knows this browser's session. */
const serverSignedIn = (t) => t.page.evaluate(async () => (await (await fetch('/api/auth/check', { credentials: 'same-origin' })).json()).authenticated === true);

/** Polls a condition read in the test (not the page) for up to `ms`. */
async function eventually(t, read, ok, ms = 8000) {
	let value;
	for (let waited = 0; waited <= ms; waited += 250) {
		value = await read();
		if (ok(value)) return value;
		await t.page.waitForTimeout(250);
	}
	return value;
}

const dashboardSignedIn = () => {
	try { return document.querySelector('iframe.slint-frame')?.contentWindow?.__aacLastAuth?.signedIn === true; } catch { return false; }
};

/** The server ends the session behind the frame's back (the same call Sign out makes, from the test). */
async function endOnServer(t) {
	const status = await t.page.evaluate(async () => (await fetch('/api/auth/logout', { method: 'POST', credentials: 'same-origin', headers: { 'content-type': 'application/json' }, body: '{}' })).status);
	return t.check('the server ended the session', status === 200 && !(await serverSignedIn(t)), `logout answered ${status}`);
}

const frameState = (t) => t.page.evaluate(() => ({
	signedIn: window.aacFrameSession?.view?.().signedIn ?? null,
	form: !!document.querySelector('.signin-card'),
	banner: document.querySelector('.signin-banner')?.innerText.replace(/\s+/g, ' ').trim() || '',
}));

export const tests = {
	async 'frame: a Scoreboard read the server refuses brings the sign-in'(t) {
		await open(t, 'scoreboard');
		// Its first read is done (the header shows its time): a Refresh during a read is not a second read.
		await t.until('the Scoreboard has read once', () => !!document.querySelector('.chrome .status-line b')?.textContent, null, { timeout: 30000, hard: true });
		await endOnServer(t);
		const refused = t.page.waitForResponse((r) => new URL(r.url()).pathname === '/api/scoreboard' && r.status() === 401, { timeout: 15000 });
		await t.page.click('.chrome button[aria-label="Refresh"]');
		await refused;
		const state = await eventually(t, () => frameState(t), (s) => s.signedIn === false && s.form, 5000);
		await t.check('the frame signs out and shows its sign-in, with the reason', state.signedIn === false && state.form && /^Your session ended|^Signed out after/.test(state.banner), JSON.stringify(state));
		await t.noConsoleErrors('console: only the refused read', { allow: [/status of 401/] });
	},

	async 'frame: Sign out while the dashboard is still loading ends the session'(t) {
		await open(t, 'status');
		await t.page.waitForSelector('.status-grid, .q-card');
		// Hold the dashboard's script: its page exists but cannot answer yet.
		let release;
		const gate = new Promise((resolve) => { release = resolve; });
		await t.page.route((u) => new URL(u).pathname === '/bridge.js', async (route) => { await gate; await route.continue(); });
		await t.page.click('.chrome .nav a[href="/app/home"]');
		await t.page.waitForSelector('iframe.slint-frame');
		await t.page.click(MENU);
		await t.page.locator('.acct-menu .menu-item', { hasText: 'Sign out' }).click();
		const ended = await eventually(t, () => serverSignedIn(t), (signedIn) => signedIn === false);
		const state = await eventually(t, () => frameState(t), (s) => s.form, 3000);
		release();
		await t.check('Sign out ends the server session while the dashboard loads', ended === false, `server signed in: ${ended}`);
		await t.check('and the frame shows its sign-in, saying so', state.form && state.signedIn === false, JSON.stringify(state));
		await t.page.waitForTimeout(1500);
		await t.check('the session stays ended once the dashboard script arrives', !(await serverSignedIn(t)));
	},

	async 'frame: the first Update apps click reaches the dashboard'(t) {
		await open(t, 'status');
		let starts = 0;
		await t.page.route((u) => new URL(u).pathname === '/api/app-updates/start', async (route) => {
			starts++;
			await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ job: { state: 'done', results: [] } }) });
		});
		// The dashboard is not created yet: this click creates it and must still count.
		await t.page.click('.chrome .upd-btn');
		await t.until('the dashboard signed in', dashboardSignedIn, null, { timeout: 60000, hard: true });
		const first = await eventually(t, async () => starts, (n) => n >= 1, 5000);
		await t.page.waitForTimeout(800);
		await t.check('the first click starts one update job', first === 1 && starts === 1, `starts after the first click: ${starts}`);
		await t.equal('and the frame is on Accounts & Settings', await t.page.evaluate(() => location.pathname), '/app/accounts');
		await t.page.click('.chrome .upd-btn');
		const second = await eventually(t, async () => starts, (n) => n >= 2, 5000);
		await t.page.waitForTimeout(800);
		await t.check('a second click starts one more, never a repeat', second === 2 && starts === 2, `starts after the second click: ${starts}`);
	},

	async 'frame: when the dashboard finds the session ended, the frame says why'(t) {
		await open(t, 'home');
		await t.until('the dashboard signed in', dashboardSignedIn, null, { timeout: 60000, hard: true });
		// Its first reading is in (the header shows its time): that reading records when the session was last seen,
		// and must not land after the time planted below.
		await t.until('the dashboard has read once', () => !!document.querySelector('.chrome .status-line b')?.textContent, null, { timeout: 30000, hard: true });
		// The session was last used longer ago than any session lasts; the dashboard is the page that notices.
		await t.page.evaluate(() => localStorage.setItem('aac-session-seen-at', String(Date.now() - 400 * 24 * 3600 * 1000)));
		await endOnServer(t);
		await t.page.click('.chrome button[aria-label="Refresh"]');
		const state = await eventually(t, () => frameState(t), (s) => s.form && !!s.banner, 10000);
		await t.check('the frame says how long sessions last, as the dashboard found', state.form && /^Signed out after /.test(state.banner), JSON.stringify(state));
		await t.noConsoleErrors('console: only the refused reads', { allow: [/status of 401/] });
	},
};
