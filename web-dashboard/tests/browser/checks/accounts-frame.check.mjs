// Accounts & Settings as a native page in the app frame (PLAN step 6, leptos/src/accounts): the real test server and
// sign-in, with the account routes answered in the page (a fake server: made-up accounts, keys and codes, nothing
// real is touched). Each flow checks what the page draws by DOM selector and exactly which requests it sends: the
// same ones the Slint page sent through bridge.js and accounts-controller.mjs (now run by public/frame-dashboard.mjs).

import { fakeServer, serve, changes, dashboard } from '../accounts-fixture.mjs';

async function open(t, server, { width = 1440, height = 900, phone = false, duo = false, safe = '' } = {}) {
	await t.newContext(phone
		? { viewport: { width, height }, ...(duo ? { screen: { width: 466, height: 678 } } : {}), isMobile: true, hasTouch: true, serviceWorkers: 'block' }
		: { viewport: { width, height }, ...(duo ? { screen: { width: 466, height: 678 } } : {}), serviceWorkers: 'block' });
	await t.app.signIn();
	await serve(t, server);
	await t.goto(`/app/accounts?e2e${safe ? `&safe=${safe}` : ''}`, { settleMs: 300 });
	await t.until('the Accounts page is drawn from the dashboard', () => document.querySelectorAll('.prov[data-provider]').length >= 9, null, { timeout: 45000, hard: true });
}

const row = (id) => `.acc-item[data-row="${id}"]`;

export const tests = {
	// Astra max M1 (post-landing fix): the frame's dashboard controller stays in the window after sign-out, unlike the
	// old iframe, so a save queued behind a held reply must die with its session. Ordinary clicks; synthetic answers.
	async 'accounts: a save queued before sign-out is never sent with the next session'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		await serve(t, fakeServer());
		let signedIn = true;
		let epoch = 1;
		let release;
		const gate = new Promise((resolve) => { release = resolve; });
		const patches = [];
		const hidden = new Set();
		const json = (body, extra = {}) => ({ status: 200, contentType: 'application/json', body: JSON.stringify(body), ...extra });
		await t.page.route('**/api/auth/check', (r) => r.fulfill(json({ authenticated: signedIn, authRequired: true, configured: true, accessMode: 'login', username: 'reviewer', secureTransport: true, trustedLocalNetwork: true })));
		await t.page.route('**/api/auth/logout', (r) => { signedIn = false; return r.fulfill(json({ success: true })); });
		await t.page.route('**/api/auth/login', (r) => { signedIn = true; epoch = 2; return r.fulfill(json({ success: true, username: 'reviewer' }, { headers: { 'set-cookie': 'fixture_epoch=two; Path=/; SameSite=Lax' } })); });
		await t.page.route('**/api/accounts/dashboard?*', (r) => {
			const d = dashboard();
			d.settings.hiddenProviders = [...hidden];
			d.providers.forEach((p) => { p.visible = !hidden.has(p.id); });
			d.accounts.forEach((a) => { a.hidden = hidden.has(a.provider); });
			return r.fulfill(json(d));
		});
		await t.page.route('**/api/accounts/visibility', async (r) => {
			const body = r.request().postDataJSON();
			const cookie = await r.request().headerValue('cookie');
			patches.push({ epoch, body, nextSession: /fixture_epoch=two/.test(cookie || '') });
			for (const p of body.hiddenProviders?.hide || []) hidden.add(p);
			for (const p of body.hiddenProviders?.show || []) hidden.delete(p);
			const answer = { hiddenProviders: [...hidden], hiddenAccountIds: [], trayHiddenProviders: [], trayHiddenAccountIds: [] };
			if (patches.length === 1) await gate;
			await r.fulfill(json(answer)).catch(() => {});
		});
		try {
			await t.goto('/app/accounts?e2e', { settleMs: 300 });
			await t.page.waitForSelector('[data-probe="show:codex"]');
			await t.page.evaluate(() => { window.__oldController = window.aacFrameDashboard; });
			// Claude's Show in tray off (its reply is held), then Codex's Show on dashboard off: queued behind it
			await t.page.click('[data-probe="tray:claude"]');
			await t.page.click('[data-probe="show:codex"]');
			await t.equal('only the first save has reached the server', patches.length, 1);
			await t.page.click('.chrome .select-btn[aria-label="Account"]');
			await t.page.locator('.acct-menu .menu-item', { hasText: 'Sign out' }).click();
			await t.page.waitForSelector('.signin-card');
			await t.page.fill('input[name="username"]', 'reviewer');
			await t.page.fill('input[name="password"]', 'Fixture-password-2026!');
			await t.page.click('.signin-card button[type="submit"]');
			await t.page.waitForSelector('[data-probe="show:codex"]');
			await t.check('the old controller stopped; a new one owns the page', await t.page.evaluate(() => window.__oldController.state().stopped && window.__oldController.id !== window.aacFrameDashboard.id));
			// the new session hides Codex, then shows it again, before the old reply arrives
			await t.page.click('[data-probe="show:codex"]');
			await t.until('the new session hides Codex', () => document.querySelector('[data-probe="show:codex"]')?.getAttribute('aria-checked') === 'false', null, { timeout: 5000, hard: true });
			await t.page.click('[data-probe="show:codex"]');
			await t.until('and shows it again', () => document.querySelector('[data-probe="show:codex"]')?.getAttribute('aria-checked') === 'true', null, { timeout: 5000, hard: true });
			const before = patches.length;
			release();
			await t.page.waitForTimeout(400);
			await t.page.click('.chrome button[aria-label="Refresh"]');
			await t.page.waitForTimeout(500);
			const shown = await t.page.evaluate(() => document.querySelector('[data-probe="show:codex"]')?.getAttribute('aria-checked'));
			await t.equal('the old queue sends no save after the new session has saved', patches.length, before, { note: JSON.stringify(patches.map((p) => ({ epoch: p.epoch, nextSession: p.nextSession, body: p.body }))) });
			await t.equal('the latest explicit Show choice stands', shown, 'true');
		} finally {
			release();
		}
	},

	// Sol max L1 (post-landing fix): a layout callback the Accounts page queued for its next frame must do nothing when the
	// page has gone by then (a quick sign-out disposes it first). Before the fix it read the disposed NodeRef and the
	// runtime trapped ("already been disposed", then "RuntimeError: unreachable").
	async 'accounts: a sign-out just after Accounts lays out leaves no uncaught error'(t) {
		const server = fakeServer();
		await open(t, server);
		const errors = [];
		t.page.on('pageerror', (e) => errors.push(`pageerror: ${String(e?.message || e).slice(0, 200)}`));
		t.page.on('console', (m) => { if (m.type() === 'error' && /disposed|unreachable|panicked/i.test(m.text())) errors.push(`console: ${m.text().slice(0, 200)}`); });
		for (let round = 1; round <= 3; round++) {
			await t.page.click('.chrome .nav a[href="/app/home"]');
			await t.page.waitForTimeout(250);
			// back to Accounts, then signed out before its next frame: its layout callbacks are already queued. The next
			// frames come late here (as on a busy phone), so the page is gone when they run, every time.
			await t.page.evaluate(async () => {
				window.__kitFrame ||= window.requestAnimationFrame;
				const frame = window.__kitFrame.bind(window);
				window.requestAnimationFrame = (cb) => setTimeout(() => frame(cb), 400);
				document.querySelector('.chrome .nav a[href="/app/accounts"]').click();
				await new Promise((resolve) => setTimeout(resolve, 0));
				window.dispatchEvent(new CustomEvent('aac-dash', { detail: { key: 'signed-out', json: JSON.stringify({ state: 'default', reason: '' }) } }));
			});
			await t.page.waitForSelector('.signin-card', { timeout: 10000 });
			// the late frames run now, on the page that went
			await t.page.waitForTimeout(900);
			await t.page.evaluate(() => { window.requestAnimationFrame = window.__kitFrame; });
			// the server session is still valid: the frame's sign-in finds it again
			await t.page.evaluate(() => window.aacFrameSession.recheck());
			await t.until(`round ${round}: Accounts is drawn again`, () => document.querySelectorAll('.prov[data-provider]').length >= 9, null, { timeout: 30000, hard: true });
		}
		await t.page.waitForTimeout(500);
		await t.equal('no uncaught error from a callback of the page that went', errors, []);
	},

	async 'accounts: every card is drawn natively, no Slint dashboard is loaded'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.check('nine provider cards and the settings cards', await t.page.evaluate(() =>
			document.querySelectorAll('.prov').length === 9 && !!document.querySelector('[data-probe="signin-card"]') && !!document.querySelector('[data-probe="update-card"]')));
		await t.check('no dashboard iframe, no bridge.js, no dashboard wasm', await t.page.evaluate(() => !document.querySelector('iframe.slint-frame') && !performance.getEntriesByType('resource').some((e) => /\/bridge\.js|ccs_account_dashboard/.test(e.name))));
		await t.check('Update apps spans the page above the columns', await t.page.evaluate(() => document.querySelector('.ac-page .ac-card')?.dataset.probe === 'update-card' && !!document.querySelector('.ac-top [data-probe="update-card"]')));
		await t.check('three columns at 1440', await t.page.evaluate(() => document.querySelectorAll('.ac-col').length === 3));
		await t.text(`${row('codex:one')} .acc-email`, 'codex.active@example.invalid');
		await t.visible(`${row('codex:one')} .active-mark`, 'the active Codex account says Active');
		await t.visible(`${row('codex:two')} [data-probe="activate:codex:two"]`, 'the other one has Activate');
		await t.audit('accounts 1440');
	},

	async 'accounts: Codex device-code sign-in, from the name to Done'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click('[data-probe="add:codex"]');
		await t.page.waitForSelector('.prov[data-provider="codex"] .flow .field-input');
		await t.page.fill('.prov[data-provider="codex"] .flow .field-input', 'codex-3');
		await t.page.click('.prov[data-provider="codex"] .flow [data-probe="flow-submit:codex"]');
		await t.until('the code and the verification page show', () => document.querySelector('.prov[data-provider="codex"] .cb-text')?.textContent === 'ABCD-12345', null, { timeout: 15000 });
		await t.text('.prov[data-provider="codex"] .cb-url', 'https://auth.openai.com/codex/device');
		await t.visible('.prov[data-provider="codex"] [data-probe="flow-open-url:codex"]', 'Open verification page');
		await t.visible('.prov[data-provider="codex"] [data-probe="flow-copy:codex"]', 'Copy code');
		// both need the tap's own user activation, so the frame does them (the dashboard's hidden page could not)
		await t.page.context().route((u) => String(u).startsWith('https://auth.openai.com/'), (r) => r.fulfill({ status: 200, contentType: 'text/html', body: '<title>verification</title>' }));
		const popup = t.page.waitForEvent('popup', { timeout: 10000 });
		await t.page.click('.prov[data-provider="codex"] [data-probe="flow-open-url:codex"]');
		const opened = await popup;
		await t.equal('Open verification page opens it from the tap', new URL(opened.url()).origin, 'https://auth.openai.com');
		await opened.close();
		await t.page.click('.prov[data-provider="codex"] [data-probe="flow-copy:codex"]');
		await t.until('Copy code says what happened', () => [...document.querySelectorAll('.toast')].some((el) => /Code copied|Copy the code by hand/.test(el.textContent)), null, { timeout: 5000 });
		await t.until('the sign-in finishes', () => /is signed in as new\.codex@example\.invalid/.test(document.querySelector('.prov[data-provider="codex"] .flow-done')?.textContent || ''), null, { timeout: 15000 });
		const adds = changes(server).filter((r) => r.path === '/api/accounts/add');
		await t.equal('one add request with the typed profile name', JSON.stringify(adds.map((r) => r.body)), JSON.stringify([{ provider: 'codex', profileName: 'codex-3' }]));
		await t.page.click('.prov[data-provider="codex"] [data-probe="flow-done:codex"]');
		await t.until('Done closes the panel', () => !document.querySelector('.prov[data-provider="codex"] .flow'));
	},

	async 'accounts: an API key is sent once and leaves the field at once'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click('[data-probe="add-key:kimi-code"]');
		const key = '.prov[data-provider="kimi-code"] .flow input[type="password"]';
		await t.page.waitForSelector(key);
		await t.page.fill(key, 'sk-test-not-a-real-key-7c2e');
		await t.page.fill('.prov[data-provider="kimi-code"] .flow .label-field input', 'Work');
		await t.page.click('.prov[data-provider="kimi-code"] [data-probe="flow-submit:kimi-code"]');
		await t.equal('the field is empty right after the submit', await t.page.evaluate((sel) => document.querySelector(sel)?.value ?? '', key), '');
		await t.until('the key is stored', () => /Key stored, ending in 7c2e/.test(document.querySelector('.prov[data-provider="kimi-code"] .flow-done')?.textContent || ''), null, { timeout: 15000 });
		const adds = changes(server).filter((r) => r.path === '/api/accounts/add');
		await t.equal('one add request with the key and its label', JSON.stringify(adds.map((r) => r.body)), JSON.stringify([{ provider: 'kimi-code', key: 'sk-test-not-a-real-key-7c2e', label: 'Work' }]));
		await t.check('the key is nowhere on the page', await t.page.evaluate(() => !document.body.innerHTML.includes('sk-test-not-a-real-key')));
	},

	async 'accounts: a pasted code goes once and the field clears; a running job opens again after a reload'(t) {
		const job = { id: 'job_bbbbbbbbbbbbbbbb', provider: 'antigravity', kind: 'supervised-cli', mode: 'add', state: 'awaiting_code', profileName: 'agy-2', verification: { url: 'https://accounts.google.com/o/oauth2/auth', userCode: null, expiresAt: null } };
		const server = fakeServer({ runningJobs: [job] });
		server.s.jobs.set(job.id, [job]);
		await open(t, server);
		const field = '.prov[data-provider="antigravity"] .flow .field-input';
		await t.until('the running job opened its flow on load', () => !!document.querySelector('.prov[data-provider="antigravity"] .flow .field-input'), null, { timeout: 15000 });
		await t.page.fill(field, '4/0AbCdEf-test-code');
		await t.page.click('.prov[data-provider="antigravity"] [data-probe="flow-submit:antigravity"]');
		await t.equal('the code field is empty right after the submit', await t.page.evaluate((sel) => document.querySelector(sel)?.value ?? '', field), '');
		await t.until('it waits for the dashboard to save it', () => /Saving the sign-in/.test(document.querySelector('.prov[data-provider="antigravity"] .flow-wait')?.textContent || ''), null, { timeout: 15000 });
		const codes = changes(server).filter((r) => r.path.endsWith('/code'));
		await t.equal('one code request', JSON.stringify(codes.map((r) => r.body)), JSON.stringify([{ code: '4/0AbCdEf-test-code' }]));
	},

	async 'accounts: remove asks first, then sends its one-use token once'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click(`${row('codex:two')} [data-probe="remove:codex:two"]`);
		await t.until('the confirmation line names the effect', () => /Its saved Codex login is deleted\./.test(document.querySelector('.acc-item[data-row="codex:two"] .row-line.confirm')?.textContent || ''), null, { timeout: 10000 });
		await t.check('nothing is removed yet', !server.s.removed.has('codex:two'));
		await t.page.click(`${row('codex:two')} [data-probe="remove-commit:codex:two"]`);
		await t.until('the row goes', () => !document.querySelector('.acc-item[data-row="codex:two"]'), null, { timeout: 15000 });
		const removes = changes(server).filter((r) => r.path === '/api/accounts/codex%3Atwo/remove');
		await t.equal('an ask, then one commit with the token', JSON.stringify(removes.map((r) => r.body)), JSON.stringify([{}, { confirmationToken: 'tok-remove-codex:two' }]));
	},

	async 'accounts: restore from the trash asks first, then sends its token once'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click('[data-probe="restore:tr_0123456789abcdef"]');
		await t.until('the restore line names the effect', () => /moves back on Mac and Windows/.test(document.querySelector('.trash .row-line.confirm')?.textContent || ''), null, { timeout: 10000 });
		await t.page.click('[data-probe="restore-commit:tr_0123456789abcdef"]');
		await t.until('the trash row goes', () => !document.querySelector('.trash-head'), null, { timeout: 15000 });
		const restores = changes(server).filter((r) => r.path.endsWith('/restore'));
		await t.equal('an ask, then one commit with the token', JSON.stringify(restores.map((r) => r.body)), JSON.stringify([{}, { confirmationToken: 'tok-restore' }]));
	},

	async 'accounts: a refused control says why under its row and sends nothing'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click(`${row('codex:one')} [data-probe="signin-again:codex:one"]`);
		await t.until('the refusal line', () => /is the active account, so it cannot sign in again/.test(document.querySelector('.acc-item[data-row="codex:one"] .row-line.refuse')?.textContent || ''), null, { timeout: 10000 });
		await t.page.click(`${row('codex:one')} [data-probe="remove:codex:one"]`);
		await t.until('the remove refusal', () => /cannot be removed\. Activate another account first/.test(document.querySelector('.acc-item[data-row="codex:one"] .row-line.refuse')?.textContent || ''), null, { timeout: 10000 });
		await t.equal('no change was sent', changes(server).length, 0);
	},

	async 'accounts: a pending switch holds changes, shows the frame\'s approval, and cancels'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click(`${row('codex:two')} [data-probe="activate:codex:two"]`);
		await t.until('the frame shows its switch approval (activation.rs)', () => !!document.querySelector('.adialog') && !document.querySelector('iframe.slint-frame'), null, { timeout: 15000 });
		const before = changes(server).length;
		const toasts = await t.page.evaluate(() => document.querySelectorAll('.toast').length);
		// a change from the page while the switch waits: the controller holds it with a toast
		await t.page.evaluate(() => window.aacFrameDashboard.act('remove', 'codex:two'));
		await t.until('a toast says to finish the switch first', () => [...document.querySelectorAll('.toast')].some((el) => /Finish the account switch first/.test(el.textContent)), null, { timeout: 10000 });
		await t.equal('the remove was not sent', changes(server).length, before);
		await t.check('one toast more', (await t.page.evaluate(() => document.querySelectorAll('.toast').length)) > toasts);
		await t.page.locator('.adialog .a-btns .btn', { hasText: 'Cancel' }).click();
		await t.until('Cancel closes the approval', () => !document.querySelector('.adialog'), null, { timeout: 10000 });
		const activations = changes(server).filter((r) => r.path.startsWith('/api/codex/profiles/'));
		await t.equal('one activation ask, no confirmation sent', JSON.stringify(activations.map((r) => r.body)), JSON.stringify([{}]));
		await t.noConsoleErrors('console: only the switch that asked for a confirmation', { allow: [/status of 409/] });
	},

	async 'accounts: Details opens from a row, swaps, and closes'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click(`${row('codex:two')} .acc-email`);
		await t.until('Details shows the account', () => document.querySelector('.details .dt-title')?.textContent === 'codex.second@example.invalid', null, { timeout: 10000 });
		await t.check('its row is selected', await t.page.evaluate(() => document.querySelector('.acc-item[data-row="codex:two"]').classList.contains('sel')));
		await t.check('its windows are listed', await t.page.evaluate(() => document.querySelectorAll('.details .dm').length >= 2));
		await t.page.click(`${row('claude:one')} .acc-email`);
		await t.until('a row click swaps it', () => document.querySelector('.details .dt-title')?.textContent === 'one.claude@example.invalid', null, { timeout: 10000 });
		await t.visible('.details .dt-acts button:has-text("Open on Mac")', 'Claude Details has Open on Mac');
		await t.page.keyboard.press('Escape');
		await t.until('Escape closes it', () => !document.querySelector('.details'), null, { timeout: 5000 });
		await t.page.click(`${row('claude:one')} .acc-email`);
		await t.page.waitForSelector('.details');
		await t.page.click('[data-probe="details-close"]');
		await t.until('the close button closes it', () => !document.querySelector('.details'), null, { timeout: 5000 });
	},

	async 'accounts: on a phone the cards stack, Update apps folds to its line, nothing runs sideways'(t) {
		const server = fakeServer();
		await open(t, server, { width: 390, height: 844, phone: true });
		await t.check('one column', await t.page.evaluate(() => document.querySelectorAll('.ac-col').length === 1));
		await t.check('Update apps first and folded', await t.page.evaluate(() => {
			const card = document.querySelector('.ac-col .ac-card');
			return card?.dataset.probe === 'update-card' && card.querySelector('.upd-head')?.getAttribute('aria-expanded') === 'false';
		}));
		await t.check('no sideways page scroll', await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
		await t.audit('accounts phone');
		await t.page.click(`${row('codex:two')} .acc-email`);
		await t.until('Details is a bottom sheet', () => {
			const d = document.querySelector('.details');
			return !!d && Math.abs(d.getBoundingClientRect().bottom - innerHeight) < 2 && d.getBoundingClientRect().left === 0;
		}, null, { timeout: 10000 });
	},

	async 'accounts: Details reaches the right edge beside a left rail'(t) {
		const server = fakeServer();
		await open(t, server, { width: 678, height: 466, phone: true, duo: true, safe: '0,0,34,84' });
		await t.page.click(`${row('codex:two')} .acc-email`);
		await t.until('Details shows the account', () => document.querySelector('.details .dt-title')?.textContent === 'codex.second@example.invalid', null, { timeout: 10000 });
		const rect = await t.page.evaluate(() => {
			const box = (el) => { const r = el.getBoundingClientRect(); return { x: r.x, r: r.right, y: r.y, w: r.width }; };
			return { details: box(document.querySelector('.details')), rail: box(document.querySelector('.rail')), w: innerWidth };
		});
		await t.check('Details starts at the rail and reaches the right edge, with no rail-width gap', rect.details.x >= rect.rail.r - 0.5 && Math.abs(rect.details.r - rect.w) < 1, JSON.stringify(rect));
		await t.page.keyboard.press('Escape');
	},
};
