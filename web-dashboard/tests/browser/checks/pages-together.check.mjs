// Home (PLAN step 5) and Accounts & Settings (step 6) together in the frame: a Home row opens the frame's Details
// overlay (details.rs), never the dashboard's Slint one, and neither page offers a switch (account switching was
// removed, 2026-11-14). Real test server and sign-in; the readings and the account routes are answered in the page
// (made-up data, home-fixture.mjs and accounts-fixture.mjs).
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';
import { fakeServer, serve, changes } from '../accounts-fixture.mjs';

/** No Slint dashboard is loaded or drawn: no iframe, no bridge.js, no dashboard wasm (G5). */
const slintHidden = () =>
	!document.querySelector('iframe.slint-frame') && !performance.getEntriesByType('resource').some((e) => /\/bridge\.js|ccs_account_dashboard/.test(e.name));

export const tests = {
	async 'pages: a Home row opens Details, which carries no live-login mark and offers no switch'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		await t.app.stub('/api/accounts/dashboard', homeAccounts());
		await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
		await t.goto('/app/home', { settleMs: 300 });
		await t.until('Home is drawn natively', () => document.querySelectorAll('.home .hrow').length >= 8, null, { timeout: 60000, hard: true });
		await t.page.click('.hrow[data-row="codex:second"] .hemail');
		await t.until('the frame\'s Details opens for that row', () => document.querySelector('.details .dt-title')?.textContent === 'codex.second@example.invalid', null, { timeout: 10000 });
		await t.check('no Slint dashboard is loaded', await t.page.evaluate(slintHidden));
		await t.check('Details offers no Activate and no switch question', await t.page.evaluate(() =>
			!document.querySelector('.details [data-probe^="details-activate:"], .details .confirm-line, .adialog')));
		await t.page.click('.hrow[data-row="claude:pro-a"] .hemail');
		await t.until('another row swaps it in place', () => (document.querySelector('.details .dt-title')?.textContent || '').length > 0 && document.querySelector('.details .dt-title')?.textContent !== 'codex.second@example.invalid', null, { timeout: 10000 });
		await t.page.click('.hrow[data-row="codex:primary"] .hemail');
		await t.until('the live login\'s Details opens', () => document.querySelector('.details .dt-title')?.textContent === 'codex.primary.workspace@example.invalid', null, { timeout: 10000 });
		await t.check('the live login\'s Details carries no Active mark', await t.page.evaluate(() =>
			!document.querySelector('.details .active-mark') && !/Active on Ubuntu/.test(document.querySelector('.details .dt-sub')?.textContent || '')));
		await t.noConsoleErrors('console: clean');
	},

	async 'pages: Accounts is drawn natively with no Activate, and nothing is sent on entry'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		const server = fakeServer();
		await serve(t, server);
		await t.goto('/app/accounts', { settleMs: 300 });
		await t.until('Accounts is drawn natively', () => document.querySelectorAll('.prov[data-provider]').length >= 9, null, { timeout: 60000, hard: true });
		await t.check('no Slint dashboard is raised over the tab', await t.page.evaluate(slintHidden));
		await t.check('no account offers Activate', await t.page.evaluate(() => !document.querySelector('[data-probe^="activate:"]')));
		await t.equal('no change was sent', changes(server).length, 0);
		await t.noConsoleErrors('console: clean');
	},
};
