// browser-check config for the AAC web dashboard (the Leptos app frame, compiled to WebAssembly).
// Serves this checkout's dist/ (dist/ccs.js + dist/ui) with an empty temporary CCS_HOME (never the real one); pages
// open at /app/<tab>?e2e, which keeps the controllers' last views on window for the checks to read.
//   browser-check run web-dashboard/tests/browser/checks
//   browser-check run web-dashboard/tests/browser/checks --set home=/tmp/fixture-home   (a prepared CCS_HOME, copied)
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

export default (ctx) => {
	const root = ctx.args.repo ? path.resolve(ctx.args.repo) : path.resolve(ctx.configDir, '..', '..', '..');
	const cli = path.join(root, 'dist', 'ccs.js');
	if (!fs.existsSync(cli) || !fs.existsSync(path.join(root, 'dist', 'ui', 'app', 'index.html'))) {
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
			// Opt-in real data for Status comparisons: `--set reports=<copy of the reports share>` (and `registry=<copy of
			// the pm-mail registry>`) are copied into this run's temporary home, so the server reads and writes only the
			// copy (it writes reader receipts beside the reports). `--set realPins=1` below adds the real T3 pins.
			if (ctx.args.reports) {
				fs.cpSync(path.resolve(String(ctx.args.reports)), reports, { recursive: true });
				if (ctx.args.registry) fs.cpSync(path.resolve(String(ctx.args.registry)), path.join(reports, 'pm-mail-registry'), { recursive: true });
			}
			// The other machines' pins come over ssh. Its config's `Include ~/...` lines resolve against HOME, so the real
			// ~/.ssh is linked into the temporary home; nothing else of the real home is.
			if (ctx.args.realPins && !fs.existsSync(path.join(home, '.ssh'))) fs.symlinkSync(path.join(os.homedir(), '.ssh'), path.join(home, '.ssh'));
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
			env: { CCS_HOME: s.home, HOME: s.home, USERPROFILE: s.home, APPDATA: path.join(s.home, 'AppData', 'Roaming'), LOCALAPPDATA: path.join(s.home, 'AppData', 'Local'), XDG_CONFIG_HOME: path.join(s.home, '.config'), XDG_DATA_HOME: path.join(s.home, '.local', 'share'), XDG_STATE_HOME: path.join(s.home, '.local', 'state'), XDG_CACHE_HOME: path.join(s.home, '.cache'), CODEX_HOME: path.join(s.home, '.codex'), CLAUDE_CONFIG_DIR: path.join(s.home, '.claude'), AAC_AGENT_REPORTS_DIR: s.reports, AAC_PM_MAIL_REGISTRY_DIR: path.join(s.reports, 'pm-mail-registry'), AAC_STATUS_DOORBELL: 'off',
				// `--set realPins=1`: this machine's real T3 pins, opened read-only by the status module (the other machines'
				// pins come over ssh, set up above).
				...(ctx.args.realPins ? { AAC_T3_STATE_DB: path.join(os.homedir(), '.t3', 'userdata', 'statev2.sqlite') } : {}) }
		}),
		async teardown(_, s) {
			if (!s || !s.home) return;
			// The realPins link to ~/.ssh goes first, by itself, so removing the home can never reach the real folder.
			const ssh = path.join(s.home, '.ssh');
			if (fs.lstatSync(ssh, { throwIfNoEntry: false })?.isSymbolicLink()) fs.unlinkSync(ssh);
			if (!ctx.args.keep) fs.rmSync(s.home, { recursive: true, force: true });
		},
		viewport: { width: 1920, height: 1080 },
		// Signed out on an empty CCS_HOME, the update status call answers 401 by design
		consoleErrors: { fail: true, allow: [/status of 401 .*\/api\/app-updates\/status/, /status of 409 .*\/activate\b/] }, // 409: a Codex switch asks for its approval first (overlay-close drives it)
		// A video with preload="metadata" stops its own request once it has the metadata (net::ERR_ABORTED).
		failedRequests: { fail: false, allow: [/\/api\/status\/files\/.*\.(mp4|webm)\b/] },
		// Each test starts with no API stubs: accounts-clicks' synthetic accounts must not reach smoke's empty Home, even after a test that threw
		async beforeEach(t) { await t.page.unrouteAll({ behavior: 'ignoreErrors' }); },
		helpers: {
			// Sign in for real: on an unconfigured home, loopback setup turns dashboard sign-in on and starts a session
			// (POST /api/auth/setup); afterwards the same credentials sign in. Cookies land in this page's context.
			async signIn(t, { username = 'labadmin', password = 'Lab-Correct-Horse-Battery-9' } = {}) {
				// fetch from the page itself: the cookie must land in the page's own jar (page.request doesn't share it
				// with the default context of a CDP-attached browser)
				if (!t.page.url().startsWith(t.run.baseURL)) await t.goto('/api/health', { settleMs: 0 });
				const r = await t.page.evaluate(async (cred) => {
					const check = await (await fetch('/api/auth/check', { credentials: 'same-origin' })).json();
					if (check.authenticated) return { ok: true, status: 200, body: 'already signed in' };
					const setup = await (await fetch('/api/auth/setup', { credentials: 'same-origin' })).json();
					const res = await fetch(setup.configured ? '/api/auth/login' : '/api/auth/setup', { method: 'POST', credentials: 'same-origin', headers: { 'content-type': 'application/json' }, body: JSON.stringify(cred) });
					return { ok: res.ok, status: res.status, body: (await res.text()).slice(0, 200) };
				}, { username, password });
				return t.check('signed in', r.ok, `setup/login answered ${r.status}: ${r.body}`, { hard: true });
			},
			// Open a tab of the app frame (home, analytics, accounts, status, scoreboard) with ?e2e; ready when the frame is
			// drawn signed in (its shell) or, with signIn: false, when the sign-in page is.
			async open(t, { page = 'home', signIn = true, query = '' } = {}) {
				if (signIn) await t.app.signIn();
				await t.goto(`/app/${page}?e2e${query ? '&' + query : ''}`, { settleMs: 300 });
				if (signIn) await t.until(`${page} ready (signed in)`, () => !!document.querySelector('.shell') && !document.querySelector('.signin-page'), null, { timeout: 60000, hard: true });
				else await t.until(`${page} ready (sign-in page)`, () => !!document.querySelector('.signin-page input[name="username"]'), null, { timeout: 60000, hard: true });
				return true;
			},
			// Synthetic API answers (shape them like src/web-server/services/*-types.ts; made-up names only)
			async stub(t, apiPath, json) { await t.page.route((u) => new URL(u).pathname === apiPath, (r) => r.fulfill({ json })); },
			// Details reads each account's reset inventory (GET /api/accounts/<id>/reset-claims). A server without the
			// native reset backend (the VM) answers 503; this answers as a backend with nothing to report ('unavailable'),
			// so Details draws the dashboard's own reset rows, as on that 503, without a console error. For checks of
			// synthetic accounts whose subject isn't resets.
			async noResets(t) {
				const answer = { status: 'unavailable', reason: 'Fixture: no reset backend.', grants: [], history: [] };
				await t.page.route((u) => /^\/api\/accounts\/[^/]+\/reset-claims$/.test(new URL(u).pathname), (r) =>
					r.request().method() === 'GET' ? r.fulfill({ json: answer }) : r.continue());
			}
		}
	};
};
