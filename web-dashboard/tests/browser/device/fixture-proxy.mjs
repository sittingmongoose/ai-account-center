// Realistic data for device checks (DEVICE-RECIPE.md): a proxy in front of a test dashboard that answers Home's
// accounts, Analytics, Status and the Scoreboard from made-up fixtures and forwards everything else (sign-in, the frame's pages,
// the socket) to the real server unchanged. Test data only: invented names, never a real account.
//   node web-dashboard/tests/browser/device/fixture-proxy.mjs <listen port> <dashboard port>
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { analyticsFixture } from './analytics-fixture.mjs';

const [listen = '18860', target = '18861'] = process.argv.slice(2);
const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos/fixtures');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, name), 'utf8'));

const at = (minutes) => new Date(Date.now() + minutes * 60_000).toISOString();
const quota = (key, label, used, minutes, reset) => ({ key, label, usedPercent: used, remainingPercent: 100 - used, resetAt: at(reset), windowMinutes: minutes });
const account = (id, provider, email, plan, windows, extra = {}) => ({
	id, provider, providerLabel: provider, label: email, email, plan, platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok', message: null,
	fetchedAt: at(-2), sampledAt: at(-2), isActive: false, hidden: false, trayHidden: false, windows, capabilities: {}, signInNeeded: [], ...extra,
});
const five = (u, r) => quota('five_hour', 'Five-hour usage', u, 300, r);
const week = (u, r) => quota('seven_day', 'Weekly usage', u, 10080, r);
const accounts = () => ({
	schemaVersion: 1,
	updatedAt: at(-1),
	accounts: [
		account('claude-1', 'claude', 'a.very.long.example.name@example.invalid', 'pro', [five(9, 160), week(93, 3500)]),
		account('claude-2', 'claude', 'second.claude.account@example.invalid', 'max', [five(100, 210), week(7, 9000)]),
		account('claude-3', 'claude', 'third-claude-with-a-long-name@example.invalid', 'max', [five(94, 60), week(0, 9900)]),
		account('codex-1', 'codex', 'codex.primary.workspace@example.invalid', 'pro', [quota('five_hour', '5h', 55, 300, 130), quota('seven_day', 'week', 44, 10080, 2300)], { isActive: true }),
		account('codex-2', 'codex', 'codex.second@example.invalid', 'plus', [quota('five_hour', '5h', 12, 300, 250), quota('seven_day', 'week', 31, 10080, 7000)]),
		account('cursor-1', 'cursor', 'cursor.team.seat@example.invalid', 'pro', [quota('monthly', 'Monthly usage', 18, 43200, 20000)]),
		account('kimi-1', 'kimi', 'kimi.code.example@example.invalid', 'pro', [quota('weekly', 'Weekly usage', 63, 10080, 4000)]),
		account('zai-1', 'zai', 'zai.coding.plan.example@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 27, 300, 120)]),
		account('qwen-1', 'qwen', 'qwen.token.plan@example.invalid', 'pro', [quota('monthly', 'Monthly usage', 41, 43200, 15000)]),
	],
});
function status() {
	const view = { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json'), generatedAt: new Date().toISOString() };
	const now = Date.now();
	view.cards.open = view.cards.open.map((c) => ({ ...c, askedAt: new Date(now - (c.askedAgo ?? 0) * 1000).toISOString(), ageSeconds: c.askedAgo ?? 0 }));
	return view;
}
const scoreboard = () => ({ ...fixture('scoreboard-view.json'), byModel: fixture('scoreboard-by-model.json') });
// Analytics: made-up usage logs and quota history for the same accounts, for the range asked (24h, 7d or 30d)
const analytics = (url) => analyticsFixture({ range: ['24h', '7d', '30d'].includes(url.searchParams.get('range')) ? url.searchParams.get('range') : '7d', accounts: accounts().accounts });
const ANSWERS = { '/api/accounts/dashboard': accounts, '/api/accounts/analytics': analytics, '/api/status': status, '/api/scoreboard': scoreboard };

const server = http.createServer((req, res) => {
	const url = new URL(req.url, 'http://x');
	const pathname = url.pathname;
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
