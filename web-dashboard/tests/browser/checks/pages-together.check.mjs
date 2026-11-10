// Home (PLAN step 5) and Accounts & Settings (step 6) together in the frame: a Home row opens the frame's Details
// overlay (details.rs), and a switch asked for from Details or Accounts shows the frame's own approval dialog
// (activation.rs), never the dashboard's Slint one. Real test server and sign-in; the readings, the account routes and
// every switch are answered in the page (made-up data, home-fixture.mjs and accounts-fixture.mjs), so nothing real is
// switched.
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';
import { fakeServer, serve, changes } from '../accounts-fixture.mjs';

/** The 43-character one-use token shape activation-confirmation.mjs accepts (made up). */
const TOKEN = 'Abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE';
const approval = (target) => ({
	status: 409,
	json: { error: 'Processes are running.', code: 'confirmation_required', confirmation: {
		token: TOKEN, targetProfile: target, expiresAt: new Date(Date.now() + 5 * 60_000).toISOString(),
		processes: [{ label: 'codex (made-up terminal)', pid: 4242, role: 'cli' }], warning: 'Active Codex work will stop.' } },
});

/** Records the requests a route gets, answering each with `answerWith(body, count)` ({ status, json }). */
async function answer(t, path, answerWith) {
	const calls = [];
	await t.page.route((u) => new URL(u).pathname === path, async (route) => {
		const request = route.request();
		let body = null;
		try { body = request.postDataJSON(); } catch {}
		calls.push({ method: request.method(), body });
		const { status = 200, json = {} } = answerWith(body, calls.length);
		await route.fulfill({ status, json });
	});
	return calls;
}

/** The dashboard iframe is loaded, hidden, and not raised over the page. */
const slintHidden = () => {
	const host = document.querySelector('.slint-host');
	return !!document.querySelector('iframe.slint-frame') && !!host && host.hidden && !host.classList.contains('raised');
};

export const tests = {
	async 'pages: a Home row opens Details; Activate anyway there shows the native approval'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		await t.app.stub('/api/accounts/dashboard', homeAccounts());
		await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
		const activations = await answer(t, '/api/codex/profiles/second/activate', (body, n) => n === 1 ? approval('second') : { status: 200, json: { name: 'second', email: 'codex.second@example.invalid' } });
		await t.goto('/app/home', { settleMs: 300 });
		await t.until('Home is drawn natively', () => document.querySelectorAll('.home .hrow').length >= 8, null, { timeout: 60000, hard: true });
		await t.page.click('.hrow[data-row="codex:second"] .hemail');
		await t.until('the frame\'s Details opens for that row', () => document.querySelector('.details .dt-title')?.textContent === 'codex.second@example.invalid', null, { timeout: 10000 });
		await t.check('the Slint dashboard stays hidden', await t.page.evaluate(slintHidden));
		await t.page.click('.hrow[data-row="claude:pro-a"] .hemail');
		await t.until('another row swaps it in place', () => (document.querySelector('.details .dt-title')?.textContent || '').length > 0 && document.querySelector('.details .dt-title')?.textContent !== 'codex.second@example.invalid', null, { timeout: 10000 });
		await t.page.click('.hrow[data-row="codex:second"] .hemail');
		await t.until('and back', () => document.querySelector('.details .dt-title')?.textContent === 'codex.second@example.invalid', null, { timeout: 10000 });
		await t.page.click('.details [data-probe="details-activate:codex:second"]');
		await t.until('Details asks first (the account is past its switch point)', () => /Activate anyway/.test(document.querySelector('.details .confirm-line')?.textContent || ''), null, { timeout: 5000 });
		await t.check('nothing is sent before the answer', activations.length === 0);
		await t.page.click('.details .confirm-line .btn.accent-line');
		await t.until('the frame\'s approval dialog opens', () => !!document.querySelector('.adialog'), null, { timeout: 10000 });
		await t.check('Details made way for it, and Slint draws nothing', await t.page.evaluate(() => !document.querySelector('.details')) && await t.page.evaluate(slintHidden));
		await t.text('.adialog .a-target', /Switch to second\?/);
		await t.check('one activation ask, with no token', activations.length === 1 && !activations[0].body?.confirmationToken, JSON.stringify(activations));
		await t.page.locator('.adialog .a-btns .btn', { hasText: 'Cancel' }).click();
		await t.until('Cancel closes it', () => !document.querySelector('.adialog'), null, { timeout: 8000 });
		await t.check('and sends no token', activations.length === 1);
		await t.noConsoleErrors('console: only the 409 that asks for the approval', { allow: [/status of 409/] });
	},

	async 'pages: Activate on Accounts shows the native approval, not the dashboard over the tab'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		const server = fakeServer();
		await serve(t, server);
		await t.goto('/app/accounts', { settleMs: 300 });
		await t.until('Accounts is drawn natively', () => document.querySelectorAll('.prov[data-provider]').length >= 9, null, { timeout: 60000, hard: true });
		await t.page.click('.acc-item[data-row="codex:two"] [data-probe="activate:codex:two"]');
		await t.until('the frame\'s approval dialog opens', () => !!document.querySelector('.adialog'), null, { timeout: 10000 });
		await t.check('the dashboard is not raised over the tab', await t.page.evaluate(slintHidden));
		await t.text('.adialog .a-target', /Switch to two\?/);
		await t.page.locator('.adialog .a-btns .btn', { hasText: 'Cancel' }).click();
		await t.until('Cancel closes it', () => !document.querySelector('.adialog'), null, { timeout: 8000 });
		const sent = changes(server).filter((r) => r.path.startsWith('/api/codex/profiles/'));
		await t.equal('one activation ask, no confirmation', JSON.stringify(sent.map((r) => r.body)), JSON.stringify([{}]));
		await t.noConsoleErrors('console: only the 409 that asks for the approval', { allow: [/status of 409/] });
	},
};
