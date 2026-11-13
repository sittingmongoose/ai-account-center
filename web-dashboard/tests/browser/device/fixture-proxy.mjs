// Realistic data for device checks (DEVICE-RECIPE.md): a proxy in front of a test dashboard that answers Home's
// accounts, Analytics, Status and the Scoreboard from made-up fixtures and forwards everything else (sign-in, the frame's pages,
// the socket) to the real server unchanged. Test data only: invented names, never a real account.
// Home's accounts are home-fixture.mjs (Claude x4, Codex x3, Antigravity, Cursor, Kimi, Z.ai, Qwen, Muse, OpenCode Go).
//   node web-dashboard/tests/browser/device/fixture-proxy.mjs <listen port> <dashboard port>
// With AAC_FIXTURE_ACCOUNTS=rich, the account routes come from ../accounts-fixture.mjs instead (every provider, a trash
// entry, paired trays, an Update apps run, and the sign-in and remove flows), for Accounts & Settings device checks.
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
const ANSWERS = { '/api/accounts/dashboard': accounts, '/api/claude/desktop-profiles': homeProfiles, '/api/accounts/analytics': analytics, '/api/status': status, '/api/scoreboard': scoreboard };

// Home's switch flow without switching anything: a Codex Activate first asks for the approval (409, the made-up
// processes below), and the approval's one-use token then "switches". Nothing reaches the real server.
const TOKEN = 'Abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE';
function activate(profile, body) {
	if (body?.confirmationToken === TOKEN) return [200, { name: profile, email: `${profile}@example.invalid` }];
	return [409, { error: 'Codex processes are running.', code: 'confirmation_required', confirmation: {
		token: TOKEN, targetProfile: profile, expiresAt: new Date(Date.now() + 5 * 60_000).toISOString(), warning: 'Active Codex work will stop. The listed processes will be stopped and Codex will restart with the selected account.',
		processes: [{ label: 'codex (made-up terminal session)', pid: 4242, role: 'cli' }, { label: 'Codex app (made-up)', pid: 4311, role: 'app' }] } }];
}

const { fakeServer, updateJob } = await import('../accounts-fixture.mjs');
const rich = process.env.AAC_FIXTURE_ACCOUNTS === 'rich' ? fakeServer({ rich: true, update: updateJob() }) : null;
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

const server = http.createServer((req, res) => {
	const url = new URL(req.url, 'http://x');
	const pathname = url.pathname;
	const switching = req.method === 'POST' && pathname.match(/^\/api\/codex\/profiles\/([A-Za-z0-9_-]+)\/activate$/);
	if (switching) {
		let raw = '';
		req.on('data', (d) => (raw += d)).on('end', () => {
			let body = null;
			try { body = JSON.parse(raw); } catch {}
			const [status, json] = activate(switching[1], body);
			res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
			res.end(JSON.stringify(json));
		});
		return;
	}
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
