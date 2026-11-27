// Static logins (Central Logins step 2) under the Logins list on Accounts & Settings. The real routes: the view answers
// with no secret, and a write refuses a rotating sign-in (or the plain-HTTP address) without echoing what was posted.
// The panel, on the device fixture (web-dashboard/leptos/fixtures/login-delivery-view.json, invented names only):
// every delivered login drawn on a desktop and a phone, the form's secret is a password field that is cleared the
// moment it is sent, the confirmation names the fingerprint, and no page text ever holds the secret.
// LOOK: the three screenshots.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const FIXTURE = JSON.parse(fs.readFileSync(path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos/fixtures/login-delivery-view.json'), 'utf8'));
const ROWS = '.route:not([hidden]) .lg-plate .ld-row';
const SECRET = 'sk-ant-oat01-TESTONLY-kit-token-000000000000';
const LEAK = /sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{8}|access_?token|refresh_?token|cookie|Bearer |\/home\/|\/Users\/|\.claude\/|\.codex\/|auth\.json/i;

const noSideways = async (t, name) => t.check(name, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));

export const tests = {
	async 'login delivery: the real routes never echo a secret'(t) {
		await t.app.open({ page: 'accounts' });
		const answer = await t.page.evaluate(async () => {
			const r = await fetch('/api/accounts/logins/delivery', { credentials: 'same-origin', cache: 'no-store' });
			return { status: r.status, store: r.headers.get('cache-control'), text: await r.text() };
		});
		// A test server whose private-file writes are not the Rust service's says so honestly (503).
		await t.check('GET answers the view or an honest 503', [200, 503].includes(answer.status), answer.status);
		await t.check('no-store', /no-store/.test(answer.store || ''), answer.store);
		await t.check('no credential-shaped text in the view', !LEAK.test(answer.text), answer.text.slice(0, 200));
		const post = (body, type = 'application/json') => t.page.evaluate(async ([body, type]) => {
			const r = await fetch('/api/accounts/logins/delivery/put', { method: 'POST', credentials: 'same-origin', headers: { 'content-type': type }, body });
			return { status: r.status, text: await r.text() };
		}, [body, type]);
		const rotating = await post(JSON.stringify({ kind: 'codex-signin', provider: 'claude', account: 'claude:x', secret: SECRET, expiresOn: null, targets: [{ computer: 'ubuntu', expect: 'absent' }], confirm: 'deliver' }));
		await t.check('a rotating sign-in is refused (403 on plain HTTP, else 422 by type)', [403, 422].includes(rotating.status), rotating);
		await t.check('the refusal does not echo the secret', !rotating.text.includes('TESTONLY'), rotating.text);
		const plain = await post('x', 'text/plain');
		await t.check('a write needs JSON', plain.status === 415, plain.status);
		await t.noConsoleErrors('console: only the two refused writes', { allow: [/status of (403|415|422) .*delivery\/put/] });
	},

	async 'login delivery: panel on a desktop and a phone, the secret sent once and gone'(t) {
		await t.newContext({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1, serviceWorkers: 'block' });
		await t.app.stub('/api/accounts/logins/delivery', { ...FIXTURE, generatedAt: new Date().toISOString() });
		await t.app.stub('/api/accounts/registry', { providers: [], accounts: [], jobs: [], trash: [] });
		await t.app.noResets();
		const sent = [];
		await t.page.route((u) => new URL(u).pathname === '/api/accounts/logins/delivery/put', async (route) => {
			sent.push(route.request().postData() || '');
			await route.fulfill({ json: { loginId: 'aa00bb11', fingerprint: 'sha256:fedcba9876543210', results: [{ computer: 'mac', state: 'delivered', text: 'Delivered' }] } });
		});
		await t.app.open({ page: 'accounts' });
		await t.until('every delivered login is drawn', (n) => document.querySelectorAll(n).length > 0, ROWS, { timeout: 30000 });
		await t.check('one row per delivered login', await t.page.locator(ROWS).count() === FIXTURE.logins.length);
		await t.check('a fingerprint is shown, never a secret', /sha256:0011223344556677/.test(await t.page.locator(ROWS).first().innerText()));
		await t.check('a computer not read at the last check says so', /Nas1 not checked/.test(await t.page.locator(ROWS).first().innerText()));
		await t.check('a computer holding another value names it', /Mac other value 8899aabb/.test(await t.page.locator(ROWS).nth(1).innerText()));
		await t.check('a login whose computers disagree says so and offers no copy', /Computers disagree/.test(await t.page.locator(ROWS).nth(1).innerText()) && await t.page.locator(ROWS).nth(1).locator('[data-probe="ld-copy"]').count() === 0);
		await t.check('an unreadable file is named', /Ubuntu unreadable file/.test(await t.page.locator(ROWS).nth(2).innerText()));
		await t.check('another computer answering is said, with Trust it', await t.page.locator('[data-probe="ld-other:windows"] [data-probe="ld-trust"]').count() === 1);
		await t.check('a computer answering with another computer\'s key is said, with nothing to press', /Odroid: answers as ubuntu-test, already pinned to another computer/.test(await t.page.locator('[data-probe="ld-other:odroid"]').innerText()) && await t.page.locator('[data-probe="ld-other:odroid"] button').count() === 0);
		await t.check('a long where line wraps instead of being cut', await t.page.locator(`${ROWS} .ld-where`).nth(2).evaluate((el) => el.scrollWidth <= el.clientWidth + 1 && getComputedStyle(el).whiteSpace === 'normal'));
		await t.page.locator('.ld').scrollIntoViewIfNeeded();
		await t.crop('delivery-desktop', '.ld', 'Static logins at 1920: three rows (account, kind, fingerprint, end, where, actions), the expiring one in amber, Copy to the rest only where a computer lacks it');
		await noSideways(t, 'no sideways scroll at 1920');
		// Replace on a login with an unchecked holder: that computer cannot be ticked, and says why; hosts are shown.
		await t.page.locator(ROWS).first().locator('[data-probe="ld-replace"]').click();
		await t.check('an unchecked holder cannot be ticked in a replace', await t.page.locator('[data-probe="ld-comp:nas1"]').isDisabled());
		await t.check('the reason is shown', /Nas1.*check computers first/.test(await t.page.locator('.ld-comps').innerText()));
		await t.check('enrolled computers show their host name', /Ubuntu · ubuntu-test/.test(await t.page.locator('.ld-comps').innerText()));
		await t.crop('delivery-replace-unchecked', '.ld', 'Replace form: Nas1 cannot be ticked (not read at the last check: check computers first); Ubuntu and Mac ticked with host names; Windows refused (another computer answered)');
		// Switching to Deliver while Replace is open draws a fresh form: Kind and Account can be chosen again.
		await t.page.locator('[data-probe="ld-new"]').click();
		await t.check('switching forms rebuilds the form', !(await t.page.locator('[data-probe="ld-kind"]').isDisabled()) && !(await t.page.locator('[data-probe="ld-comp:nas1"]').isDisabled()));
		// Check computers closes an open form (nothing secret is kept), so a stale form is never sent.
		await t.page.route((u) => new URL(u).pathname === '/api/accounts/logins/delivery/check', (r) => r.fulfill({ json: { ...FIXTURE, generatedAt: new Date().toISOString() } }));
		await t.page.locator('[data-probe="ld-check"]').click();
		await t.until('Check computers closes the open form', () => !document.querySelector('.ld-form'));
		// Deliver a new token to Mac.
		await t.page.locator('[data-probe="ld-new"]').click();
		await t.check('the secret field is a password field', await t.page.locator('[data-probe="ld-secret"]').getAttribute('type') === 'password');
		await t.page.locator('[data-probe="ld-account"]').selectOption('claude:sam');
		await t.page.locator('[data-probe="ld-secret"]').fill(SECRET);
		await t.check('a computer another one answered for cannot be ticked', await t.page.locator('[data-probe="ld-comp:windows"]').isDisabled());
		await t.page.locator('[data-probe="ld-comp:mac"]').check();
		await t.page.locator('[data-probe="ld-review"]').click();
		await t.until('the confirmation is shown', () => !!document.querySelector('[data-probe="ld-confirm"]'));
		await t.check('the confirmation does not show the secret', !(await t.page.locator('[data-probe="ld-confirm"]').innerText()).includes('TESTONLY'));
		await t.crop('delivery-confirm', '.ld', 'The confirmation step: one sentence (write this Claude token to 1 computer; sent once, never shown again), Back and Deliver');
		await t.page.locator('[data-probe="ld-confirm-go"]').click();
		await t.until('the receipt is shown', () => !!document.querySelector('[data-probe="ld-outcome"]'));
		const body = JSON.parse(sent[0] || '{}');
		await t.check('one request carried the secret, with an explicit confirm and what the page showed for Mac', sent.length === 1 && body.secret === SECRET && body.confirm === 'deliver' && body.targets?.length === 1 && body.targets[0].computer === 'mac' && body.targets[0].expect === 'absent', body.confirm);
		await t.check('no page text or field holds the secret after sending', await t.page.evaluate((s) => {
			const values = [...document.querySelectorAll('input, textarea')].map((el) => el.value);
			return !document.documentElement.outerHTML.includes(s) && !values.some((v) => v.includes(s));
		}, SECRET));
		await t.viewport(390, 844);
		await t.page.waitForTimeout(600);
		await noSideways(t, 'no sideways scroll at 390');
		await t.page.locator('.ld').scrollIntoViewIfNeeded();
		await t.crop('delivery-phone', '.ld', 'Static logins at 390: each login in short lines (account | end; kind, fingerprint; where; actions), small text, no orphaned word');
		await t.audit('accounts with static logins at 390');
	},
};
