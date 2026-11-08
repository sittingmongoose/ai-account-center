// The Status and Scoreboard tabs (docs/status-tab.md, "Shell iframe"): every header keeps its five tabs inside the
// window, the iframe sits exactly in the content area the shell reports (below the header, above the phone tab bar),
// nothing of it covers the tab bar, the theme reaches the page, a page can switch tabs, and a page that does not
// load shows the shell's own note instead of a blank area.
//
// Until the Leptos pages are on the server under test, /app/status and /app/scoreboard are answered here with a
// tiny page that paints its whole box and speaks the message contract. With the real pages present they are used
// as they are; `--set appStub=1` forces the stand-in. Service workers are blocked in these contexts: the worker
// would answer the iframe's navigation itself, past the stub.
import { KNOWN_SMALL } from '../browser-check.config.mjs';

const STUB = `<!doctype html><html><head><meta charset="utf-8"><title>stand-in</title><style>
html,body{margin:0;height:100%}body{background:#c2185b;font:16px system-ui;color:#fff}body[data-mode=dark]{background:#00838f}
p{margin:0;padding:12px}</style></head><body><p>stand-in page</p><script>
addEventListener('message', (e) => {
  if (e.origin !== location.origin || e.source !== parent) return;
  if (e.data && e.data.type === 'aac-theme') { document.body.dataset.mode = e.data.mode; window.__themes = (window.__themes || 0) + 1; }
});
parent.postMessage({ type: 'aac-ready' }, location.origin);
</script></body></html>`;

/** What the content area must be at a window size with no safe areas (Chrome reports none): [top, bottom]. */
function expectedBand(w, h) {
	if (w >= 1280) return [62, h];
	const short = h < 500;
	if (short) return [48, h];
	if (w < 600) return [52, h - 60]; // P: the 52 px row; the tab bar is 52 + max(safe-bottom, 8)
	if (w < 900) return [108, h];
	return [60, h];
}

async function useApp(t, { scoreboardBroken = false } = {}) {
	const real = await t.page.evaluate(async () => {
		try {
			// followed (a manual redirect shows up as an aborted request): the real page answers at its own path
			const r = await fetch('/app/status?embed=1', { credentials: 'same-origin' });
			return r.status === 200 && new URL(r.url).pathname === '/app/status' && /text\/html/.test(r.headers.get('content-type') || '');
		} catch { return false; }
	});
	const stub = !real || t.args.appStub === '1' || t.args.appStub === 1;
	await t.page.route((u) => /^\/app\/(status|scoreboard)$/.test(new URL(u).pathname), (route) => {
		const path = new URL(route.request().url()).pathname;
		// a page that lands outside /app (here a redirect to the shell) must give the "unavailable" note
		if (scoreboardBroken && path === '/app/scoreboard') return route.fulfill({ status: 302, headers: { location: '/' } });
		if (stub) return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: STUB });
		return route.continue();
	});
	t.record('app pages', true, stub ? 'stand-in page (no /app/status on this server, or appStub=1)' : 'the real /app pages');
	return stub;
}

async function frameState(t, page) {
	return t.page.evaluate((p) => {
		const f = [...document.querySelectorAll('iframe.aac-frame')].find((x) => (x.getAttribute('src') || '').startsWith(`/app/${p}`));
		const note = document.getElementById('aac-frame-note');
		const r = f ? f.getBoundingClientRect() : null;
		return {
			frame: r ? { x: r.x, y: r.y, w: r.width, h: r.height } : null,
			visibility: f ? getComputedStyle(f).visibility : null,
			note: note && getComputedStyle(note).display !== 'none' ? note.textContent : null,
			iw: innerWidth, ih: innerHeight, path: location.pathname,
		};
	}, page);
}

/** The iframe is shown and fills exactly the content band; points just outside it hit the canvas. */
async function frameFits(t, label, page) {
	await t.until(`${label}: ${page} iframe shown`, (p) => {
		const f = [...document.querySelectorAll('iframe.aac-frame')].find((x) => (x.getAttribute('src') || '').startsWith(`/app/${p}`));
		return !!f && getComputedStyle(f).visibility === 'visible';
	}, page, { timeout: 20000 });
	const s = await frameState(t, page);
	const [top, bottom] = expectedBand(s.iw, s.ih);
	const f = s.frame;
	const shell = await t.page.evaluate(() => JSON.stringify(window.__aacFrames ? __aacFrames.snapshot() : null));
	t.check(`${label}: iframe in the content area`, !!f && f.x === 0 && f.w === s.iw && f.y === top && f.y + f.h === bottom,
		f ? `iframe ${f.x},${f.y} ${f.w}x${f.h} (bottom ${f.y + f.h}); content area 0,${top} ${s.iw}x${bottom - top}; shell ${shell}` : `no iframe; shell ${shell}`);
	const hits = await t.page.evaluate(([y0, y1, w]) => {
		const at = (x, y) => { const el = document.elementFromPoint(x, y); return el ? el.tagName.toLowerCase() : null; };
		return { above: at(w / 2, y0 - 2), inside: at(w / 2, y0 + 2), insideBottom: at(w / 2, y1 - 2), below: y1 < innerHeight ? at(w / 2, y1 + 2) : 'canvas' };
	}, [top, bottom, s.iw]);
	t.check(`${label}: header and tab bar stay the canvas's`, hits.above === 'canvas' && hits.inside === 'iframe' && hits.insideBottom === 'iframe' && hits.below === 'canvas',
		`above ${hits.above}, inside ${hits.inside}/${hits.insideBottom}, below ${hits.below}`);
	return s;
}

/** The five tabs of whichever bar is on screen: inside the window, in order, not overlapping the controls. */
async function tabsFit(t, label, prefix) {
	const ids = ['home', 'analytics', 'accounts', 'status', 'scoreboard'].map((p) => `${prefix}-${p}`);
	const all = await t.app.probes({ require: [...ids, 'update-apps'] });
	const w = await t.page.evaluate(() => innerWidth);
	const rects = ids.map((id) => all[id]);
	const missing = ids.filter((id, i) => !rects[i]);
	if (missing.length) return t.record(`${label}: five tabs`, false, `missing ${missing.join(', ')}`);
	const inOrder = rects.every((r, i) => i === 0 || r.x >= rects[i - 1].x + rects[i - 1].width - 0.5);
	const inside = rects.every((r) => r.x >= -0.5 && r.x + r.width <= w + 0.5 && r.width >= 24);
	const up = all['update-apps'];
	const last = rects[4];
	// in a header row the tabs end before the controls start (the phone tab bar is its own row)
	const clear = prefix === 'tab' || last.y + last.height <= up.y || last.y >= up.y + up.height || last.x + last.width <= up.x;
	return t.check(`${label}: five tabs fit`, inOrder && inside && clear,
		`${rects.map((r, i) => `${ids[i]} ${Math.round(r.x)}+${Math.round(r.width)}`).join(', ')}; update-apps at ${Math.round(up.x)}; window ${w}`);
}

/** A tap on a tab-bar control. Navigation rewrites the address without ?e2e, and the kit finds controls only on an
 * ?e2e address, so the query goes back first. */
async function tapTab(t, id) {
	await t.page.evaluate(() => { if (!/[?&]e2e\b/.test(location.search)) history.replaceState(history.state, '', `${location.pathname}?e2e`); });
	return t.app.click(id);
}

async function setTheme(t, mode) {
	// the stored last page too: an earlier test's page must not open instead of the one asked for
	await t.page.evaluate((m) => { try { localStorage.setItem('aac-theme', m); localStorage.setItem('aac-last-page', 'home'); } catch {} }, mode);
}

async function stubTheme(t, page) {
	const frame = t.page.frames().find((f) => new URL(f.url()).pathname === `/app/${page}`);
	return frame ? frame.evaluate(() => document.body.dataset.mode || null) : null;
}

const SIZES = [
	// [label, width, height, touch]
	['desktop 1440', 1440, 900, false],
	['desktop 1280', 1280, 800, false],
	['tablet 1160', 1160, 800, true],
	['tablet 1024', 1024, 768, true],
	['tablet 900', 900, 700, true],
	['Duo unfolded 669', 669, 951, true],
	['Duo folded 466', 466, 678, true],
	['iPhone 17 Pro 402', 402, 874, true],
	['phone 320', 320, 568, true],
	['iPhone 17 Pro landscape', 874, 402, true],
	['Duo folded landscape', 678, 466, true],
];

export const tests = {
	async 'tabs fit and the iframe fills the content area'(t) {
		for (const [label, w, h, touch] of SIZES) {
			await t.step(label, async () => {
				await t.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: 1, isMobile: touch, hasTouch: touch, serviceWorkers: 'block' });
				await t.app.signIn();
				await useApp(t);
				await t.app.open({ page: 'status' });
				const phone = w < 600 && h >= 500;
				await tabsFit(t, label, phone ? 'tab' : 'nav');
				await frameFits(t, label, 'status');
				if (phone) {
					// the tab bar's own probes start exactly where the iframe ends
					const tab = await t.app.rect('tab-status');
					const s = await frameState(t, 'status');
					t.check(`${label}: iframe ends at the tab bar`, !!tab && Math.round(tab.y) === s.frame.y + s.frame.h, `tab bar top ${tab && tab.y}, iframe bottom ${s.frame.y + s.frame.h}`);
				}
				await t.app.noProbeOverflow(`${label}: no control past the edge`);
				// every canvas control (the header status line included) still has room and a usable size
				await t.audit(`${label} status`, { controls: ['update-apps'], ignore: KNOWN_SMALL });
			});
		}
	},

	async 'light and dark, desktop and phone'(t) {
		for (const [label, w, h, touch] of [['1440', 1440, 900, false], ['402', 402, 874, true]]) {
			for (const mode of ['light', 'dark']) {
				await t.step(`${label} ${mode}`, async () => {
					await t.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: 1, isMobile: touch, hasTouch: touch, serviceWorkers: 'block' });
					await t.app.signIn();
					const stub = await useApp(t);
					await setTheme(t, mode);
					const page = mode === 'light' ? 'status' : 'scoreboard';
					await t.app.open({ page });
					await frameFits(t, `${label} ${mode}`, page);
					if (stub) {
						let seen = null;
						for (let i = 0; i < 20 && seen !== mode; i++) { seen = await stubTheme(t, page); if (seen !== mode) await t.page.waitForTimeout(150); }
						t.equal(`${label} ${mode}: aac-theme`, seen, mode);
					}
					await t.audit(`${label} ${mode} ${page}`, { controls: ['update-apps'], ignore: KNOWN_SMALL });
					await t.shot(`${page}-${label}-${mode}`, `${page} tab at ${w}x${h} ${mode}: tab selected, the page box exactly between header and ${w < 600 ? 'tab bar' : 'window bottom'}, nothing over the tab bar`);
				});
			}
		}
	},

	async 'switching tabs, theme changes, messages and the unavailable note'(t) {
		await t.newContext({ viewport: { width: 402, height: 874 }, deviceScaleFactor: 1, isMobile: true, hasTouch: true, serviceWorkers: 'block' });
		await t.app.signIn();
		const stub = await useApp(t, { scoreboardBroken: true });
		await setTheme(t, 'light');
		await t.app.open({ page: 'accounts' });
		await tapTab(t, 'tab-status');
		await frameFits(t, 'tapped Status', 'status');
		t.equal('tapped Status: path', (await frameState(t, 'status')).path, '/status');
		if (stub) {
			// a live theme change reaches the page
			await t.page.evaluate(() => window.ccsDashboardAction('theme', 'dark'));
			let seen = null;
			for (let i = 0; i < 20 && seen !== 'dark'; i++) { seen = await stubTheme(t, 'status'); if (seen !== 'dark') await t.page.waitForTimeout(150); }
			t.equal('live theme change: aac-theme dark', seen, 'dark');
			await t.page.evaluate(() => window.ccsDashboardAction('theme', 'light'));
			// the page links to another tab
			const frame = t.page.frames().find((f) => new URL(f.url()).pathname === '/app/status');
			await frame.evaluate(() => parent.postMessage({ type: 'aac-navigate', page: 'home' }, location.origin));
			await t.until('aac-navigate: Home opened', () => location.pathname === '/', null, { timeout: 5000 });
			t.equal('aac-navigate: iframe hidden', (await frameState(t, 'status')).visibility, 'hidden');
			// junk and wrong types are ignored
			await frame.evaluate(() => { parent.postMessage({ type: 'aac-navigate', page: 'nowhere' }, location.origin); parent.postMessage('aac-ready', location.origin); });
			await t.page.waitForTimeout(300);
			t.equal('junk messages ignored', (await frameState(t, 'status')).path, '/');
		}
		// returning is instant: the same iframe, already loaded
		await tapTab(t, 'tab-status');
		await frameFits(t, 'back to Status', 'status');
		t.equal('one Status iframe', await t.page.evaluate(() => document.querySelectorAll('iframe.aac-frame[src^="/app/status"]').length), 1);
		// Scoreboard's page lands outside /app (a redirect to the shell): the shell's note, never a blank area
		await tapTab(t, 'tab-scoreboard');
		await t.until('Scoreboard unavailable note', () => {
			const n = document.getElementById('aac-frame-note');
			return !!n && getComputedStyle(n).display !== 'none' && /Scoreboard page unavailable/.test(n.textContent);
		}, null, { timeout: 10000 });
		const s = await frameState(t, 'scoreboard');
		t.equal('broken Scoreboard iframe stays hidden', s.visibility, 'hidden');
		t.equal('the shell inside the iframe did not start a dashboard', await t.page.evaluate(() => {
			const f = document.querySelector('iframe.aac-frame[src^="/app/scoreboard"]');
			try { return f.contentDocument.querySelector('#loading')?.textContent ?? null; } catch { return 'unreadable'; }
		}), 'This page is unavailable here.');
		await t.shot('scoreboard-unavailable-402', 'Scoreboard page unavailable note with Try again, centred in the content area at 402x874');
		// sign-out removes the iframes
		await t.page.evaluate(() => window.ccsDashboardAction('logout', ''));
		await t.until('signed out: iframes removed', () => document.querySelectorAll('iframe.aac-frame').length === 0, null, { timeout: 10000 });
	},
};
