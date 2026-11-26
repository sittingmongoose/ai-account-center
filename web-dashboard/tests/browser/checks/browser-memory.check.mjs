// Browser memory: what one dashboard tab costs Chrome, page by page and after sitting open.
//
//   browser-check run web-dashboard/tests/browser/checks --grep 'browser memory'
//   (--set idleSeconds=120 sets the idle wait on Home; --set out=<file> also writes the numbers there;
//    --set limitMB=<n> fails when the idle Home renderer PSS exceeds n MB; without it the numbers are recorded, not judged)
//
// For each tab (Home, Accounts, Analytics, Status, Scoreboard), opened in turn inside one frame, it records the page's
// JS heap (CDP Performance.getMetrics), DOM node count, the WebAssembly memory the app holds (sum of every
// WebAssembly.Memory the page created, read via a wrapper installed before the app loads), and the proportional set
// size (PSS) of every Chrome process from /proc (Linux only): renderer, GPU, browser and utility processes. Recorded,
// not judged unless --set limitMB is given. The server runs on the kit's temporary home, so data-heavy pages are smaller than on a real install.
import fs from 'node:fs';
import path from 'node:path';

const VIEWPORT = { width: 1440, height: 900 };
const PAGES = ['home', 'accounts', 'analytics', 'status', 'scoreboard'];

// Count every wasm linear memory the page creates (wasm-bindgen instantiates one; it can grow later).
const WASM_PROBE = `(() => {
	const seen = (window.__wasmMemories = []);
	const wrap = (fn, pick) => async function (...a) { const r = await fn.apply(this, a); try { const i = pick(r); const m = i && i.exports && i.exports.memory; if (m instanceof WebAssembly.Memory) seen.push(m); } catch {} return r; };
	WebAssembly.instantiate = wrap(WebAssembly.instantiate, (r) => r.instance || r);
	if (WebAssembly.instantiateStreaming) WebAssembly.instantiateStreaming = wrap(WebAssembly.instantiateStreaming, (r) => r.instance);
})();`;

function pssKb(pid) {
	try {
		const m = /^Pss:\s+(\d+) kB/m.exec(fs.readFileSync(`/proc/${pid}/smaps_rollup`, 'utf8'));
		return m ? +m[1] : null;
	} catch {
		return null;
	}
}

async function sample(t, label, browserSession) {
	const cdp = await t.context.newCDPSession(t.page);
	await cdp.send('Performance.enable');
	const metrics = Object.fromEntries((await cdp.send('Performance.getMetrics')).metrics.map((m) => [m.name, m.value]));
	await cdp.detach();
	const page = await t.page.evaluate(() => {
		const frames = [window, ...Array.from(document.querySelectorAll('iframe')).map((f) => { try { return f.contentWindow; } catch { return null; } })].filter(Boolean);
		let wasm = 0;
		for (const w of frames) for (const m of w.__wasmMemories || []) wasm += m.buffer.byteLength;
		return { wasmBytes: wasm, nodes: document.getElementsByTagName('*').length };
	});
	let processes = null;
	if (process.platform === 'linux') {
		const info = await browserSession.send('SystemInfo.getProcessInfo');
		processes = info.processInfo.map((p) => ({ type: p.type, id: p.id, pssKb: pssKb(p.id) }));
	}
	const mb = (b) => Math.round((b / 1048576) * 10) / 10;
	const sum = (type) => processes ? processes.filter((p) => !type || p.type === type).reduce((s, p) => s + (p.pssKb || 0), 0) / 1024 : null;
	return {
		label,
		jsHeapUsedMB: mb(metrics.JSHeapUsedSize),
		jsHeapTotalMB: mb(metrics.JSHeapTotalSize),
		wasmMB: mb(page.wasmBytes),
		domNodes: page.nodes,
		rendererPssMB: processes ? Math.round(sum('renderer')) : null,
		gpuPssMB: processes ? Math.round(sum('GPU')) : null,
		chromeTotalPssMB: processes ? Math.round(sum()) : null,
		processes,
	};
}

export const tests = {
	async 'browser memory: each tab and an idle Home, one frame'(t) {
		const idleSeconds = Math.max(0, +(t.args.idleSeconds ?? 120));
		await t.newContext({ viewport: VIEWPORT });
		await t.context.addInitScript(WASM_PROBE);
		const browserSession = process.platform === 'linux' ? await t.context.browser().newBrowserCDPSession() : null;
		await t.goto('/api/health', { settleMs: 0 });
		await t.app.signIn();
		const rows = [];
		for (const page of PAGES) {
			await t.app.open({ page });
			await t.page.waitForTimeout(5000);
			rows.push(await sample(t, page, browserSession));
		}
		await t.app.open({ page: 'home' });
		await t.page.waitForTimeout(idleSeconds * 1000);
		rows.push(await sample(t, `home after ${idleSeconds} s`, browserSession));
		const out = { at: new Date().toISOString(), viewport: VIEWPORT, rows };
		fs.writeFileSync(path.join(t.run.dir, 'browser-memory.json'), JSON.stringify(out, null, 2));
		if (t.args.out) fs.writeFileSync(path.resolve(t.args.out), JSON.stringify(out, null, 2));
		const line = (r) => `${r.label}: heap ${r.jsHeapUsedMB} MB, wasm ${r.wasmMB} MB, renderer ${r.rendererPssMB} MB, chrome ${r.chromeTotalPssMB} MB`;
		await t.check('memory recorded', rows.length === PAGES.length + 1, '', { note: rows.map(line).join('; ') });
		// Recorded by default; --set limitMB=<n> judges the idle Home renderer total PSS against n MB.
		if (t.args.limitMB !== undefined) {
			const limit = Number(t.args.limitMB);
			if (!Number.isFinite(limit) || limit < 0) {
				await t.check('browser memory idle renderer PSS within limit', false, `limitMB=${t.args.limitMB} is not a non-negative number`);
			} else {
				const idle = rows[rows.length - 1];
				const renderer = idle ? idle.rendererPssMB : null;
				if (renderer === null || renderer === undefined) {
					await t.check('browser memory idle renderer PSS within limit', false, `renderer PSS unavailable after ${idleSeconds} s idle (Linux only)`);
				} else {
					await t.check('browser memory idle renderer PSS within limit', renderer <= limit, `renderer ${renderer} MB vs limit ${limit} MB after ${idleSeconds} s idle`);
				}
			}
		}
	},
};
