// Home drawn natively in the app frame (PLAN step 5, G5): the frame's dashboard controller (public/frame-dashboard.mjs)
// reads the dashboard and builds view model v2 with view-model.mjs, as bridge.js did; the page draws it and sends the
// dashboard's own action kinds back (dash.rs). No Slint dashboard is loaded. Real test server and sign-in (the kit's temporary home); the dashboard's readings and every
// switch, auto-switch and Open request are answered in the page (made-up data, home-fixture.mjs), so nothing real
// is switched or opened.
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';

const ROW = (id) => `.hrow[data-row="${id}"]`;

/** The 43-character one-use token shape activation-confirmation.mjs accepts (made up). */
const TOKEN = 'Abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE';

async function openHome(t, { width = 1440, height = 900, mobile = false, safe = '' } = {}) {
	await t.newContext({ viewport: { width, height }, isMobile: mobile, hasTouch: mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
	await t.app.signIn();
	await t.app.stub('/api/accounts/dashboard', homeAccounts());
	await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
	await t.goto(`/app/home${safe ? `?safe=${safe}` : ''}`, { settleMs: 300 });
	await t.until('Home is drawn from the dashboard\'s view', () => document.querySelectorAll('.home .hrow').length >= 8 && document.querySelectorAll('.hcard').length >= 6,
		null, { timeout: 60000, hard: true });
}

/** Records the requests a route gets, answering each with `answer(body, count)` ({ status, json }). */
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

async function eventually(t, read, ok, ms = 8000) {
	let value;
	for (let waited = 0; waited <= ms; waited += 200) {
		value = await read();
		if (ok(value)) return value;
		await t.page.waitForTimeout(200);
	}
	return value;
}

const layout = () => {
	const sections = [...document.querySelectorAll('.hsec')].map((s) => {
		const table = s.querySelector('.htable');
		const head = s.querySelector('.hhead');
		const firstRow = s.querySelector('.hrow');
		return {
			id: s.dataset.sec,
			left: Math.round(s.getBoundingClientRect().left),
			width: Math.round(s.getBoundingClientRect().width),
			table: !!table && getComputedStyle(firstRow).gridTemplateColumns.split(' ').length > 2,
			headShown: !!head && getComputedStyle(head).display !== 'none',
		};
	});
	const cards = [...document.querySelectorAll('.hcard')].map((c) => {
		const r = c.getBoundingClientRect();
		return { top: Math.round(r.top), left: Math.round(r.left), right: Math.round(r.right) };
	});
	const grid = document.querySelector('.hcards').getBoundingClientRect();
	const rows = new Map();
	for (const c of cards) rows.set(c.top, [...(rows.get(c.top) || []), c]);
	// every row of cards spans the grid's whole width (no hole at a row's end)
	const holes = [...rows.values()].filter((row) => Math.abs(Math.max(...row.map((c) => c.right)) - grid.right) > 2 || Math.abs(Math.min(...row.map((c) => c.left)) - grid.left) > 2).length;
	const overflow = [...document.querySelectorAll('.home *')].filter((el) => {
		const r = el.getBoundingClientRect();
		return r.width > 0 && (r.right > innerWidth + 1 || r.left < -1);
	}).map((el) => el.className).slice(0, 5);
	return { sections, cardRows: [...rows.values()].map((r) => r.length), holes, sideways: document.documentElement.scrollWidth > innerWidth + 1, overflow };
};

export const tests = {
	async 'home: draws the dashboard view natively, with no Slint on screen'(t) {
		await openHome(t);
		await t.count('.hsec', 3);
		await t.text('.hsec[data-sec="claude"] .plate-meta', /4\s*accounts/);
		await t.text('.hsec[data-sec="codex"] .plate-meta', /codex\.primary\.workspace/);
		await t.check('Home loads no Slint dashboard: no iframe, no bridge.js, no dashboard wasm', async () => t.page.evaluate(() => !document.querySelector('iframe.slint-frame') && !performance.getEntriesByType('resource').some((e) => /\/bridge\.js|ccs_account_dashboard/.test(e.name))));
		const meters = await t.page.evaluate(() => [...document.querySelectorAll(`.hrow .um`)].map((m) => ({ sev: m.dataset.sev, num: m.querySelector('.um-num')?.textContent || '', na: m.querySelector('.um-na')?.textContent || '' })));
		await t.check('every reading shows its number, settled (no count-up left over)', meters.filter((m) => m.sev !== 'na').every((m) => /^\d+(\.\d+)?%$/.test(m.num)), JSON.stringify(meters.slice(0, 6)));
		await t.check('a window with no reading says so, never 0%', meters.some((m) => m.na === 'Not reported yet'));
		await t.check('severity follows the reading: warn at 93 and 94, crit at 97, over past 100',
			await t.page.evaluate(() => {
				const sev = (row, i) => document.querySelectorAll(`.hrow[data-row="${row}"] .hcell`)[i]?.querySelector('.um')?.dataset.sev;
				return sev('claude:pro-a', 1) === 'warn' && sev('claude:max-c', 0) === 'warn' && sev('codex:second', 1) === 'crit' && sev('claude:max-b', 0) === 'over';
			}));
		await t.check('past 100% draws the overage beyond the track', await t.page.evaluate(() => {
			const o = document.querySelector('.hrow[data-row="claude:max-b"] .um-bar.over .um-ovr');
			return !!o && o.getBoundingClientRect().width > 6;
		}));
		await t.check('the Codex meters carry the 95% auto-switch notch', await t.page.evaluate(() => {
			const n = [...document.querySelectorAll('.hsec[data-sec="codex"] .um-notch')];
			return n.length >= 5 && n.every((x) => parseFloat(x.style.left) === 95);
		}));
		await t.check('a Pro account leaves its Fable cell empty', await t.page.evaluate(() => document.querySelectorAll('.hrow[data-row="claude:pro-a"] .hcell')[2]?.classList.contains('empty')));
		await t.check('the active Codex row is highlighted and says where it is active', await t.page.evaluate(() => {
			const row = document.querySelector('.hrow[data-row="codex:primary"]');
			const glide = document.querySelector('.hsec[data-sec="codex"] .hglide');
			const r = row.getBoundingClientRect(), g = glide.getBoundingClientRect();
			return row.querySelector('.hmark.on')?.textContent.includes('Active') && getComputedStyle(glide).opacity === '1' && Math.abs(g.top - r.top - 3) < 2 && Math.abs(g.height - (r.height - 6)) < 2;
		}));
		await t.text('.hrow[data-row="codex:primary"] .hamts', /62\.5K\s*credits/);
		await t.text('.hsec[data-sec="codex"] .hfoot', /below the switch point/);
		await t.text('.hsec[data-sec="antigravity"] .hauto-off', /needs a second account/);
		await t.count('.hcard', 6);
		await t.text('.hcard', /\$12\.40/);
		await t.audit('home 1440');

		// A refresh with a new reading moves the meter but rebuilds nothing else: the Active mark is the same element (no
		// replayed draw-on), and so is the auto-switch control.
		await t.page.evaluate(() => {
			document.querySelector('.hrow[data-row="codex:primary"] .hmark').__kit = 1;
			document.querySelector('.hsec[data-sec="codex"] .hauto').__kit = 1;
		});
		const next = homeAccounts();
		next.accounts.find((a) => a.id === 'codex:primary').windows[0].usedPercent = 61;
		await t.app.stub('/api/accounts/dashboard', next);
		await t.page.click('.chrome button[aria-label="Refresh"]');
		await t.until('the new reading lands', () => /^61%$/.test(document.querySelector('.hrow[data-row="codex:primary"] .um-num')?.textContent || ''), null, { timeout: 15000 });
		await t.check('the Active mark and the auto-switch control are kept through a refresh', await t.page.evaluate(() =>
			document.querySelector('.hrow[data-row="codex:primary"] .hmark').__kit === 1 && document.querySelector('.hsec[data-sec="codex"] .hauto').__kit === 1));
		await t.noConsoleErrors('console: clean');
	},

	async 'home: Activate past the switch point asks first, then the target-bound approval runs as before'(t) {
		await openHome(t);
		const activations = await answer(t, '/api/codex/profiles/second/activate', (body, n) => n === 1
			? { status: 409, json: { error: 'Processes are running.', code: 'confirmation_required', confirmation: {
				token: TOKEN, targetProfile: 'second', expiresAt: new Date(Date.now() + 5 * 60_000).toISOString(),
				processes: [{ label: 'codex (made-up terminal)', pid: 4242, role: 'cli' }], warning: 'Active Codex work will stop.' } } }
			: { status: 200, json: { name: 'second', email: 'codex.second@example.invalid' } });
		await t.page.click(`${ROW('codex:second')} .hact`);
		await t.until('the inline question opens under the row', () => !!document.querySelector('.hrow[data-row="codex:second"] .hconfirm'), null, { timeout: 5000 });
		await t.text(`${ROW('codex:second')} .hconfirm p`, /97% used.*above the 95% switch point/);
		await t.check('nothing is sent before the answer', activations.length === 0);
		await t.page.click(`${ROW('codex:second')} .hconfirm .btn.accent-line`);
		await eventually(t, async () => activations.length, (n) => n >= 1);
		await t.check('Activate anyway runs the existing guarded activation, with no token', activations.length === 1 && activations[0].method === 'POST' && !activations[0].body?.confirmationToken, JSON.stringify(activations));
		await t.until('the switch approval opens over the page', () => !!document.querySelector('.adialog'), null, { timeout: 8000 });
		await t.text('.adialog', /Another Codex process is running/);
		await t.text('.adialog .a-target', /Switch to second\?/);
		await t.text('.adialog .a-procs', /PID 4242 · cli/);
		await t.shot('home-activation-dialog', 'The switch approval in the frame, over Home');
		await t.page.click('.adialog .btn.primary');
		await eventually(t, async () => activations.length, (n) => n >= 2);
		await t.check('Yes sends the one-use token for that target only', activations.length === 2 && activations[1].body?.confirmationToken === TOKEN, JSON.stringify(activations[1]));
		await t.until('the dialog closes', () => !document.querySelector('.adialog'), null, { timeout: 8000 });
		await t.until('the toast says it switched', () => /Codex account switched/.test(document.querySelector('.toasts')?.textContent || ''), null, { timeout: 8000 });
		await t.noConsoleErrors('console: only the 409 that asks for the approval', { allow: [/status of 409/] });
	},

	async 'home: Keep current and Cancel change nothing'(t) {
		await openHome(t);
		const activations = await answer(t, '/api/codex/profiles/second/activate', () => ({ status: 409, json: { code: 'confirmation_required', confirmation: {
			token: TOKEN, targetProfile: 'second', expiresAt: new Date(Date.now() + 5 * 60_000).toISOString(), processes: [], warning: 'Active Codex work will stop.' } } }));
		await t.page.click(`${ROW('codex:second')} .hact`);
		await t.page.click(`${ROW('codex:second')} .hconfirm .btn.ghost`);
		await t.until('Keep current closes the question', () => !document.querySelector('.hconfirm'), null, { timeout: 4000 });
		await t.check('and sends nothing', activations.length === 0);
		await t.page.click(`${ROW('codex:second')} .hact`);
		await t.page.click(`${ROW('codex:second')} .hconfirm .btn.accent-line`);
		await t.until('the approval opens', () => !!document.querySelector('.adialog'), null, { timeout: 8000 });
		await t.text('.adialog .a-procs', /No running processes were listed/);
		await t.page.click('.adialog .a-btns .btn:not(.primary)');
		await t.until('Cancel closes it', () => !document.querySelector('.adialog'), null, { timeout: 8000 });
		await t.until('and says nothing changed', () => /Switch canceled/.test(document.querySelector('.toasts')?.textContent || ''), null, { timeout: 8000 });
		await t.check('one request (the first ask), no token sent', activations.length === 1 && !activations[0].body?.confirmationToken, JSON.stringify(activations));
		await t.check('the active account is unchanged', await t.page.evaluate(() => !!document.querySelector('.hrow[data-row="codex:primary"] .hmark.on')));
		await t.noConsoleErrors('console: only the 409 that asks for the approval', { allow: [/status of 409/] });
	},

	async 'home: an account below the switch point activates at once, and the highlight glides to it'(t) {
		await openHome(t);
		const activations = await answer(t, '/api/codex/profiles/third/activate', () => ({ status: 200, json: { name: 'third', email: 'codex.third@example.invalid' } }));
		// After the switch the dashboard reads again: the next reading says third is active.
		const after = homeAccounts();
		for (const a of after.accounts) if (a.provider === 'codex') a.isActive = a.capabilities.codexProfile === 'third';
		const before = await t.page.evaluate(() => document.querySelector('.hsec[data-sec="codex"] .hglide').getBoundingClientRect().top);
		await t.app.stub('/api/accounts/dashboard', after);
		await t.page.click(`${ROW('codex:third')} .hact`);
		await eventually(t, async () => activations.length, (n) => n >= 1);
		await t.check('no question for an account under the switch point', activations.length === 1 && !(await t.page.$('.hconfirm')));
		await t.until('third becomes the active row', () => !!document.querySelector('.hrow[data-row="codex:third"] .hmark.on'), null, { timeout: 8000 });
		await t.page.waitForTimeout(800);
		await t.check('the one highlight moved to the new row', await t.page.evaluate((before) => {
			const g = document.querySelector('.hsec[data-sec="codex"] .hglide').getBoundingClientRect();
			const r = document.querySelector('.hrow[data-row="codex:third"]').getBoundingClientRect();
			return g.top > before && Math.abs(g.top - r.top - 3) < 2;
		}, before));
		await t.check('the slot keeps its width through the switch', await t.page.evaluate(() => {
			const w = [...document.querySelectorAll('.hsec[data-sec="codex"] .hslot')].map((s) => Math.round(s.getBoundingClientRect().width));
			return new Set(w).size === 1;
		}));

		// A switch reported while Home's tab is hidden: the highlight is on the new row as soon as Home shows again.
		await t.page.click('.chrome .nav a[href="/app/status"]');
		await t.until('Home is hidden behind Status', () => document.querySelector('.home').closest('.route').hidden, null, { timeout: 8000 });
		const back = homeAccounts();
		for (const a of back.accounts) if (a.provider === 'codex') a.isActive = a.capabilities.codexProfile === 'second';
		await t.app.stub('/api/accounts/dashboard', back);
		await t.page.evaluate(() => window.aacFrameDashboard.command('refresh'));
		await t.until('the hidden Home has the new active row', () => !!document.querySelector('.hrow[data-row="codex:second"] .hmark.on'), null, { timeout: 15000 });
		await t.page.click('.chrome .nav a[href="/app/home"]');
		await t.page.waitForTimeout(400);
		await t.check('shown again, the highlight sits on that row', await t.page.evaluate(() => {
			const g = document.querySelector('.hsec[data-sec="codex"] .hglide').getBoundingClientRect();
			const r = document.querySelector('.hrow[data-row="codex:second"]').getBoundingClientRect();
			return Math.abs(g.top - r.top - 3) < 2 && Math.abs(g.height - (r.height - 6)) < 2;
		}));
	},

	async 'home: auto-switch and its switch point go to the server as before'(t) {
		await openHome(t);
		const saves = await answer(t, '/api/codex/profiles/auto-switch', (body) => ({ status: 200, json: {
			enabled: body?.enabled ?? true, thresholdPercent: body?.thresholdPercent ?? 5, pollIntervalSeconds: 60, outcome: 'healthy', message: 'Saved.' } }));
		await t.page.click('.hsec[data-sec="codex"] .hstep button[aria-label="Switch later"]');
		await t.page.click('.hsec[data-sec="codex"] .hstep button[aria-label="Switch later"]');
		await t.text('.hsec[data-sec="codex"] .hstep-val', /97%/);
		await t.check('the notch moves at once, before the save', await t.page.evaluate(() => parseFloat(document.querySelector('.hsec[data-sec="codex"] .um-notch').style.left) === 97));
		await t.page.waitForTimeout(300);
		await t.check('nothing is sent while still picking', saves.length === 0);
		await eventually(t, async () => saves.length, (n) => n >= 1, 3000);
		await t.page.waitForTimeout(700);
		await t.check('one save, 600 ms after the last step, in % left', saves.length === 1 && saves[0].method === 'PUT' && saves[0].body?.thresholdPercent === 3, JSON.stringify(saves));
		await t.page.click('.hsec[data-sec="codex"] .htoggle');
		await eventually(t, async () => saves.length, (n) => n >= 2);
		await t.check('the toggle sends enabled: false', saves[1]?.body?.enabled === false, JSON.stringify(saves[1]));
		await t.until('the toast repeats what the server confirmed', () => /Codex auto-switch off/.test(document.querySelector('.toasts')?.textContent || ''), null, { timeout: 6000 });
	},

	async 'home: Open on Mac starts Claude, and a control that can\'t says why'(t) {
		await openHome(t);
		const opens = await answer(t, '/api/claude/desktop-profiles/pro-a/open', () => ({ status: 202, json: { id: 'pro-a', platform: 'mac', state: 'starting', operationId: 'op-1' } }));
		await t.page.click(`${ROW('claude:pro-a')} .hopen[aria-label="Open on Mac"]`);
		await eventually(t, async () => opens.length, (n) => n >= 1);
		await t.check('one Open request for that profile on the Mac', opens.length === 1 && opens[0].body?.platform === 'mac', JSON.stringify(opens));
		// A refused control stays tappable (aria-disabled, not disabled), so it can say why.
		await t.page.click(`${ROW('claude:max-b')} .hopen[aria-label^="Open on Windows"]`, { force: true });
		await t.text(`${ROW('claude:max-b')} .hreason`, /Open on Windows is unavailable for this profile/);
		await t.check('and sends nothing', opens.length === 1);
	},

	async 'home: layout in each format (tables when there is room, two sections a row, full rows of cards)'(t) {
		const formats = [
			{ name: 'desktop 2560', width: 2560, height: 1300, table: ['claude', 'codex', 'antigravity'], side: true },
			{ name: 'desktop 1440', width: 1440, height: 900, table: ['claude', 'codex', 'antigravity'], side: false },
			{ name: 'desktop 1024', width: 1024, height: 800, table: ['antigravity'], side: true },
			{ name: 'iphone', width: 402, height: 812, mobile: true, safe: '0,0,34,0', table: [], side: false },
			{ name: 'duo folded', width: 466, height: 678, mobile: true, safe: '0,84,34,0', table: [], side: false },
			{ name: 'duo open upright', width: 669, height: 869, mobile: true, safe: '0,0,34,0', table: ['codex'], side: false },
			{ name: 'duo landscape', width: 951, height: 669, mobile: true, safe: '0,84,34,0', table: ['antigravity'], side: true },
		];
		for (const f of formats) {
			await openHome(t, f);
			await t.page.waitForTimeout(300);
			const l = await t.page.evaluate(layout);
			const tables = l.sections.filter((s) => s.table).map((s) => s.id);
			await t.check(`${f.name}: table rows where the section is wide enough`, JSON.stringify(tables) === JSON.stringify(f.table), JSON.stringify(l.sections));
			const [claude, codex] = l.sections;
			await t.check(`${f.name}: Claude and Codex ${f.side ? 'share a row' : 'stack'}`, f.side ? codex.left > claude.left : codex.left === claude.left, JSON.stringify(l.sections));
			await t.check(`${f.name}: every row of cards is full (rows ${l.cardRows.join('+')})`, l.holes === 0);
			await t.check(`${f.name}: nothing runs off the side`, !l.sideways && l.overflow.length === 0, JSON.stringify(l.overflow));
			await t.audit(`home ${f.name}`);
		}
	},
};
