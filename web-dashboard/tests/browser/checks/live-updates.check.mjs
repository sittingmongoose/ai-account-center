// A signed-in page left open while its readings change (owner report 2026-11-20: "I leave the page up and it
// frequently flashes and reloads. Every 30-60 seconds or so"). Live updates must patch only what changed: no
// navigation, no re-mount of the app or of a page's parts, no meter replaying its first sweep from zero, no blank
// frame, and the scroll position and the focused control stay where they were.
//
// The dashboard's numbers change on every read (the refresh interval is 30 s here), so updates really arrive; the
// page is opened with ?motion, so a re-created meter would visibly sweep from 0 again as it does for a person.
// `--set idleSeconds=N` changes the 300 s idle period of the Home test (the Status and Analytics tests idle 90 s).
// `film desktop home` is the same Home check on the run's own page, so it can be recorded (`--grep film --record`):
//   browser-check run web-dashboard/tests/browser/checks/live-updates.check.mjs --grep film --record /abs/home-idle.mp4
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';
import { analyticsFixture } from '../device/analytics-fixture.mjs';

/** Counts what a flash is made of, from the first script on: documents started (a navigation or reload), the app's
 * root and routes removed, element churn, meters created or swept back to zero, and frames with no page drawn. */
function instrument() {
	let docs = 0;
	try {
		docs = Number(sessionStorage.getItem('aacFlashDocs') || 0) + 1;
		sessionStorage.setItem('aacFlashDocs', String(docs));
	} catch {}
	const f = (window.__flash = {
		docs, armed: false, shellRemoved: 0, routeRemoved: 0, pageRemoved: 0, rowsRemoved: 0, metersAdded: 0, metersRemoved: 0,
		sweeps: 0, removedEls: 0, blankFrames: 0, frames: 0, signinSeen: 0, naAtArm: 0, naMax: 0, naRestored: false,
	});
	// meters reading "Unavailable" on Home (a window whose reading went away)
	const unavailable = () => [...document.querySelectorAll('.home .um-na')].filter((e) => e.textContent.trim() === 'Unavailable').length;
	const count = (node, sel) => (node.nodeType !== 1 ? 0 : (node.matches(sel) ? 1 : 0) + node.querySelectorAll(sel).length);
	const lastWidth = new WeakMap();
	const observer = new MutationObserver((records) => {
		if (!f.armed) return;
		for (const r of records) {
			if (r.type === 'childList') {
				for (const n of r.removedNodes) {
					if (n.nodeType !== 1) continue;
					f.removedEls += 1 + n.querySelectorAll('*').length;
					f.shellRemoved += count(n, '.shell');
					f.routeRemoved += count(n, '.route');
					f.pageRemoved += count(n, '.page');
					f.rowsRemoved += count(n, '.hrow, .hcard, .plate');
					f.metersRemoved += count(n, '.um');
				}
				for (const n of r.addedNodes) {
					f.metersAdded += count(n, '.um');
					f.signinSeen += count(n, '.signin-page');
				}
			} else if (r.type === 'attributes' && r.target.classList?.contains('fill')) {
				// a meter's fill going back to (near) zero from a real reading: the first sweep replayed
				const m = /width:\s*([\d.]+)%/.exec(r.target.getAttribute('style') || '');
				if (!m) continue;
				const w = Number(m[1]);
				const before = lastWidth.get(r.target);
				lastWidth.set(r.target, w);
				// a meter whose reading went away (its track marked unavailable) empties on purpose
				if (before !== undefined && before >= 5 && w < 1 && !r.target.closest('.um-track.na')) f.sweeps++;
			}
		}
	});
	const start = () => observer.observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ['style'] });
	if (document.documentElement) start();
	else document.addEventListener('readystatechange', start, { once: true });
	const frame = () => {
		if (f.armed) {
			f.frames++;
			const page = document.querySelector('.route:not([hidden]) .page');
			if (!page || page.getBoundingClientRect().height < 50 || document.querySelector('.signin-page')) f.blankFrames++;
			if (f.frames % 15 === 0) {
				const n = unavailable();
				if (n > f.naMax) f.naMax = n;
				if (f.naMax > f.naAtArm && n === f.naAtArm) f.naRestored = true;
			}
		}
		requestAnimationFrame(frame);
	};
	requestAnimationFrame(frame);
	window.__flashArm = () => {
		for (const el of document.querySelectorAll('.um-track .fill')) {
			const m = /width:\s*([\d.]+)%/.exec(el.getAttribute('style') || '');
			if (m) lastWidth.set(el, Number(m[1]));
		}
		// the scrolled boxes and the focused control as they are now
		const scrolled = [document.scrollingElement, ...document.querySelectorAll('*')].filter((el) => el && el.scrollTop > 0);
		f.scroll = scrolled.map((el) => ({ el, top: el.scrollTop }));
		f.focus = document.activeElement;
		f.root = document.querySelector('.shell');
		f.naAtArm = f.naMax = unavailable();
		try { sessionStorage.setItem('aacFlashArmDocs', String(docs)); } catch {}
		f.armed = true;
	};
	window.__flashRead = () => ({
		// documents started since arming: a navigation or reload starts one
		docs: Number(sessionStorage.getItem('aacFlashDocs') || 0) - Number(sessionStorage.getItem('aacFlashArmDocs') || 0),
		navigations: performance.getEntriesByType('navigation').length,
		shellRemoved: f.shellRemoved, routeRemoved: f.routeRemoved, pageRemoved: f.pageRemoved, rowsRemoved: f.rowsRemoved,
		metersAdded: f.metersAdded, metersRemoved: f.metersRemoved, sweeps: f.sweeps, removedEls: f.removedEls,
		blankFrames: f.blankFrames, frames: f.frames, signinSeen: f.signinSeen, naAtArm: f.naAtArm, naMax: f.naMax, naRestored: f.naRestored,
		sameRoot: f.root === document.querySelector('.shell') && !!f.root?.isConnected,
		scrollResets: (f.scroll || []).filter((s) => Math.abs(s.el.scrollTop - s.top) > 1).map((s) => `${s.el.className || s.el.tagName}: ${s.top} -> ${s.el.scrollTop}`),
		scrolledBoxes: (f.scroll || []).length,
		sameFocus: f.focus === document.activeElement && !!f.focus?.isConnected,
		focus: document.activeElement?.className || document.activeElement?.tagName,
	});
}

/** Home's dashboard, its numbers moved on every read (and its refresh interval 30 s). Every fifth read from the third
 * on, the first Claude account's five-hour reading goes away (the meter must say Unavailable in place), and the next
 * read brings it back. */
function changingDashboard(call) {
	const data = homeAccounts();
	data.settings = { ...data.settings, refreshIntervalSeconds: 30 };
	data.updatedAt = new Date().toISOString();
	for (const a of data.accounts) {
		for (const w of a.windows) {
			if (typeof w.usedPercent === 'number') {
				w.usedPercent = Math.min(99, Math.max(1, w.usedPercent + ((call * 7) % 13) - 6));
				w.remainingPercent = 100 - w.usedPercent;
			}
		}
		a.fetchedAt = a.sampledAt = new Date().toISOString();
	}
	if (call % 5 === 3) {
		const w = data.accounts.find((a) => a.provider === 'claude').windows.find((x) => x.key === 'five_hour');
		w.usedPercent = null;
		w.remainingPercent = null;
	}
	return data;
}

async function stubChanging(t) {
	const reads = { dashboard: 0 };
	await t.page.route((u) => new URL(u).pathname === '/api/accounts/dashboard', (r) => r.fulfill({ json: changingDashboard(++reads.dashboard) }));
	await t.page.route((u) => new URL(u).pathname === '/api/accounts/settings', (r) =>
		r.request().method() === 'GET' ? r.fulfill({ json: { refreshIntervalSeconds: 30 } }) : r.continue());
	await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
	return reads;
}

async function idle(t, seconds, label) {
	await t.page.evaluate(() => window.__flashArm());
	await t.mark(`${label}: armed, idle ${seconds} s`);
	for (let left = seconds; left > 0; left -= 30) await t.page.waitForTimeout(Math.min(30, left) * 1000);
	await t.mark(`${label}: idle over`);
	return t.page.evaluate(() => window.__flashRead());
}

async function judge(t, label, r, { updates }) {
	await t.check(`${label}: updates arrived during the idle period (${updates} reads)`, updates > 0, `reads during the idle: ${updates}`);
	await t.equal(`${label}: no navigation or reload (documents started)`, r.docs, 0);
	await t.check(`${label}: the app root was never re-mounted`, r.shellRemoved === 0 && r.sameRoot, JSON.stringify(r));
	await t.equal(`${label}: no route or page removed`, r.routeRemoved + r.pageRemoved, 0);
	await t.equal(`${label}: no meter re-created`, r.metersAdded + r.metersRemoved, 0);
	await t.equal(`${label}: no meter swept back to zero (${r.sweeps})`, r.sweeps, 0);
	await t.equal(`${label}: no blank frame (of ${r.frames})`, r.blankFrames, 0);
	await t.equal(`${label}: the sign-in page never appeared`, r.signinSeen, 0);
	await t.equal(`${label}: scroll positions kept (${r.scrolledBoxes} scrolled)`, r.scrollResets.join('; '), '');
	await t.check(`${label}: the focused control kept its focus`, r.sameFocus, `focus now on ${r.focus}`);
}

/** Scroll the page's own scroller down a little, and focus a control on screen, before arming. */
async function scrollAndFocus(t, focusSel) {
	await t.page.evaluate((sel) => {
		const page = document.querySelector('.route:not([hidden]) .page');
		let el = page;
		while (el && !(el.scrollHeight > el.clientHeight + 20 && /auto|scroll/.test(getComputedStyle(el).overflowY))) el = el.parentElement;
		(el || document.scrollingElement).scrollTop = 120;
		const target = [...document.querySelectorAll(sel)].find((n) => { const r = n.getBoundingClientRect(); return r.top > 0 && r.bottom < innerHeight; });
		target?.focus();
	}, focusSel);
}

export const tests = {
	async 'idle desktop home: no flash while readings change'(t) {
		const seconds = Number(t.args.idleSeconds || 300);
		await t.newContext({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 });
		await t.page.addInitScript(instrument);
		await t.app.signIn();
		const reads = await stubChanging(t);
		await t.goto('/app/home?e2e&motion', { settleMs: 300 });
		await t.until('Home drawn', () => document.querySelectorAll('.home .hrow').length >= 8, null, { timeout: 60000, hard: true });
		await t.page.waitForTimeout(3000);
		await scrollAndFocus(t, '.hrow');
		const before = reads.dashboard;
		const first = await t.page.evaluate(() => [...document.querySelectorAll('.home .um')].map((m) => m.textContent).join('|'));
		const r = await idle(t, seconds, 'home');
		const last = await t.page.evaluate(() => [...document.querySelectorAll('.home .um')].map((m) => m.textContent).join('|'));
		await t.check('home: the meters show the changed numbers', first !== last, 'the meters read the same before and after');
		await judge(t, 'home', r, { updates: reads.dashboard - before });
		// a live update turned a meter Unavailable mid-run, in place (no meter re-created, above), and the next read
		// brought its number back
		await t.check('home: a meter turned Unavailable mid-run', r.naMax > r.naAtArm, `Unavailable meters: ${r.naAtArm} at the start, at most ${r.naMax}`);
		await t.check('home: the Unavailable meter got its reading back on the next read', r.naRestored, JSON.stringify({ naAtArm: r.naAtArm, naMax: r.naMax }));
	},

	async 'film desktop home: idle while readings change, for a recording'(t) {
		await t.page.addInitScript(instrument);
		await t.app.signIn();
		const reads = await stubChanging(t);
		await t.goto('/app/home?e2e&motion', { settleMs: 300 });
		await t.until('Home drawn', () => document.querySelectorAll('.home .hrow').length >= 8, null, { timeout: 60000, hard: true });
		await t.page.waitForTimeout(3000);
		const before = reads.dashboard;
		const r = await idle(t, Number(t.args.filmSeconds || 95), 'film');
		await judge(t, 'film', { ...r, scrollResets: [], sameFocus: true }, { updates: reads.dashboard - before });
	},

	async 'idle desktop status: no flash while polls arrive'(t) {
		await t.newContext({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 });
		await t.page.addInitScript(instrument);
		await t.app.signIn();
		let polls = 0;
		await t.page.route((u) => new URL(u).pathname === '/api/status', async (r) => {
			polls++;
			const answer = await r.fetch();
			const json = await answer.json();
			if (json?.refresh) json.refresh.pollSeconds = 10;
			await r.fulfill({ response: answer, json });
		});
		await t.goto('/app/status?e2e&motion', { settleMs: 300 });
		await t.until('Status drawn', () => !!document.querySelector('.status-grid'), null, { timeout: 60000, hard: true });
		await t.page.waitForTimeout(2000);
		await scrollAndFocus(t, '.status-grid button, .status-grid [tabindex="0"]');
		const before = polls;
		const r = await idle(t, Number(t.args.pageIdleSeconds || 90), 'status');
		await judge(t, 'status', r, { updates: polls - before });
	},

	async 'idle desktop analytics: no flash while reads arrive'(t) {
		await t.newContext({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 });
		await t.page.addInitScript(instrument);
		await t.app.signIn();
		const reads = await stubChanging(t);
		let analytics = 0;
		await t.page.route((u) => new URL(u).pathname === '/api/accounts/analytics', (r) => {
			analytics++;
			r.fulfill({ json: analyticsFixture({ range: new URL(r.request().url()).searchParams.get('range') || '7d' }) });
		});
		await t.goto('/app/analytics?e2e&motion', { settleMs: 300 });
		await t.until('Analytics drawn', () => !!document.querySelector('.ax-page .kpi'), null, { timeout: 60000, hard: true });
		await t.page.waitForTimeout(2000);
		await scrollAndFocus(t, '.ax-page button');
		const before = analytics + reads.dashboard;
		const r = await idle(t, Number(t.args.pageIdleSeconds || 90), 'analytics');
		await judge(t, 'analytics', r, { updates: analytics + reads.dashboard - before });
	},
};
