// browser-check config for the AAC web dashboard (Slint 1.18 compiled to WebAssembly, drawn into one canvas).
// Serves this checkout's dist/ (dist/ccs.js + dist/ui) with an empty temporary CCS_HOME (never the real one), opens
// /?e2e so every ProbeMark reports its rectangle, and checks the canvas through those rectangles and its pixels.
//   browser-check run web-dashboard/tests/browser/checks
//   browser-check run web-dashboard/tests/browser/checks --set home=/tmp/fixture-home   (a prepared CCS_HOME, copied)
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

// Probes that are things to click or type into. The rest are headings, hints, bars, panels and marks, which t.audit must not
// judge as controls (a 21 px heading isn't a target that is too small). pw-others is left out: the "Also sign out other
// browsers" box shows only while a new password is confirmed, and while hidden its probe is a 16 px sliver over Sign out.
// Known finding, 2026-11-01 (browser-check 2.0's first audit of Accounts): above the phone tier each usage or account row's show
// and tray toggles are 28x16 (36x22 in the F tiers) with centres 21 px apart, under the 24 px WCAG 2.2 SC 2.5.8 asks for. The
// audits leave them out by name until the row gives them room; every other control on the page is audited.
export const KNOWN_SMALL = [/^acct-(show|tray):/];

const CONTROL = /^(signin-(user|pass|submit|remember)$|update-apps$|install-app$|status-(line|block)$|home-legend$|analytics-provider$|included-usage$|(tab|nav)-(home|analytics|accounts|status|scoreboard)$|(show|tray|add|add-key|row|acct-show|acct-tray|remove|signin-again|signin|session-signin|recheck):|recheck$|logout$|logsource-add$|cleanup-now$|all-(ask|do)$|network$|others-out$|pw-(cur|new|conf|toggle|submit|cancel)$)/;

export default (ctx) => {
	const root = ctx.args.repo ? path.resolve(ctx.args.repo) : path.resolve(ctx.configDir, '..', '..', '..');
	const cli = path.join(root, 'dist', 'ccs.js');
	if (!fs.existsSync(cli) || !fs.existsSync(path.join(root, 'dist', 'ui', 'index.html'))) {
		throw new Error(`no built dashboard in ${root}/dist: run bun run build:server, and build or copy dist/ui (see web-dashboard/tests/browser/README.md)`);
	}
	if (ctx.fleet) throw new Error('--host: the dashboard listens on 127.0.0.1 and setup is loopback-only, so a browser on another machine can\'t use it. Run from a checkout on that machine.');
	return {
		name: 'aac',
		async setup() {
			const home = path.join(ctx.dir, 'home');
			if (ctx.args.home) {
				const src = path.resolve(ctx.args.home);
				const inside = (dir, root) => { const r = path.relative(root, dir); return !r.startsWith('..') && !path.isAbsolute(r); };
				if (inside(src, os.homedir()) && !inside(src, os.tmpdir())) throw new Error('home= must be a fixture folder outside your real home (temp folders are fine)');
				fs.cpSync(src, home, { recursive: true });
			} else fs.mkdirSync(home, { recursive: true });
			// The Status tab reads agent reports from the share by default. Here it reads a fixture in the temporary home
			// (scripts/status-fixture.mjs: 9 open cards, 2 answered, threads, streams and option media), its answers go to
			// that home's store, and the doorbell is off, so a test answer never reaches a real thread.
			const reports = path.join(home, 'agent-reports');
			if (!fs.existsSync(path.join(reports, 'v1', 'writers'))) {
				const { writeStatusFixture } = await import(pathToFileURL(path.join(root, 'scripts', 'status-fixture.mjs')).href);
				writeStatusFixture(reports, { ccsDir: path.join(home, '.ccs'), withKinds: true });
				// The Model Scoreboard reads runs and the catalog from the same folder.
				const { writeScoreboardFixture } = await import(pathToFileURL(path.join(root, 'scripts', 'scoreboard-fixture.mjs')).href);
				writeScoreboardFixture(reports);
			}
			return { home, reports, port: +ctx.args.port || (await ctx.freePort()) };
		},
		// CCS_HOME alone is not enough: some provider collectors read the real home (CLI logins, usage files, ssh keys
		// for other machines). Every home-like variable points at the temporary folder, so no real account shows up.
		server: (_, s) => ({
			command: [process.execPath, cli, 'dashboard', '--no-open', '--host', '127.0.0.1', '--port', String(s.port)], cwd: root, url: `http://127.0.0.1:${s.port}/`, timeoutMs: +ctx.args.serverTimeout || 240000, // node_modules on the share can take 90 s to load
			env: { CCS_HOME: s.home, HOME: s.home, USERPROFILE: s.home, APPDATA: path.join(s.home, 'AppData', 'Roaming'), LOCALAPPDATA: path.join(s.home, 'AppData', 'Local'), XDG_CONFIG_HOME: path.join(s.home, '.config'), XDG_DATA_HOME: path.join(s.home, '.local', 'share'), XDG_STATE_HOME: path.join(s.home, '.local', 'state'), XDG_CACHE_HOME: path.join(s.home, '.cache'), CODEX_HOME: path.join(s.home, '.codex'), CLAUDE_CONFIG_DIR: path.join(s.home, '.claude'), AAC_AGENT_REPORTS_DIR: s.reports, AAC_PM_MAIL_REGISTRY_DIR: path.join(s.reports, 'pm-mail-registry'), AAC_STATUS_DOORBELL: 'off' }
		}),
		async teardown(_, s) { if (s && s.home && !ctx.args.keep) fs.rmSync(s.home, { recursive: true, force: true }); },
		viewport: { width: 1920, height: 1080 },
		// Signed out on an empty CCS_HOME, the update status call answers 401 by design
		consoleErrors: { fail: true, allow: [/status of 401 .*\/api\/app-updates\/status/] },
		// A video with preload="metadata" stops its own request once it has the metadata (net::ERR_ABORTED).
		failedRequests: { fail: false, allow: [/\/api\/status\/files\/.*\.(mp4|webm)\b/] },
		// The canvas's controls, for t.audit, t.clickControl and t.scrollToControl (browser-check 2.0): the interactive probes on
		// this page, in CSS pixels. Signed in, the sign-in panel's probes still report while it is hidden, so they are left out.
		// null off an ?e2e page, where nothing reports.
		async controls(t) {
			if (!/[?&]e2e\b/.test(t.page.url())) return null;
			const all = await t.app.probes();
			const signedIn = await t.page.evaluate(() => !!(window.__aacLastAuth && __aacLastAuth.signedIn));
			return Object.entries(all).filter(([id]) => CONTROL.test(id) && !(signedIn && id.startsWith('signin-'))).map(([name, r]) => ({ name, ...r }));
		},
		// probe_tick reports over a few frames: a required control is asked for again for this long before it counts as missing
		controlsWaitMs: 4000,
		// Each test starts with no API stubs: accounts-clicks' synthetic accounts must not reach smoke's empty Home, even after a test that threw
		async beforeEach(t) { await t.page.unrouteAll({ behavior: 'ignoreErrors' }); },
		helpers: {
			// Sign in for real: on an unconfigured home, loopback setup turns dashboard sign-in on and starts a session
			// (POST /api/auth/setup); afterwards the same credentials sign in. Cookies land in this page's context.
			async signIn(t, { username = 'labadmin', password = 'Lab-Correct-Horse-Battery-9' } = {}) {
				// fetch from the page itself: the cookie must land in the page's own jar (page.request doesn't share it
				// with the default context of a CDP-attached browser)
				if (!t.page.url().startsWith(t.run.baseURL)) await t.goto('/?e2e', { settleMs: 200 });
				const r = await t.page.evaluate(async (cred) => {
					const check = await (await fetch('/api/auth/check', { credentials: 'same-origin' })).json();
					if (check.authenticated) return { ok: true, status: 200, body: 'already signed in' };
					const setup = await (await fetch('/api/auth/setup', { credentials: 'same-origin' })).json();
					const res = await fetch(setup.configured ? '/api/auth/login' : '/api/auth/setup', { method: 'POST', credentials: 'same-origin', headers: { 'content-type': 'application/json' }, body: JSON.stringify(cred) });
					return { ok: res.ok, status: res.status, body: (await res.text()).slice(0, 200) };
				}, { username, password });
				return t.check('signed in', r.ok, `setup/login answered ${r.status}: ${r.body}`, { hard: true });
			},
			// Load a page (home, analytics, accounts) with probes on; ready when the bridge has handed Slint its auth view.
			// Desktop has no tab probes (they exist in the narrow tab bar only), so pages are reached by URL (?view=).
			async open(t, { page = 'home', signIn = true, query = '' } = {}) {
				if (signIn) await t.app.signIn();
				await t.goto(`/?e2e${page === 'home' ? '' : '&view=' + page}${query ? '&' + query : ''}`, { settleMs: 500 });
				if (signIn) await t.until(`${page} ready (signed in)`, () => !!(window.__aacLastAuth && __aacLastAuth.signedIn), null, { timeout: 60000, hard: true });
				// Signed out, the sign-in panel's probes are the signal. probe_tick reports a frame later and
				// __aacProbeNow clears the map, so ask at most every 500 ms.
				else await t.until(`${page} ready (sign-in panel)`, () => {
					if (window.__aacProbe && __aacProbe.has('signin-user')) return true;
					if (window.__aacProbeNow && (!window.__bcAsked || Date.now() - window.__bcAsked > 500)) { window.__bcAsked = Date.now(); __aacProbeNow(); }
					return false;
				}, null, { timeout: 60000, hard: true });
				await t.page.waitForTimeout(800);
				return true;
			},
			// Every probed control on the current page: { id: { x, y, width, height } } in CSS pixels
			// probe_tick reports over the next frames, a section's probes some frames after the rest: read until the set has held
			// still for 600 ms and has every id in require (max 4 s). Two reads 150 ms apart ended before a late probe reported
			// ('probes missing: home-legend' on phone Home).
			async probes(t, { require = [] } = {}) {
				await t.page.evaluate(() => window.__aacProbeNow && __aacProbeNow());
				let last = -1, same = 0;
				for (let waited = 0; waited < 4000; waited += 150) {
					await t.page.waitForTimeout(150);
					const { size, has } = await t.page.evaluate((ids) => ({ size: window.__aacProbe ? __aacProbe.size : 0, has: ids.every((id) => window.__aacProbe && __aacProbe.has(id)) }), require);
					same = size > 0 && size === last ? same + 1 : 0;
					if (same >= 4 && has) break;
					last = size;
				}
				return t.page.evaluate(() => Object.fromEntries([...(window.__aacProbe || new Map())].map(([k, r]) => [k, { x: r.x, y: r.y, width: r.w, height: r.h }])));
			},
			async rect(t, id) { return (await t.app.probes())[id] || null; },
			// A real mouse click at a control's centre, through browser-check: it waits for a late probe, wheels a control above or
			// below the window into view first, and refuses one a DOM element covers
			async click(t, id) {
				const ok = await t.clickControl(id, { hover: 60 });
				await t.page.waitForTimeout(400);
				return ok;
			},
			// A probed control is reported, sized and inside the viewport
			async probeVisible(t, id, name) {
				const r = await t.app.rect(id);
				const [w, h] = await t.page.evaluate(() => [innerWidth, innerHeight]);
				const ok = !!r && r.width >= 1 && r.height >= 1 && r.x >= -1 && r.y >= -1 && r.x + r.width <= w + 1 && r.y + r.height <= h + 1;
				return t.record(name || `${id} visible`, ok, r ? `at ${Math.round(r.x)},${Math.round(r.y)} ${Math.round(r.width)}x${Math.round(r.height)} in a ${w}x${h} viewport` : `no probe "${id}" reported`, { target: r && r.width >= 1 ? r : null });
			},
			// Canvas layout without a DOM: no probed control starts left of 0 or ends past the right edge. An empty or
			// incomplete probe map fails (probes that stopped reporting must not read as "no overflow").
			// A probe parked wholly past the edge (the F-tier SidePanel sits at x = window width while closed, and a probe reports
			// even while hidden) is left out and named in the note. ignore: ids to leave out as well, such as an overlay while it opens.
			async noProbeOverflow(t, name = 'no control past the edge', { require = ['update-apps'], ignore = [] } = {}) {
				const all = await t.app.probes({ require });
				const w = await t.page.evaluate(() => innerWidth);
				const missing = require.filter((id) => !all[id]);
				if (!Object.keys(all).length || missing.length) return t.record(name, false, `probes missing: ${Object.keys(all).length ? missing.join(', ') : 'none reported (is ?e2e on?)'}`);
				const skipped = new Set(ignore);
				// Parked wholly past the left or right edge (a closed side panel) is not overflow: only what reaches into the window is,
				// as browser-check's own canvas control check counts it
				const parked = Object.entries(all).filter(([id, r]) => !skipped.has(id) && r.width > 0 && (r.x >= w || r.x + r.width <= 0)).map(([id]) => id);
				const bad = Object.entries(all).filter(([id, r]) => !skipped.has(id) && !parked.includes(id) && r.width > 0 && (r.x < -1 || r.x + r.width > w + 1)).map(([id, r]) => `${id} spans ${Math.round(r.x)}..${Math.round(r.x + r.width)} of ${w}`);
				return t.record(name, bad.length === 0, bad.slice(0, 8).join('; '), { note: `${Object.keys(all).length} probes${parked.length ? `; parked past the edge: ${parked.join(', ')}` : ''}` });
			},
			// Synthetic API answers (shape them like src/web-server/services/*-types.ts; made-up names only)
			async stub(t, apiPath, json) { await t.page.route((u) => new URL(u).pathname === apiPath, (r) => r.fulfill({ json })); }
		}
	};
};
