// Realistic data for device checks (DEVICE-RECIPE.md): a proxy in front of a test dashboard that answers Home's
// accounts, Status and the Scoreboard from made-up fixtures and forwards everything else (sign-in, the frame's pages,
// the socket) to the real server unchanged. Test data only: invented names, never a real account.
// Home's accounts are home-fixture.mjs (Claude x4, Codex x3, Antigravity, Cursor, Kimi, Z.ai, Qwen, Muse, OpenCode Go).
//   node web-dashboard/tests/browser/device/fixture-proxy.mjs <listen port> <dashboard port>
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';

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
const ANSWERS = { '/api/accounts/dashboard': accounts, '/api/claude/desktop-profiles': homeProfiles, '/api/status': status, '/api/scoreboard': scoreboard };

const server = http.createServer((req, res) => {
	const pathname = new URL(req.url, 'http://x').pathname;
	if (req.method === 'GET' && ANSWERS[pathname]) {
		// Only for a signed-in browser: ask the real server first, so the session rules stay the real ones.
		const check = http.request({ host: '127.0.0.1', port: target, path: '/api/auth/check', headers: { cookie: req.headers.cookie || '', host: req.headers.host } }, (r) => {
			let body = '';
			r.on('data', (d) => (body += d)).on('end', () => {
				let signedIn = false;
				try { const s = JSON.parse(body); signedIn = s.authenticated === true || s.authRequired === false; } catch {}
				if (!signedIn) { res.writeHead(401, { 'content-type': 'application/json' }); res.end('{"error":"Sign in first.","code":"auth_required"}'); return; }
				res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
				res.end(JSON.stringify(ANSWERS[pathname]()));
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
