// D7: a Rust service (aac-http) of the test harness's own, for Home data that never reaches the server under test (the
// kit's stubs, the device fixture proxy). The server builds Home itself from what it reads, so the harness hands its
// made-up data to this service the way the front door does: the page's context header and the three answers become
// the M3 presentation envelope (the real `homeReadInput`, run by tests/spec/views/home/envelope.ts under Bun), sent as
// GET /api/views/home's body to the shared read dispatch; the page is answered with the Rust view. The service gets
// a private temporary root, a random boot secret on stdin and no TCP port, as the server's own does. Made-up data
// only.
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { spawn, execFile } from 'node:child_process';
import { randomBytes } from 'node:crypto';

/** Starts `binary` (aac-http) on a private socket; `repo` is the checkout whose front-door code builds envelopes. */
export async function startRustBuilder(binary, repo) {
	const helper = path.join(repo, 'tests', 'spec', 'views', 'home', 'envelope.ts');
	const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aacr-')));
	const bootId = randomBytes(12).toString('hex');
	const secret = randomBytes(32).toString('hex');
	const run = path.join(root, 'run');
	const dir = path.join(run, `aac-http-${bootId}`);
	fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
	for (const p of [root, run, dir]) fs.chmodSync(p, 0o700);
	const socketPath = path.join(dir, 'http.sock');
	const child = spawn(binary, [], { stdio: ['pipe', 'ignore', 'ignore'] });
	child.stdin.write(`${JSON.stringify({ ccsDir: root, socketPath, secret, bootId })}\n`);
	for (let waited = 0; !fs.existsSync(socketPath); waited += 20) {
		if (child.exitCode !== null || waited > 5000) throw new Error('the harness\'s aac-http did not start');
		await new Promise((resolve) => setTimeout(resolve, 20));
	}
	const principal = JSON.stringify({ kind: 'session', id: 'harness-home-builder', role: 'admin' });
	const stats = { built: 0, refused: 0, noContext: 0, failed: 0 };
	/** The Rust view for this envelope, or null (the shared read fallback: Rust refused it). */
	const build = (envelope) => {
		const body = JSON.stringify(envelope);
		return new Promise((resolve) => {
			const req = http.request({ socketPath, path: '/api/views/home', method: 'GET', headers: { 'x-aac-boot-secret': secret, 'x-aac-principal': principal, 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) } }, (res) => {
				const chunks = [];
				res.on('data', (c) => chunks.push(c));
				res.on('end', () => {
					let json = null;
					try { json = JSON.parse(Buffer.concat(chunks).toString('utf8')); } catch {}
					if (res.statusCode === 200 && json && json.version === 3) { stats.built++; resolve(json); return; }
					stats.refused++;
					resolve(null);
				});
			});
			req.on('error', () => { stats.failed++; resolve(null); });
			req.end(body);
		});
	};
	/** The front door's envelope for a page's request (null: unreadable context or no profiles). */
	const envelopeFor = (input) => new Promise((resolve) => {
		const proc = execFile(process.env.BUN || 'bun', [helper, 'front-door'], { cwd: repo, maxBuffer: 256 * 1024 * 1024 }, (error, stdout) => {
			if (error) { stats.failed++; resolve(null); return; }
			try { resolve(JSON.parse(stdout).envelope); } catch { stats.failed++; resolve(null); }
		});
		proc.stdin.end(JSON.stringify(input));
	});
	return {
		stats,
		build,
		/** The dashboard answer offering the server view (`server.views`), as the switch-on server sends it. */
		offered(dashboard) {
			return dashboard && typeof dashboard === 'object' && Array.isArray(dashboard.accounts)
				? { ...dashboard, server: { version: '', commit: null, ...dashboard.server, views: ['home'] } }
				: dashboard;
		},
		/** GET /api/views/home's answer for these inputs, as src/web-server/routes/home-view-routes.ts builds it. */
		async reply({ dashboard, profiles, antigravity, contextHeader, platform }) {
			const envelope = await envelopeFor({ contextHeader, platform, dashboard, profiles, antigravity });
			if (!envelope) stats.noContext++;
			const view = envelope ? await build(envelope) : null;
			return { schemaVersion: 1, dashboard, profiles: Array.isArray(profiles) ? profiles : null, antigravity, view };
		},
		stop() {
			child.stdin.end();
			child.kill('SIGTERM');
			fs.rmSync(root, { recursive: true, force: true });
		},
	};
}
