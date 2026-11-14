// The app frame's sign-in (web-dashboard/leptos/src/session.rs, public/frame-session.mjs) on a real test server with
// a temporary home and test passwords only: first-run setup, sign-out, a wrong password, an ended and an expired
// session, a session another browser revoked, and the limiter with its countdown. The steps run in order on one
// server; the limiter step comes last, because it closes this server's sign-in for 15 minutes.
//   browser-check run web-dashboard/tests/browser/frame-signin
const USER = 'labadmin';
const PASSWORD = 'Lab-Correct-Horse-Battery-9';

/** A frame address; `?e2e` gives the kit's hooks (the dashboard controller's state is read from `aacFrameDashboard`). */
async function page(t, path = '/app/home') {
	await t.goto(`${path}?e2e`, { settleMs: 300 });
}

/** The sign-in page is shown and settled: its title, banner and message words. */
async function signInPage(t, label, want = () => true) {
	await t.until(`${label}: the sign-in page`, () => !!document.querySelector('.signin-card') && !document.querySelector('.shell'), null, { timeout: 30000, hard: true });
	let words = null;
	for (let i = 0; i < 40; i++) {
		words = await t.page.evaluate(() => ({
			title: document.querySelector('.signin-title')?.textContent || '',
			banner: document.querySelector('.signin-banner')?.innerText.replace(/\s+/g, ' ').trim() || '',
			message: document.querySelector('.signin-message')?.innerText.replace(/\s+/g, ' ').trim() || '',
			button: document.querySelector('.signin-card button[type="submit"]')?.textContent || '',
			disabled: !!document.querySelector('.signin-card button[type="submit"]')?.disabled,
			confirm: !!document.querySelector('input[name="password-confirm"]'),
			user: document.querySelector('input[name="username"]')?.value ?? null,
		}));
		if (want(words)) break;
		await t.page.waitForTimeout(250);
	}
	return words;
}

/** The frame is signed in, and a dashboard on screen has finished its own start. A step that ends the session
 * right after must not race the dashboard's first read of it: that read would find the session gone, forget the
 * time it was last seen, and the next page could no longer say why (fix round 1 triage). */
async function framed(t, label) {
	await t.until(`${label}: the frame is drawn`, () => !!document.querySelector('.shell') && !document.querySelector('.signin-card'), null, { timeout: 30000, hard: true });
	await t.until(`${label}: the dashboard on screen has started`, () => {
		const route = [...document.querySelectorAll('.route')].find((r) => !r.hidden);
		// Home and Accounts take the dashboard controller's views (public/frame-dashboard.mjs): it has read once
		if (!route || !route.querySelector('.page.home, .ac-page')) return true;
		const s = window.aacFrameDashboard?.state?.();
		return !!s && s.authenticated === true && s.hasData === true;
	}, null, { timeout: 60000, hard: true });
	return true;
}

async function fill(t, { user = USER, password = '', confirm } = {}) {
	await t.page.fill('input[name="username"]', user);
	await t.page.fill('input[name="password"]', password);
	if (confirm !== undefined) await t.page.fill('input[name="password-confirm"]', confirm);
	await t.page.click('.signin-card button[type="submit"]');
}

async function serverSays(t) {
	return t.page.evaluate(async () => (await (await fetch('/api/auth/check', { credentials: 'same-origin' })).json()));
}

/** Leaves the frame for a same-origin document with no scripts, so no page that reads the session is still open
 * when the next step ends it. An open page that found the session gone would forget when it was last seen, and
 * the page loaded next could no longer say why (fix round 1 triage: the reviewer's empty banners). */
async function leaveTheFrame(t) {
	await t.goto('/manifest.webmanifest', { settleMs: 100 });
}

/** The server forgets this browser's session behind the frame's back (as a restart, a password change or the
 * session's end would): the same sign-out call, made by the test, so the frame keeps what it knew. */
async function endOnServer(t) {
	const r = await t.page.evaluate(async () => (await fetch('/api/auth/logout', { method: 'POST', credentials: 'same-origin', headers: { 'content-type': 'application/json' }, body: '{}' })).status);
	return t.check('the server ended the session', r === 200 && (await serverSays(t)).authenticated !== true, `logout answered ${r}`);
}

async function signOutFromMenu(t) {
	await t.page.click('.chrome .select-btn[aria-label="Account"]');
	await t.page.locator('.acct-menu .menu-item', { hasText: 'Sign out' }).click();
}

/** Another browser signs in and signs every other browser out (Settings, Sign out other browsers): the same call. */
async function revokeFromAnotherBrowser(t) {
	const base = new URL(t.run.baseURL);
	const headers = { 'content-type': 'application/json', origin: base.origin };
	const login = await fetch(new URL('/api/auth/login', base), { method: 'POST', headers, body: JSON.stringify({ username: USER, password: PASSWORD, rememberMe: false }) });
	const cookie = (login.headers.getSetCookie?.() || [login.headers.get('set-cookie') || '']).map((c) => c.split(';')[0]).filter(Boolean).join('; ');
	const revoke = await fetch(new URL('/api/auth/sessions/revoke-others', base), { method: 'POST', headers: { ...headers, cookie }, body: '{}' });
	return { login: login.status, revoke: revoke.status, body: (await revoke.text()).slice(0, 120) };
}

export const tests = {
	async 'frame sign-in: every state on a test server'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });

		await t.step('first run: the setup form', async () => {
			await page(t);
			const w = await signInPage(t, 'setup', (x) => x.confirm);
			await t.check('a fresh server asks to set up sign-in', w.title === 'Set up sign-in' && w.confirm && w.button === 'Create sign-in', JSON.stringify(w));
			await t.page.fill('input[name="username"]', USER);
			await t.page.fill('input[name="password"]', 'short');
			const typed = await t.page.evaluate(() => document.querySelector('.strength')?.innerText || '');
			await t.check('the strength line follows the typing', /Too short/i.test(typed), typed);
			await t.equal('typing a password keeps the username', await t.page.inputValue('input[name="username"]'), USER);
			await fill(t, { password: PASSWORD, confirm: `${PASSWORD}x` });
			const bad = await signInPage(t, 'setup mismatch', (x) => !!x.message);
			await t.check('a confirmation that differs is named', /match/i.test(bad.message), bad.message);
			await fill(t, { password: PASSWORD, confirm: PASSWORD });
			await framed(t, 'setup done');
			await t.check('the server has a signed-in session', (await serverSays(t)).authenticated === true);
		});

		await t.step('sign out (with the dashboard controller started)', async () => {
			await t.until('Home loaded', () => window.aacFrameDashboard?.state?.().hasData === true, null, { timeout: 60000 });
			await signOutFromMenu(t);
			const w = await signInPage(t, 'signed out', (x) => !!x.message);
			await t.check('the signed-out page says so', w.title === 'Sign in' && /Signed out\./.test(w.message), JSON.stringify(w));
			await t.check('the server ended the session', (await serverSays(t)).authenticated !== true);
		});

		await t.step('a wrong password', async () => {
			await fill(t, { password: 'not-the-password' });
			const w = await signInPage(t, 'wrong', (x) => /isn't right/.test(x.message));
			await t.check("a wrong password says so, with the tries left", /Username or password isn't right\./.test(w.message) && /tries? left/.test(w.message), w.message);
			await t.equal('the typed username stays', w.user, USER);
		});

		await t.step('the right password, with the keyboard', async () => {
			// The phone keyboard's Next on Username moves to Password (iOS would submit the half-filled form), and
			// Go on Password signs in.
			await t.page.fill('input[name="username"]', USER);
			await t.page.fill('input[name="password"]', '');
			await t.page.focus('input[name="username"]');
			await t.page.keyboard.press('Enter');
			const after = await t.page.evaluate(() => ({ focused: document.activeElement?.getAttribute('name'), message: document.querySelector('.signin-message')?.innerText || '' }));
			await t.check('Next on Username moves to Password without signing in', after.focused === 'password' && !/Enter your username/.test(after.message), JSON.stringify(after));
			await t.page.keyboard.type(PASSWORD);
			await t.page.keyboard.press('Enter');
			await framed(t, 'signed in');
		});

		await t.step('sign out from Status (the dashboard never loaded)', async () => {
			await page(t, '/app/status');
			await framed(t, 'status');
			await t.check('no dashboard controller running yet', await t.page.evaluate(() => { const s = window.aacFrameDashboard?.state?.(); return !s || !s.started || s.stopped; }));
			await signOutFromMenu(t);
			const w = await signInPage(t, 'signed out from Status', (x) => !!x.message);
			await t.check('signed out with the frame\'s own call', /Signed out\./.test(w.message) && (await serverSays(t)).authenticated !== true, JSON.stringify(w));
			await fill(t, { password: PASSWORD });
			await framed(t, 'signed in again');
		});

		await t.step('a session that ended early', async () => {
			await leaveTheFrame(t);
			await endOnServer(t);
			await page(t);
			const w = await signInPage(t, 'ended', (x) => !!x.banner);
			await t.check('the page says the session ended', /^Your session ended/.test(w.banner), w.banner);
			await fill(t, { password: PASSWORD });
			await framed(t, 'signed in after ended');
		});

		await t.step('a session that ran out', async () => {
			await leaveTheFrame(t);
			await endOnServer(t);
			// The server last accepted this session longer ago than any session lasts.
			await t.page.evaluate(() => localStorage.setItem('aac-session-seen-at', String(Date.now() - 400 * 24 * 3600 * 1000)));
			await page(t);
			const w = await signInPage(t, 'expired', (x) => !!x.banner);
			await t.check('the page says how long sessions last', /^Signed out after /.test(w.banner), w.banner);
			await fill(t, { password: PASSWORD });
			await framed(t, 'signed in after expired');
		});

		await t.step('a session ended while Status is open', async () => {
			await page(t, '/app/status');
			await framed(t, 'status');
			await endOnServer(t);
			await t.page.locator('.chrome button[aria-label="Refresh"]').click();
			const w = await signInPage(t, 'ended under Status', (x) => !!x.banner);
			await t.check('the next reading brings the sign-in page with the reason', /^Your session ended|^Signed out after /.test(w.banner), w.banner);
			await fill(t, { password: PASSWORD });
			await framed(t, 'signed in after Status');
		});

		await t.step('revoked from another browser', async () => {
			const r = await revokeFromAnotherBrowser(t);
			await t.check('another browser signed the others out', r.login === 200 && r.revoke === 200, JSON.stringify(r));
			await page(t);
			const w = await signInPage(t, 'revoked', (x) => !!x.banner);
			await t.check('the page says another browser signed this one out', /^Signed out from another browser/.test(w.banner), w.banner);
		});

		await t.step('the sign-in page offers no classic app', async () => {
			await t.equal('no link to the retired classic app', await t.page.locator('.signin-page a', { hasText: /classic/i }).count(), 0);
		});

		await t.step('too many tries: the limiter and its countdown', async () => {
			let w = null;
			for (let i = 0; i < 8; i++) {
				await signInPage(t, `try ${i + 1}`, (x) => !x.disabled);
				if ((await t.page.evaluate(() => document.querySelector('.signin-card button[type="submit"]')?.textContent || '')).startsWith('Try again')) break;
				await fill(t, { password: `wrong-${i}` });
				w = await signInPage(t, `after try ${i + 1}`, (x) => !/Signing in/.test(x.button));
				if (/^Try again/.test(w.button)) break;
			}
			await t.check('sign-in pauses, with the time it opens again', /^Try again in \d+:\d\d$/.test(w.button) && w.disabled && /^Try again in/.test(w.banner) && /opens again at/.test(w.banner), JSON.stringify(w));
			const first = w.button;
			await t.page.waitForTimeout(2200);
			const later = await t.page.evaluate(() => document.querySelector('.signin-card button[type="submit"]')?.textContent || '');
			await t.check('the countdown runs', later !== first && /^Try again in \d+:\d\d$/.test(later), `${first} then ${later}`);
			await t.check('the fields wait too', await t.page.isDisabled('input[name="password"]'));
		});
	},
};
