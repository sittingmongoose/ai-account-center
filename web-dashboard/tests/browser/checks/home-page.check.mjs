// Home drawn natively in the app frame (PLAN step 5, G5): the frame's dashboard controller (public/frame-dashboard.mjs)
// reads the dashboard and builds view model v3 with view-model.mjs; the page draws it and sends the dashboard's own
// action kinds back (dash.rs). No Slint dashboard is loaded. Real test server and sign-in (the kit's temporary home); the
// dashboard's readings and every Open request are answered in the page (made-up data, home-fixture.mjs), so nothing
// real is opened. Account switching was removed (2026-11-14) and the live-login mark was removed (2026-11-25).
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';

const ROW = (id) => `.hrow[data-row="${id}"]`;

async function openHome(t, { width = 1440, height = 900, mobile = false, safe = '', screen, reducedMotion } = {}) {
	await t.newContext({ viewport: { width, height }, screen, isMobile: mobile, hasTouch: mobile, deviceScaleFactor: 1, serviceWorkers: 'block', reducedMotion });
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

const phoneLayouts = [
	{ name: 'iPhone 17 Pro', width: 402, height: 874, mobile: true, safe: '0,0,34,0' },
	{ name: 'Duo folded', width: 466, height: 678, mobile: true, screen: { width: 466, height: 678 }, safe: '0,84,34,0' },
];

async function tapOutsideMeter(t) {
	const outside = await t.page.evaluate(() => {
		const title = document.querySelector('.topbar .t-title').getBoundingClientRect();
		if (title.width) return [title.left + title.width / 2, title.top + title.height / 2];
		// The folded rail has open space above its links. Stay clear of touch target adjustment.
		const rail = document.querySelector('.rail');
		const bounds = rail.getBoundingClientRect();
		const first = rail.querySelector('a').getBoundingClientRect();
		return [bounds.left + bounds.width / 2, (bounds.top + first.top) / 2];
	});
	await t.page.touchscreen.tap(...outside);
}

export const tests = {
	async 'home meter keyboard reveal: a phone Tab scroll keeps the focused card open'(t) {
		try {
			for (const [i, layout] of phoneLayouts.entries()) {
				await openHome(t, { ...layout, reducedMotion: 'reduce' });
				const owner = t.page.locator('.hcard').filter({ hasText: 'Z.ai Coding Plan' });
				const meter = owner.locator('.um').first();
				const card = meter.locator('.um-card');
				const openWithTab = async () => {
					await owner.evaluate((el) => el.focus({ preventScroll: true }));
					await t.page.keyboard.press('Tab');
				};
				await owner.evaluate((el) => el.focus({ preventScroll: true }));
				const before = await meter.evaluate(async (el) => {
					const targetTop = document.documentElement.clientHeight + 20;
					for (const scroller of [document.querySelector('.content'), document.scrollingElement]) {
						scroller.scrollTop += el.getBoundingClientRect().top - targetTop;
					}
					await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
					return { top: el.getBoundingClientRect().top, viewport: document.documentElement.clientHeight, focused: document.activeElement === el.closest('.hcard') };
				});
				await t.check(`${layout.name}: Tab starts on the card with its meter below the viewport`,
					before.focused && Math.abs(before.top - before.viewport - 20) <= 2, JSON.stringify(before));
				await t.page.keyboard.press('Tab');
				const after = await meter.evaluate(async (el) => {
					// Read the painted state after the browser dispatches its queued scroll events.
					await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => requestAnimationFrame(resolve))));
					const anchor = el.getBoundingClientRect();
					const tip = el.querySelector('.um-card');
					const bounds = tip.getBoundingClientRect();
					const { clientWidth: width, clientHeight: height } = document.documentElement;
					const hit = document.elementFromPoint(bounds.left + bounds.width / 2, bounds.top + bounds.height / 2);
					return {
						focused: document.activeElement === el && el.matches(':focus-visible'),
						top: anchor.top,
						open: tip.matches(':popover-open'),
						adjacent: Math.min(Math.abs(bounds.top - anchor.bottom), Math.abs(bounds.bottom - anchor.top)) <= 1,
						onTop: tip.contains(hit),
						fits: bounds.left >= 0 && bounds.top >= 0 && bounds.right <= width + 0.5 && bounds.bottom <= height + 0.5,
					};
				});
				await t.check(`${layout.name}: Tab scrolled the meter into view and kept keyboard focus`,
					after.focused && after.top >= 0 && after.top < before.viewport, JSON.stringify(after));
				await t.check(`${layout.name}: the painted tooltip follows the focused meter`,
					after.open && after.adjacent && after.onTop && after.fits, JSON.stringify(after));
				await t.shot(i === 0 ? 'home-tab-iphone' : 'home-tab-duo', 'The focused Z.ai meter after Tab scrolls it into view');

				await t.page.keyboard.press('Escape');
				await t.check(`${layout.name}: Escape dismisses the tooltip and keeps meter focus`,
					!await card.isVisible() && await meter.evaluate((el) => el === document.activeElement));
				await openWithTab();
				await t.check(`${layout.name}: Tab can open the tooltip again`, await card.isVisible());
				await t.page.keyboard.press('Tab');
				await t.check(`${layout.name}: moving focus to the next stop closes the tooltip`,
					!await card.isVisible() && await meter.evaluate((el) => el !== document.activeElement));

				await openWithTab();
				await t.check(`${layout.name}: tooltip is open before outside scrolling`, await card.isVisible());
				const top = await meter.evaluate((el) => el.getBoundingClientRect().top);
				await t.page.mouse.move(6, layout.height / 2);
				await t.page.mouse.wheel(0, -100);
				await t.until(`${layout.name}: outside wheel scrolls Home`, (before) => document.activeElement.getBoundingClientRect().top > before + 8, top, { timeout: 5000 });
				await t.check(`${layout.name}: outside scroll dismisses the tooltip and keeps meter focus`,
					!await card.isVisible() && await meter.evaluate((el) => el === document.activeElement));

				await openWithTab();
				await t.check(`${layout.name}: tooltip is open before the outside tap`, await card.isVisible());
				await tapOutsideMeter(t);
				await t.check(`${layout.name}: outside tap dismisses the tooltip and keeps Home open`,
					!await card.isVisible() && await meter.isVisible());
			}
		} finally {
			// reduced motion is this test's alone: later checks (refresh-button's spin) see the default
			await t.emulate({ reducedMotion: 'no-preference' });
		}
	},

	async 'home meter cards: every meter is on top and inside the viewport'(t) {
		const layouts = [
			{ name: 'desktop', width: 1760, height: 1080 },
			...phoneLayouts,
		];
		for (const layout of layouts) {
			await openHome(t, layout);
			await t.app.noResets();
			const meters = t.page.locator('.home .um');
			const providers = await meters.evaluateAll((elements) => [...new Set(elements.map((el) =>
				el.closest('.hsec')?.dataset.sec || (el.closest('.hcard') && 'other')))]);
			await t.equal(`${layout.name}: all four provider sections have meters`, providers, ['claude', 'codex', 'antigravity', 'other']);
			let flippedAtBottom = false;
			for (let i = 0, count = await meters.count(); i < count; i++) {
				const meter = meters.nth(i);
				const card = meter.locator('.um-card');
				if (layout.mobile) {
					await meter.tap();
				} else {
					await meter.hover();
					// The baseline's CSS hover transition delays visibility.
					await card.waitFor({ state: 'visible', timeout: 1500 }).catch(() => {});
				}
				const probe = await card.evaluate((el) => {
					const { left, top, right, bottom, width, height } = el.getBoundingClientRect();
					const hit = document.elementFromPoint(left + width / 2, top + height / 2);
					const anchor = el.closest('.um').getBoundingClientRect();
					const viewportHeight = document.documentElement.clientHeight;
					return {
						onTop: el.contains(hit),
						inside: width > 0 && height > 0 && left >= -0.5 && right <= document.documentElement.clientWidth + 0.5
							&& top >= -0.5 && bottom <= viewportHeight + 0.5,
						flipped: height > 0 && height > viewportHeight - anchor.bottom && bottom <= anchor.top + 1,
						row: el.closest('.hrow')?.dataset.row,
						cell: [...el.closest('.hrow')?.querySelectorAll('.hcell') || []].indexOf(el.closest('.hcell')),
						hit: hit?.className,
						rect: [left, top, right, bottom],
					};
				});
				flippedAtBottom ||= probe.flipped;
				await t.check(`${layout.name}: meter ${i + 1} paints its tooltip center inside the viewport`, probe.onTop && probe.inside, JSON.stringify(probe));
				if (!layout.mobile && probe.row === 'claude:max-c' && probe.cell === 1) {
					await t.shot('home-weekly-tip', 'Claude weekly usage card above the following rows');
				}
				if (layout.mobile) {
					// A tap that opens Details instead leaves the fixture in a different screen.
					const details = t.page.locator('[data-probe="details"]');
					if (await details.isVisible()) {
						await t.check(`${layout.name}: tap on meter ${i + 1} opens the tooltip without Details`, false);
						break;
					}
					await tapOutsideMeter(t);
					await t.check(`${layout.name}: outside tap dismisses meter ${i + 1}`, !await card.isVisible());
					if (!await meter.isVisible()) {
						await t.check(`${layout.name}: tapping outside keeps Home open`, false);
						break;
					}
				} else {
					await t.page.mouse.move(0, 0);
				}
			}
			if (!layout.mobile) {
				await t.check('desktop: tooltip opens above a meter at the bottom edge', flippedAtBottom);
				const first = meters.first();
				await t.page.locator('.hrow').first().focus();
				await t.page.keyboard.press('Tab');
				await t.check('Tab moves focus into the meter', await first.evaluate((el) => document.activeElement === el));
				const card = first.locator('.um-card');
				await t.check('keyboard focus opens the tooltip', await card.isVisible());
				await t.page.keyboard.press('Escape');
				await t.check('Escape dismisses the tooltip and keeps focus', !await card.isVisible()
					&& await first.evaluate((el) => document.activeElement === el));
			}
		}
	},

	async 'home: draws the dashboard view natively, with no Slint on screen'(t) {
		await openHome(t);
		await t.count('.hsec', 3);
		await t.text('.hsec[data-sec="claude"] .plate-meta', /4\s*accounts/);
		await t.text('.hsec[data-sec="codex"] .plate-meta', /3\s*accounts/);
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
		await t.check('no switch control, switch point or approval is drawn', await t.page.evaluate(() =>
			!document.querySelector('.um-notch, .hauto, .hauto-off, .hconfirm, .hact, .hglide, .adialog')));
		await t.check('a Pro account leaves its Fable cell empty', await t.page.evaluate(() => document.querySelectorAll('.hrow[data-row="claude:pro-a"] .hcell')[2]?.classList.contains('empty')));
		await t.check('no Home row carries an Active mark', await t.page.evaluate(() =>
			!document.querySelector('.hmark, .hmark-t, .hcheck, .active-mark') && !document.querySelector('.hrow.active')));
		await t.text('.hrow[data-row="codex:primary"] .hamts', /62\.5K\s*credits/);
		await t.check('Codex has no auto-switch footer', await t.page.evaluate(() => !document.querySelector('.hsec[data-sec="codex"] .hfoot')));
		await t.count('.hcard', 6);
		await t.text('.hcard', /\$12\.40/);
		await t.audit('home 1440');

		// A refresh with a new reading moves the meter but rebuilds nothing else: the row is the same element (no
		// replayed draw-on).
		await t.page.evaluate(() => {
			document.querySelector('.hrow[data-row="codex:primary"]').__kit = 1;
		});
		const next = homeAccounts();
		next.accounts.find((a) => a.id === 'codex:primary').windows[0].usedPercent = 61;
		await t.app.stub('/api/accounts/dashboard', next);
		await t.page.click('.chrome button[aria-label="Refresh"]');
		await t.until('the new reading lands', () => /^61%$/.test(document.querySelector('.hrow[data-row="codex:primary"] .um-num')?.textContent || ''), null, { timeout: 15000 });
		await t.check('the row is the same element through a refresh and no Active mark appears', await t.page.evaluate(() =>
			document.querySelector('.hrow[data-row="codex:primary"]').__kit === 1 && !document.querySelector('.hmark, .active-mark')));
		await t.noConsoleErrors('console: clean');
	},

	async 'home: Open on Mac starts Claude, and a control that can\'t says why'(t) {
		await openHome(t);
		const opens = await answer(t, '/api/claude/desktop-profiles/pro-a/open', () => ({ status: 202, json: { id: 'pro-a', platform: 'mac', state: 'starting', operationId: 'op-1' } }));
		await t.page.click(`${ROW('claude:pro-a')} .hopen[aria-label="Open on Mac"]`);
		await eventually(t, async () => opens.length, (n) => n >= 1);
		await t.check('one Open request for that profile on the Mac', opens.length === 1 && opens[0].body?.platform === 'mac', JSON.stringify(opens));
		// Open is asynchronous and temporarily disables every Open control.
		await t.page.locator(`${ROW('claude:pro-a')} .hopen[aria-label="Open on Mac"][aria-disabled="false"]`).waitFor({ state: 'visible' });
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

	// home-stale-after-account-add: a key saved in another browser reaches the server, which broadcasts
	// accounts-changed on /ws; Home must redraw on the hint alone, with no reload and no wait for the interval read.
	async 'home: a named account is headed by its name over its email, on rows and cards, at 402 px'(t) {
		await t.newContext({ viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1, serviceWorkers: 'block' });
		await t.app.signIn();
		const data = homeAccounts();
		// a renamed Claude profile, and an OpenCode Go key whose provider reports no email but the owner typed one
		data.accounts.find((a) => a.id === 'claude:max-b').label = 'Work Claude';
		const key = data.accounts.find((a) => a.provider === 'opencode-go');
		Object.assign(key, { id: 'opencode-go:acct:0123abcd', label: 'Account 2', email: null, ownerEmail: 'go.key@example.invalid', providerLabel: 'OpenCode Go' });
		await t.app.stub('/api/accounts/dashboard', data);
		await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
		await t.goto('/app/home?safe=0,0,34,0', { settleMs: 300 });
		await t.until('Home is drawn', () => document.querySelectorAll('.home .hrow').length >= 4 && document.querySelectorAll('.hcard').length >= 6, null, { timeout: 60000, hard: true });
		const named = await t.page.evaluate(() => {
			const r = document.querySelector('.hrow[data-row="claude:max-b"]');
			const plain = document.querySelector('.hrow[data-row="claude:pro-a"]');
			return { head: r?.querySelector('.hemail')?.textContent, sub: r?.querySelector('.hsub')?.textContent, plainHead: plain?.querySelector('.hemail')?.textContent, plainSub: !!plain?.querySelector('.hsub') };
		});
		await t.equal('the renamed Claude row: name over email; an unnamed row keeps its email heading', named,
			{ head: 'Work Claude', sub: 'second.claude.account@example.invalid', plainHead: 'a.very.long.example.name@example.invalid', plainSub: false });
		const card = await t.page.evaluate(() => {
			const c = [...document.querySelectorAll('.hcard')].find((el) => /Account 2/.test(el.textContent));
			const id = c?.querySelector('.hcard-id');
			const sub = c?.querySelector('.hcard-sub');
			const lh = (el) => parseFloat(getComputedStyle(el).lineHeight) || 18;
			return c ? { id: id?.textContent, sub: sub?.textContent, oneLine: !!id && !!sub && id.getBoundingClientRect().height < lh(id) * 1.5 && sub.getBoundingClientRect().height < lh(sub) * 1.5 } : null;
		});
		await t.equal('the OpenCode Go card: the key\'s name over the email the owner typed, each one line', card, { id: 'Account 2', sub: 'go.key@example.invalid', oneLine: true });
		await t.check('the phone page never scrolls sideways', await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
		await t.page.evaluate(() => document.querySelector('.hrow[data-row="claude:max-b"]')?.scrollIntoView({ block: 'center' }));
		await t.page.waitForTimeout(200);
		await t.snap('name-heading-home-rows-402x874');
		await t.page.evaluate(() => [...document.querySelectorAll('.hcard')].find((el) => /Account 2/.test(el.textContent))?.scrollIntoView({ block: 'center' }));
		await t.snap('name-heading-home-cards-402x874');
	},

	async 'home: an accounts-changed hint redraws Home without a reload'(t) {
		for (const f of [
			{ name: 'desktop', width: 1440, height: 900 },
			{ name: 'iphone', width: 402, height: 874, mobile: true, safe: '0,0,34,0' },
		]) {
			await t.step(f.name, async () => {
				await t.newContext({ viewport: { width: f.width, height: f.height }, isMobile: !!f.mobile, hasTouch: !!f.mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
				await t.app.signIn();
				const sockets = [];
				await t.page.routeWebSocket('**/ws', (ws) => { sockets.push(ws); });
				await t.app.stub('/api/accounts/dashboard', homeAccounts());
				await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
				await t.goto(`/app/home?e2e${f.safe ? `&safe=${f.safe}` : ''}`, { settleMs: 300 });
				await t.until('Home drawn from the dashboard\'s view', () => document.querySelectorAll('.home .hrow').length >= 8 && document.querySelectorAll('.hcard').length >= 6, null, { timeout: 60000, hard: true });
				await t.page.evaluate(() => { window.__aacNoReload = 1; });
				const cardsBefore = await t.page.evaluate(() => document.querySelectorAll('.hcard').length);
				// the added OpenCode Go key: the next dashboard answer carries it, and the server sends the hint
				const next = homeAccounts();
				next.accounts.push({
					id: 'opencode-go:two', provider: 'opencode-go', providerLabel: 'opencode-go', label: 'second.opencode.go@example.invalid',
					email: 'second.opencode.go@example.invalid', plan: 'go', platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok',
					message: null, fetchedAt: new Date().toISOString(), sampledAt: new Date().toISOString(), isActive: false, hidden: false, trayHidden: false,
					windows: [{ key: 'monthly_spend', label: 'Monthly spend', kind: 'spend', unit: 'USD', used: 1.2, limit: 10, resetAt: new Date(Date.now() + 15000 * 60_000).toISOString(), usedPercent: null, remainingPercent: null, windowMinutes: null }],
					capabilities: {}, signInNeeded: [],
				});
				await t.app.stub('/api/accounts/dashboard', next);
				await t.page.waitForTimeout(300);
				await t.check('the hint socket is connected', sockets.length >= 1, `${sockets.length} sockets`);
				for (const ws of sockets) ws.send(JSON.stringify({ type: 'accounts-changed' }));
				// the coalesced re-read lands well inside this wait; the interval read (60 s) cannot have run
				await t.page.waitForTimeout(1500);
				// show the Other providers cards, where the added OpenCode Go key lands
				await t.page.evaluate(() => { const cards = document.querySelectorAll('.hcard'); cards[cards.length - 1]?.scrollIntoView({ block: 'center' }); });
				await t.page.waitForTimeout(200);
				if (t.args.shots) await t.shot(`home-hint-${f.name}`, `${f.name}: Home after the accounts-changed hint`);
				let redrawn = true;
				try { await t.page.waitForFunction(() => [...document.querySelectorAll('.hcard')].some((c) => c.textContent.includes('second.opencode.go@example.invalid')), null, { timeout: 8000 }); } catch { redrawn = false; }
				await t.check('Home redraws with the added account', redrawn);
				if (redrawn) {
					await t.equal('one more card, no others lost', await t.page.evaluate(() => document.querySelectorAll('.hcard').length), cardsBefore + 1);
					await t.check('the page never reloaded', await t.page.evaluate(() => window.__aacNoReload === 1));
					await t.audit(`home hint ${f.name}`);
				}
			});
		}
	},
};
