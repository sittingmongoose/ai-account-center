// Page loads in the app frame: every tab fetches its own data and none loads or waits on the Slint dashboard (the hidden
// `/?content=1` iframe, bridge.js and its 36 MB wasm), and how long each takes to show.
//
//   browser-check run web-dashboard/tests/browser/checks --grep 'page load'
//   browser-check run web-dashboard/tests/browser/checks --grep 'load timing' --set home=<fixture home> --set samples=3
//   (--set pages=home,accounts picks the timed tabs; the default is home, accounts, analytics and scoreboard)
//
// Real server, no stubs: route() turns the HTTP cache off, so a stubbed run can't time a warm load. For numbers worth
// comparing, give it a prepared home with real-sized synthetic data (usage logs, a full runs folder). The timings are
// written to <run folder>/page-load.json (and to --set out=<file>); they are recorded, not judged.
import fs from 'node:fs';
import path from 'node:path';

const VIEWPORT = { width: 1440, height: 900 };
const SLINT = /ccs_account_dashboard|\/bridge\.js(\?|$)/;

// What each page shows first (anything of its own, a loading state included) and in full (its data drawn).
const MARKS = `(() => {
	if (window.top !== window) return;
	const marks = (window.__loadMarks = {});
	const q = (s) => document.querySelector(s);
	const shown = (s) => { const el = q('.route:not([hidden]) ' + s); return !!el && el.getClientRects().length > 0; };
	const preds = {
		frame: () => window.__aacFrame === true,
		signin: () => !!q('.signin-page'),
		shell: () => !!q('.shell'),
		'home-first': () => shown('.page.home'),
		// Home's data drawn: its sections and cards, or its words for none; never its "Reading your accounts" state
		'home-full': () => { const p = q('.route:not([hidden]) .page.home'); return !!p && (!!p.querySelector('.hsec, .hcards, .hcard') || (!!p.querySelector('.hstate') && !/Reading your accounts/.test(p.querySelector('.hstate').textContent))); },
		'accounts-first': () => shown('.ac-page'),
		'accounts-full': () => !!q('.route:not([hidden]) .ac-page .ac-slot'),
		'analytics-first': () => shown('.ax-page'),
		'analytics-data': () => { const p = q('.ax-page'); return !!p && p.querySelectorAll('.kpi').length >= 5; },
		'analytics-full': () => { const p = q('.ax-page'); return !!p && p.querySelectorAll('.kpi').length >= 5 && !!p.querySelector('.trend-svg path[d^="M"]') && p.querySelectorAll('.heat-grid .hc').length === 168; },
		'scoreboard-first': () => shown('.sb-head') || shown('.page-title'),
		'scoreboard-full': () => !!q('.sb-matrix tbody tr') || !!q('.picks .pick') || !!q('.sb-unavail'),
	};
	addEventListener('message', (e) => {
		const d = e.data;
		if (!d || d.type !== 'aac-content') return;
		if (d.kind === 'ready' && !('bridge-ready' in marks)) marks['bridge-ready'] = performance.now();
		if (d.kind === 'view' && d.key === 'analytics' && !('bridge-analytics-view' in marks)) marks['bridge-analytics-view'] = performance.now();
	});
	const tick = () => {
		for (const [k, f] of Object.entries(preds)) if (!(k in marks)) { try { if (f()) marks[k] = performance.now(); } catch {} }
		requestAnimationFrame(tick);
	};
	requestAnimationFrame(tick);
})();`;

/** Every resource the page and its same-origin frames loaded, on the top page's clock (ms from navigation start). */
const RESOURCES = () => {
	const rows = [];
	const add = (win, frame, offset) => {
		for (const e of win.performance.getEntriesByType('resource')) {
			const u = new URL(e.name);
			rows.push({ frame, path: u.pathname + (u.pathname.startsWith('/api/') ? u.search : ''), start: Math.round(e.startTime + offset), end: Math.round(e.responseEnd + offset), ms: Math.round(e.responseEnd - e.startTime), transfer: e.transferSize, bytes: e.encodedBodySize, decoded: e.decodedBodySize });
		}
	};
	add(window, 'top', 0);
	for (const f of document.querySelectorAll('iframe')) {
		try { add(f.contentWindow, f.className || 'iframe', f.contentWindow.performance.timeOrigin - performance.timeOrigin); } catch { /* another origin */ }
	}
	const nav = performance.getEntriesByType('navigation')[0];
	const fcp = performance.getEntriesByName('first-contentful-paint')[0];
	return {
		marks: Object.fromEntries(Object.entries(window.__loadMarks || {}).map(([k, v]) => [k, Math.round(v)])),
		nav: nav ? { html: Math.round(nav.responseEnd), dcl: Math.round(nav.domContentLoadedEventEnd), load: Math.round(nav.loadEventEnd) } : null,
		fcp: fcp ? Math.round(fcp.startTime) : null,
		slintFrame: !!document.querySelector('iframe.slint-frame'),
		resources: rows.sort((a, b) => a.start - b.start),
	};
};

/** Signs in from a plain JSON page, so nothing of the app (and no wasm) is in the cache before the measured visit. */
async function signInBare(t) {
	await t.goto('/api/health', { settleMs: 0 });
	await t.app.signIn();
}

/** The analytics the server holds for the fixture logs: read until its totals are in (a cold server answers "loading"). */
async function warmServer(t) {
	return t.page.evaluate(async () => {
		const t0 = performance.now();
		const reads = [];
		for (let i = 0; i < 60; i++) {
			const s = performance.now();
			const r = await fetch('/api/accounts/analytics?platform=mac&range=7d&provider=all&account=all', { credentials: 'same-origin' });
			const body = await r.text();
			let json = null;
			try { json = JSON.parse(body); } catch {}
			reads.push({ ms: Math.round(performance.now() - s), bytes: body.length, status: json?.activity?.status ?? null, totals: !!json?.activity?.totals });
			if (json?.activity?.totals && json.activity.status !== 'loading' && json.activity.refreshing !== true) break;
			await new Promise((ok) => setTimeout(ok, 2000));
		}
		const s = performance.now();
		const sb = await fetch('/api/scoreboard', { credentials: 'same-origin' });
		const sbBody = await sb.text();
		return { analyticsReads: reads, convergedMs: Math.round(performance.now() - t0), scoreboardFirst: { ms: Math.round(performance.now() - s), bytes: sbBody.length } };
	});
}

async function visit(t, page) {
	await t.page.goto(new URL(`/app/${page}`, t.run.baseURL || t.page.url()).href);
	await t.page.waitForFunction((p) => window.__loadMarks && (`${p}-full` in window.__loadMarks), page, { timeout: 90000 }).catch(() => {});
	// late resources (the bridge's own reads) land within a second
	await t.page.waitForTimeout(1500);
	return t.page.evaluate(RESOURCES);
}

const summarize = (r) => {
	const api = r.resources.filter((x) => x.path.startsWith('/api/'));
	const wasm = r.resources.filter((x) => x.path.endsWith('.wasm'));
	return {
		marks: r.marks, fcp: r.fcp, nav: r.nav, slintFrame: r.slintFrame,
		wasm: wasm.map(({ frame, path: p, start, end, transfer }) => ({ frame, path: p.replace(/^.*\//, ''), start, end, transfer })),
		api: api.map(({ frame, path: p, start, end, ms, transfer, bytes }) => ({ frame, path: p, start, end, ms, transfer, bytes })),
		// the frame's own start: what loads before (and beside) its wasm
		boot: r.resources.filter((x) => x.frame === 'top' && !x.path.startsWith('/api/')).slice(0, 40).map(({ path: p, start, end, transfer }) => ({ path: p, start, end, transfer })),
		bytes: r.resources.reduce((n, x) => n + (x.transfer || 0), 0),
		requests: r.resources.length,
	};
};

export const tests = {
	// First, so the kit's server has just started: run it alone (--grep 'cold server') for a server-start number.
	async 'load timing: analytics on a server that just started'(t) {
		await t.newContext({ viewport: VIEWPORT, serviceWorkers: 'block' });
		await t.context.addInitScript(MARKS);
		await signInBare(t);
		const reads = [];
		t.page.on('response', async (res) => {
			if (new URL(res.url()).pathname !== '/api/accounts/analytics') return;
			try { const a = (await res.json())?.activity; reads.push({ status: a?.status ?? null, totals: !!a?.totals }); } catch { /* the page moved on */ }
		});
		const r = summarize(await visit(t, 'analytics'));
		const out = { at: new Date().toISOString(), firstReads: reads, cold: r };
		fs.writeFileSync(path.join(t.run.dir, 'page-load-cold-server.json'), JSON.stringify(out, null, 2));
		if (t.args.out) fs.writeFileSync(path.resolve(t.args.out).replace(/\.json$/, '') + '-cold-server.json', JSON.stringify(out, null, 2));
		await t.check('analytics drawn in full on a server that just started', 'analytics-full' in r.marks, JSON.stringify(r.marks), {
			note: `first read ${reads[0] ? `${reads[0].status}${reads[0].totals ? ' with totals' : ', no totals'}` : 'not seen'}; full at ${r.marks['analytics-full']} ms; ${r.api.filter((a) => a.path.startsWith('/api/accounts/analytics')).length} analytics reads`,
		});
	},

	async 'page load: no tab loads or waits on the dashboard bridge, each asks for its own data'(t) {
		// what each tab reads itself, from the top window
		const WANT = { home: '/api/accounts/dashboard', accounts: '/api/accounts/registry', analytics: '/api/accounts/analytics', scoreboard: '/api/scoreboard', status: '/api/status' };
		for (const page of Object.keys(WANT)) {
			await t.newContext({ viewport: VIEWPORT, serviceWorkers: 'block' });
			await signInBare(t);
			const slint = [];
			const api = [];
			t.page.on('request', (req) => {
				const u = new URL(req.url());
				if (SLINT.test(u.pathname)) slint.push(u.pathname);
				if (Object.values(WANT).includes(u.pathname)) api.push({ path: u.pathname, top: req.frame() === t.page.mainFrame() });
			});
			await t.page.goto(new URL(`/app/${page}`, t.page.url()).href);
			const want = WANT[page];
			await t.until(`${page}: its data is asked for`, (p) => !!document.querySelector('.shell') && performance.getEntriesByType('resource').some((e) => new URL(e.name).pathname === p), want, { timeout: 30000 });
			// the frame keeps going: a bridge started late would still show up here
			await t.page.waitForTimeout(2500);
			await t.check(`${page}: the page itself asks for ${want}`, api.some((r) => r.path === want && r.top), JSON.stringify(api));
			await t.equal(`${page}: no Slint dashboard is loaded (no bridge.js, no dashboard wasm)`, slint, []);
			await t.check(`${page}: no dashboard iframe`, await t.page.evaluate(() => !document.querySelector('iframe.slint-frame') && !document.querySelector('iframe[src*="content=1"]')));
		}
		// a visit to every tab in one session: still no bridge, and Home and Accounts drawn from the frame's own reads
		await t.newContext({ viewport: VIEWPORT, serviceWorkers: 'block' });
		await signInBare(t);
		const slint = [];
		t.page.on('request', (req) => { const u = new URL(req.url()); if (SLINT.test(u.pathname)) slint.push(u.pathname); });
		await t.page.goto(new URL('/app/home', t.page.url()).href);
		for (const tab of ['accounts', 'analytics', 'status', 'scoreboard', 'home']) {
			await t.page.locator(`a[href="/app/${tab}"]:visible`).first().click();
			await t.until(`${tab} shown`, (p) => location.pathname === `/app/${p}`, tab, { timeout: 15000 });
		}
		await t.until('Home drawn from its own read', () => { const p = document.querySelector('.route:not([hidden]) .page.home'); return !!p && !/Reading your accounts/.test(p.textContent); }, null, { timeout: 30000 });
		await t.page.waitForTimeout(1500);
		await t.equal('every tab in one session: no Slint dashboard is loaded', slint, []);
	},

	async 'load timing: home, accounts, analytics and scoreboard, cold and warm'(t) {
		const samples = Math.max(1, +t.args.samples || 1);
		const pages = String(t.args.pages || 'home,accounts,analytics,scoreboard').split(',').filter(Boolean);
		await t.newContext({ viewport: VIEWPORT, serviceWorkers: 'block' });
		await signInBare(t);
		const server = await warmServer(t);
		const out = { at: new Date().toISOString(), viewport: VIEWPORT, serviceWorkers: 'blocked', server, runs: [] };
		for (let i = 0; i < samples; i++) {
			for (const page of pages) {
				// cold: a new browser context (empty HTTP and code cache), signed in from a JSON page
				await t.newContext({ viewport: VIEWPORT, serviceWorkers: 'block' });
				await t.context.addInitScript(MARKS);
				await signInBare(t);
				const cold = summarize(await visit(t, page));
				// warm: the same context opens the page again
				const warm = summarize(await visit(t, page));
				out.runs.push({ page, sample: i + 1, cold, warm });
				await t.check(`${page} sample ${i + 1}: cold page drawn in full`, `${page}-full` in cold.marks, JSON.stringify(cold.marks));
				await t.check(`${page} sample ${i + 1}: warm page drawn in full`, `${page}-full` in warm.marks, JSON.stringify(warm.marks));
			}
		}
		const file = path.join(t.run.dir, 'page-load.json');
		fs.writeFileSync(file, JSON.stringify(out, null, 2));
		if (t.args.out) fs.writeFileSync(path.resolve(t.args.out), JSON.stringify(out, null, 2));
		const line = (r) => `${r.page} cold first ${r.cold.marks[`${r.page}-first`]} full ${r.cold.marks[`${r.page}-full`]} · warm first ${r.warm.marks[`${r.page}-first`]} full ${r.warm.marks[`${r.page}-full`]} ms`;
		await t.check('timings written', true, '', { note: out.runs.map(line).join('; ') });
	},
};
