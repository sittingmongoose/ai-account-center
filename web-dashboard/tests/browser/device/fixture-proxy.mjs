// Realistic data for device checks (DEVICE-RECIPE.md): a proxy in front of a test dashboard that answers Home's
// accounts, Analytics, Status and the Scoreboard from made-up fixtures and forwards everything else (sign-in, the frame's pages,
// the socket) to the real server unchanged. Test data only: invented names, never a real account.
// Home's accounts are home-fixture.mjs (Claude x4, Codex x3, Antigravity, Cursor, Kimi, Z.ai, Qwen, Muse, OpenCode Go).
//   node web-dashboard/tests/browser/device/fixture-proxy.mjs <listen port> <dashboard port>
// With AAC_FIXTURE_ACCOUNTS=rich, the account routes come from ../accounts-fixture.mjs instead (every provider, a trash
// entry, paired trays, an Update apps run, and the sign-in and remove flows), for Accounts & Settings device checks.
// With AAC_FIXTURE_ACCOUNTS=deep (rich plus the deep-review states, see /mnt/Cursor/share/ai-account-center/device-fixture/),
// the Update apps run is the committed contract run (updateJob({ restartFailed: true, contract: true })) plus one plain
// unknown row, the Antigravity login is selected, and two Claude accounts share the name
// before the @ (sam@alpha.example, sam@beta.example) so Compare keeps their full addresses.
// D7: with AAC_FIXTURE_RUST_HOME=<aac-http binary> (the dashboard's Rust Home switch on), the dashboard answer offers
// the server's Home view and GET /api/views/home is answered from the same fixtures, the view built by the proxy's own
// Rust service (../rust-home-builder.mjs), as the switch-on server would build it from these answers.
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';
import { analyticsFixture } from './analytics-fixture.mjs';

const [listen = '18860', target = '18861'] = process.argv.slice(2);
const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos/fixtures');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, name), 'utf8'));

// Home and Accounts: every kind of row and card Home draws (../home-fixture.mjs), and the Claude desktop profiles.
const accounts = () => homeAccounts();
function status() {
	const view = { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json'), generatedAt: new Date().toISOString() };
	const now = Date.now();
	view.cards.open = view.cards.open.map((c) => ({ ...c, askedAt: new Date(now - (c.askedAgo ?? 0) * 1000).toISOString(), ageSeconds: c.askedAgo ?? 0 }));
	return view;
}
const scoreboard = () => ({ ...fixture('scoreboard-view.json'), byModel: fixture('scoreboard-by-model.json') });
// Analytics: made-up usage logs and quota history for the same accounts, for the range asked (24h, 7d or 30d)
const analytics = (url) => {
  const range = ['24h', '7d', '30d', '90d', 'all', 'custom'].includes(url.searchParams.get('range')) ? url.searchParams.get('range') : '7d';
  const days = url.searchParams.get('from') ? { from: url.searchParams.get('from'), to: url.searchParams.get('to') } : null;
  return analyticsFixture({ range, days, accounts: accounts().accounts });
};
// Logins (Central Logins step 1): every kind and health, long names, invented only.
const logins = () => ({ ...fixture('logins-view.json'), generatedAt: new Date().toISOString() });
const ANSWERS = { '/api/accounts/logins': logins, '/api/accounts/dashboard': accounts, '/api/claude/desktop-profiles': homeProfiles, '/api/accounts/analytics': analytics, '/api/status': status, '/api/scoreboard': scoreboard };
const rustHome = process.env.AAC_FIXTURE_RUST_HOME
	? await (await import('../rust-home-builder.mjs')).startRustBuilder(path.resolve(process.env.AAC_FIXTURE_RUST_HOME), path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..'))
	: null;
if (rustHome) {
	ANSWERS['/api/accounts/dashboard'] = () => rustHome.offered(accounts());
	for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => { rustHome.stop(); process.exit(0); });
}
// D7 device spec (with AAC_FIXTURE_RUST_HOME): /__d7/home-spec runs every case of tests/spec/views/home/cases.json
// (and AAC_FIXTURE_HOME_FUZZ, a fuzz.mjs table, when set) through this browser's own view-model.mjs, and the proxy
// builds each with Rust for this browser's format probe: equal bytes, a decline, or a named difference. Test harness
// only (loopback, made-up data); the dashboard has no such route.
const SPEC_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../../tests/spec/views/home');
function specCases() {
	const tables = [path.join(SPEC_DIR, 'cases.json'), ...(process.env.AAC_FIXTURE_HOME_FUZZ ? [process.env.AAC_FIXTURE_HOME_FUZZ] : [])];
	return tables.flatMap((file) => JSON.parse(fs.readFileSync(file, 'utf8')).cases)
		.map(({ name, dashboard, profiles, antigravity, ctx, presentation }) => ({ name, dashboard, profiles, antigravity, ctx, presentation }));
}
const SPEC_PAGE = `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>D7 Home spec</title><body style="font:15px -apple-system,sans-serif;margin:16px"><h3>D7 Home spec in this browser</h3>
<pre id="out" style="white-space:pre-wrap">running...</pre><script type="module">
const out = document.getElementById('out');
try {
  const vm = await import('/view-model.mjs');
  const tf = await import('/time-format.mjs');
  const cases = await (await fetch('/__d7/home-cases')).json();
  const results = cases.map((c) => {
    tf.setDisplayTimeZone(c.ctx.timeZone);
    try {
      return { name: c.name, json: JSON.stringify(vm.dashboardViewModel(c.dashboard, { now: c.ctx.now, profiles: c.profiles, platform: c.ctx.platform, antigravityInventory: c.antigravity, intervalSeconds: c.ctx.intervalSeconds, username: c.ctx.username, host: c.ctx.host })) };
    } catch { return { name: c.name, json: null }; }
  });
  const answer = await (await fetch('/__d7/home-check', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ probe: vm.homeFormatProbe(), engine: navigator.userAgent, results }) })).json();
  out.textContent = JSON.stringify(answer, null, 1);
  document.title = answer.differ.length ? 'D7 spec: DIFFERENT' : 'D7 spec: EQUAL';
} catch (e) { out.textContent = 'error: ' + e; }
</script>`;
/** The real server's answer to a GET with this browser's cookie (the Antigravity inventory), or null. */
const upstreamJson = (p, cookie) => new Promise((resolve) => {
	const r = http.request({ host: '127.0.0.1', port: target, path: p, headers: { cookie: cookie || '' } }, (res) => {
		let text = '';
		res.on('data', (d) => (text += d)).on('end', () => { try { resolve(res.statusCode === 200 ? JSON.parse(text) : null); } catch { resolve(null); } });
	});
	r.on('error', () => resolve(null));
	r.end();
});

const { fakeServer, updateJob } = await import('../accounts-fixture.mjs');
// `deep` is `rich` plus the deep-review states (device passes check the Update apps failures, the Antigravity runtime
// line and Compare's full addresses, none of which the plain rich answers produce).
const DEEP = process.env.AAC_FIXTURE_ACCOUNTS === 'deep';
const richAccounts = DEEP || process.env.AAC_FIXTURE_ACCOUNTS === 'rich';
function deepJob() {
	const job = updateJob({ restartFailed: true, contract: true });
	// One plain unknown, neither timed out nor unreachable, so the table's generic "unknown" word is producible on device.
	const zcode = job.results.find((r) => r.platform === 'ubuntu' && r.appId === 'zcode');
	if (zcode) { zcode.status = 'unknown'; zcode.previousVersion = ''; zcode.version = ''; zcode.message = 'Unknown: this computer did not report a status.'; }
	return job;
}
const rich = richAccounts ? fakeServer({ rich: true, update: DEEP ? deepJob() : updateJob() }) : null;
/** Whether a rich account route answers this request. */
const richMatch = (method, pathname) => !!rich && rich.routes.some(([m, re]) => m === method && re.test(pathname));
/** The rich account routes' answer for this request, or null. */
function richAnswer(method, pathname, body) {
	if (!rich) return null;
	for (const [m, re, answer] of rich.routes) {
		const match = pathname.match(re);
		if (m === method && match) return answer(body, match);
	}
	return null;
}
const richJson = (method, pathname, body = {}) => { const a = richAnswer(method, pathname, body); return a ? JSON.parse(a.body) : null; };
// The deep fixture's four answers. Everything mirrors the kit's rich answers with the three deep-review states added:
// the Antigravity profile id is the bound form (antigravity:profile:a1, the kit check's board()), and two Claude
// twins share the name before the @ (analytics-quota.test.mjs's sam@ twins).
function deepDashboard() {
	const d = richJson('GET', '/api/accounts/dashboard');
	const agy = d.accounts.find((a) => a.id === 'antigravity:a1');
	if (agy) agy.id = 'antigravity:profile:a1';
	const one = d.accounts.find((a) => a.id === 'claude:one');
	if (one) {
		const twins = ['alpha', 'beta'].map((name) => {
			const twin = JSON.parse(JSON.stringify(one));
			twin.id = `claude:twin-${name}`; twin.label = `sam@${name}.example`; twin.email = `sam@${name}.example`;
			twin.capabilities = { ...twin.capabilities, claudeProfileId: `twin-${name}` };
			return twin;
		});
		d.accounts.splice(d.accounts.indexOf(one) + 1, 0, ...twins);
	}
	return d;
}
const deepAntigravity = () => ({ schemaVersion: 1, hostId: 'ubuntu',
	profiles: [{ id: 'a1', email: 'agy.workspace@example.invalid', plan: 'pro', hostId: 'ubuntu', available: true, selected: true, verifiedAt: new Date().toISOString() }] });
function deepRegistry() {
	const r = richJson('GET', '/api/accounts/registry');
	for (const a of r.accounts) if (a.id === 'antigravity:a1') a.id = 'antigravity:profile:a1';
	for (const name of ['alpha', 'beta']) r.accounts.push({ id: `claude:twin-${name}`, provider: 'claude', actions: { remove: true, signInAgain: true, replaceKey: true, recheck: true }, removeRefusal: null });
	return r;
}
const deepAnswer = (pathname, url) => {
	if (pathname === '/api/accounts/dashboard') return deepDashboard();
	if (pathname === '/api/accounts/registry') return deepRegistry();
	if (pathname === '/api/antigravity/profiles') return deepAntigravity();
	if (pathname === '/api/accounts/analytics') {
		const range = ['24h', '7d', '30d', '90d', 'all', 'custom'].includes(url.searchParams.get('range')) ? url.searchParams.get('range') : '7d';
		const days = url.searchParams.get('from') ? { from: url.searchParams.get('from'), to: url.searchParams.get('to') } : null;
		return analyticsFixture({ range, days, accounts: deepDashboard().accounts });
	}
	return null;
};
const DEEP_PATHS = new Set(['/api/accounts/dashboard', '/api/accounts/registry', '/api/antigravity/profiles', '/api/accounts/analytics']);
/** The deep answers, only for a signed-in browser (the same check the other fixture routes make). */
function serveDeep(req, res, pathname, url) {
	const check = http.request({ host: '127.0.0.1', port: target, path: '/api/auth/check', headers: { cookie: req.headers.cookie || '', host: req.headers.host } }, (r) => {
		let text = '';
		r.on('data', (d) => (text += d)).on('end', () => {
			let signedIn = false;
			try { const s = JSON.parse(text); signedIn = s.authenticated === true || s.authRequired === false; } catch {}
			if (!signedIn) { res.writeHead(401, { 'content-type': 'application/json' }); res.end('{"error":"Sign in first.","code":"auth_required"}'); return; }
			res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
			res.end(JSON.stringify(deepAnswer(pathname, url)));
		});
	});
	check.on('error', () => { res.writeHead(502); res.end(); });
	check.end();
}

const server = http.createServer((req, res) => {
	const url = new URL(req.url, 'http://x');
	const pathname = url.pathname;
	if (DEEP && req.method === 'GET' && DEEP_PATHS.has(pathname)) { serveDeep(req, res, pathname, url); return; }
	if (richMatch(req.method, pathname)) {
		let raw = '';
		req.on('data', (d) => (raw += d)).on('end', () => {
			let body = {};
			try { body = raw ? JSON.parse(raw) : {}; } catch {}
			// only for a signed-in browser, as below
			const check = http.request({ host: '127.0.0.1', port: target, path: '/api/auth/check', headers: { cookie: req.headers.cookie || '', host: req.headers.host } }, (r) => {
				let text = '';
				r.on('data', (d) => (text += d)).on('end', () => {
					let signedIn = false;
					try { const s = JSON.parse(text); signedIn = s.authenticated === true || s.authRequired === false; } catch {}
					if (!signedIn) { res.writeHead(401, { 'content-type': 'application/json' }); res.end('{"error":"Sign in first.","code":"auth_required"}'); return; }
					const a = richAnswer(req.method, pathname, body);
					res.writeHead(a.status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
					res.end(a.body);
				});
			});
			check.on('error', () => { res.writeHead(502); res.end(); });
			check.end();
		});
		return;
	}
	if (rustHome && pathname.startsWith('/__d7/')) {
		if (req.method === 'GET' && pathname === '/__d7/home-spec') { res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' }); res.end(SPEC_PAGE); return; }
		if (req.method === 'GET' && pathname === '/__d7/home-cases') { res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' }); res.end(JSON.stringify(specCases())); return; }
		if (req.method === 'POST' && pathname === '/__d7/home-check') {
			const chunks = [];
			req.on('data', (d) => chunks.push(d)).on('end', async () => {
				const { probe, engine, results } = JSON.parse(Buffer.concat(chunks).toString('utf8'));
				const byName = new Map(specCases().map((c) => [c.name, c]));
				const summary = { engine, probe, cases: results.length, equal: 0, refused: 0, differ: [] };
				for (const r of results) {
					const c = byName.get(r.name);
					if (!c) { summary.refused++; continue; }
					// the stored envelope facts, with this browser's own probe as the page's context would carry it
					const data = { ctx: { ...c.ctx, formatProbe: probe }, dashboard: c.dashboard, profiles: c.profiles, antigravity: c.antigravity };
					const view = await rustHome.build({ data, presentation: c.presentation });
					if (!view) { summary.refused++; continue; }
					const rust = JSON.stringify(view);
					if (rust === r.json) { summary.equal++; continue; }
					let i = 0;
					const js = r.json ?? '';
					while (i < js.length && js[i] === rust[i]) i++;
					summary.differ.push({ name: r.name, at: i, js: js.slice(Math.max(0, i - 40), i + 60), rust: rust.slice(Math.max(0, i - 40), i + 60) });
				}
				console.log(`home spec in ${engine.slice(0, 80)}: ${summary.equal} equal, ${summary.refused} refused, ${summary.differ.length} differ`);
				res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
				res.end(JSON.stringify(summary));
			});
			return;
		}
	}
	if (rustHome && req.method === 'GET' && pathname === '/api/views/home') {
		const check = http.request({ host: '127.0.0.1', port: target, path: '/api/auth/check', headers: { cookie: req.headers.cookie || '', host: req.headers.host } }, (r) => {
			let body = '';
			r.on('data', (d) => (body += d)).on('end', async () => {
				let signedIn = false;
				try { const s = JSON.parse(body); signedIn = s.authenticated === true || s.authRequired === false; } catch {}
				if (!signedIn) { res.writeHead(401, { 'content-type': 'application/json' }); res.end('{"error":"Sign in first.","code":"auth_required"}'); return; }
				const reply = await rustHome.reply({
					dashboard: ANSWERS['/api/accounts/dashboard'](url),
					profiles: homeProfiles().profiles,
					antigravity: await upstreamJson('/api/antigravity/profiles', req.headers.cookie),
					contextHeader: req.headers['x-aac-home-context'],
					platform: url.searchParams.get('platform') ?? 'mac',
				});
				// a decline names what this browser's formatters wrote (fixed reference values, no account data)
				let probe = null;
				try { probe = JSON.parse(decodeURIComponent(req.headers['x-aac-home-context'] || '')).formatProbe; } catch {}
				console.log(`home view: ${reply.view ? 'built by Rust' : `refused, probe ${JSON.stringify(probe)}`} ${JSON.stringify(rustHome.stats)}`);
				res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
				res.end(JSON.stringify(reply));
			});
		});
		check.on('error', () => { res.writeHead(502); res.end(); });
		check.end();
		return;
	}
	if (req.method === 'GET' && ANSWERS[pathname]) {
		// Only for a signed-in browser: ask the real server first, so the session rules stay the real ones.
		const check = http.request({ host: '127.0.0.1', port: target, path: '/api/auth/check', headers: { cookie: req.headers.cookie || '', host: req.headers.host } }, (r) => {
			let body = '';
			r.on('data', (d) => (body += d)).on('end', () => {
				let signedIn = false;
				try { const s = JSON.parse(body); signedIn = s.authenticated === true || s.authRequired === false; } catch {}
				if (!signedIn) { res.writeHead(401, { 'content-type': 'application/json' }); res.end('{"error":"Sign in first.","code":"auth_required"}'); return; }
				res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
				res.end(JSON.stringify(ANSWERS[pathname](url)));
			});
		});
		check.on('error', () => { res.writeHead(502); res.end(); });
		check.end();
		return;
	}
	const upstream = http.request({ host: '127.0.0.1', port: target, path: req.url, method: req.method, headers: req.headers }, (r) => {
		res.writeHead(r.statusCode, r.headers);
		r.pipe(res);
	});
	upstream.on('error', () => { res.writeHead(502); res.end(); });
	req.pipe(upstream);
});
// The hint socket (/ws) and any other upgrade go straight through.
server.on('upgrade', (req, socket, head) => {
	const upstream = net.connect(Number(target), '127.0.0.1', () => {
		upstream.write(`${req.method} ${req.url} HTTP/1.1\r\n${Object.entries(req.headers).map(([k, v]) => `${k}: ${v}`).join('\r\n')}\r\n\r\n`);
		if (head?.length) upstream.write(head);
		upstream.pipe(socket).pipe(upstream);
	});
	upstream.on('error', () => socket.destroy());
	socket.on('error', () => upstream.destroy());
});
server.listen(Number(listen), '127.0.0.1', () => console.log(`fixture proxy on 127.0.0.1:${listen} -> 127.0.0.1:${target}`));
