// Home drawn natively in the app frame (PLAN step 5, G5): the frame's dashboard controller (public/frame-dashboard.mjs)
// reads the dashboard and builds view model v3 with view-model.mjs; the page draws it and sends the dashboard's own
// action kinds back (dash.rs). No Slint dashboard is loaded. Real test server and sign-in (the kit's temporary home); the
// dashboard's readings and every Open request are answered in the page (made-up data, home-fixture.mjs), so nothing
// real is opened. Account switching was removed (2026-11-14): Home marks the live login and offers no switch.
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';

const ROW = (id) => `.hrow[data-row="${id}"]`;

async function openHome(t, { width = 1440, height = 900, mobile = false, safe = '', screen } = {}) {
	await t.newContext({ viewport: { width, height }, screen, isMobile: mobile, hasTouch: mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
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
	async 'home meter cards: every meter is on top and inside the viewport'(t) {
		const layouts = [
			{ name: 'desktop', width: 1760, height: 1080 },
			{ name: 'iPhone 17 Pro', width: 402, height: 874, mobile: true, safe: '0,0,34,0' },
			{ name: 'Duo folded', width: 466, height: 678, mobile: true, screen: { width: 466, height: 678 }, safe: '0,84,34,0' },
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
					const outside = await t.page.evaluate(() => {
						const title = document.querySelector('.topbar .t-title').getBoundingClientRect();
						const rail = document.querySelector('.rail').getBoundingClientRect();
						return rail.width
							? [rail.right - 4, rail.top + rail.height / 2]
							: [title.left + title.width / 2, title.top + title.height / 2];
					});
					await t.page.touchscreen.tap(...outside);
					await t.check(`${layout.name}: outside tap dismisses meter ${i + 1}`, !await card.isVisible());
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
		await t.check('no switch control, switch point or approval is drawn', await t.page.evaluate(() =>
			!document.querySelector('.um-notch, .hauto, .hauto-off, .hconfirm, .hact, .hglide, .adialog')));
		await t.check('a Pro account leaves its Fable cell empty', await t.page.evaluate(() => document.querySelectorAll('.hrow[data-row="claude:pro-a"] .hcell')[2]?.classList.contains('empty')));
		await t.check('the live Codex login says where it is active, and only that row does', await t.page.evaluate(() => {
			const row = document.querySelector('.hrow[data-row="codex:primary"]');
			return row.classList.contains('active') && row.querySelector('.hmark.on')?.textContent.includes('Active')
				&& document.querySelectorAll('.hsec[data-sec="codex"] .hmark.on').length === 1;
		}));
		await t.text('.hrow[data-row="codex:primary"] .hamts', /62\.5K\s*credits/);
		await t.check('Codex has no auto-switch footer', await t.page.evaluate(() => !document.querySelector('.hsec[data-sec="codex"] .hfoot')));
		await t.count('.hcard', 6);
		await t.text('.hcard', /\$12\.40/);
		await t.audit('home 1440');

		// A refresh with a new reading moves the meter but rebuilds nothing else: the Active mark is the same element (no
		// replayed draw-on).
		await t.page.evaluate(() => {
			document.querySelector('.hrow[data-row="codex:primary"] .hmark').__kit = 1;
		});
		const next = homeAccounts();
		next.accounts.find((a) => a.id === 'codex:primary').windows[0].usedPercent = 61;
		await t.app.stub('/api/accounts/dashboard', next);
		await t.page.click('.chrome button[aria-label="Refresh"]');
		await t.until('the new reading lands', () => /^61%$/.test(document.querySelector('.hrow[data-row="codex:primary"] .um-num')?.textContent || ''), null, { timeout: 15000 });
		await t.check('the Active mark is kept through a refresh', await t.page.evaluate(() =>
			document.querySelector('.hrow[data-row="codex:primary"] .hmark').__kit === 1));
		await t.noConsoleErrors('console: clean');
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
