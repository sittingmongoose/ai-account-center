// Accounts & Settings as a native page in the app frame (PLAN step 6, leptos/src/accounts): the real test server and
// sign-in, with the account routes answered in the page (a fake server: made-up accounts, keys and codes, nothing
// real is touched). Each flow checks what the page draws by DOM selector and exactly which requests it sends: the
// same ones the Slint page sent through bridge.js and accounts-controller.mjs (now run by public/frame-dashboard.mjs).

import { fakeServer, serve, changes, dashboard, updateJob } from '../accounts-fixture.mjs';
import { HOST_IDS, HOST_LABELS } from '../../../public/computers.mjs';

async function open(t, server, { width = 1440, height = 900, phone = false, duo = false, safe = '' } = {}) {
	await t.newContext(phone
		? { viewport: { width, height }, ...(duo ? { screen: { width: 466, height: 678 } } : {}), isMobile: true, hasTouch: true, serviceWorkers: 'block' }
		: { viewport: { width, height }, ...(duo ? { screen: { width: 466, height: 678 } } : {}), serviceWorkers: 'block' });
	await t.app.signIn();
	await serve(t, server);
	await t.goto(`/app/accounts?e2e${safe ? `&safe=${safe}` : ''}`, { settleMs: 300 });
	await t.until('the Accounts page is drawn from the dashboard', () => document.querySelectorAll('.prov[data-provider]').length >= 9, null, { timeout: 45000, hard: true });
}

const row = (id) => `.acc-item[data-row="${id}"]`;

// What the Update apps table shows, as a reader sees it: every text that is drawn (not the 1 px words kept for screen
// readers), each word's line boxes and the column it must stay inside (its cell, computer head or app name). Each
// head's visible name, title and state; each cell's drawn words, its column's width, the visible name and title of the
// head it is drawn beneath, and how wide each of `words` (the job's versions) is drawn in that cell's own font.
function measureUpdateTable(words = []) {
	const table = document.querySelector('[data-probe="update-card"] .upd-table');
	if (!table) return null;
	const drawn = (el) => { const r = el.getBoundingClientRect(); return r.width > 1 && r.height > 1 && el.checkVisibility({ visibilityProperty: true }); };
	const broken = [];
	const spill = [];
	const walker = document.createTreeWalker(table, NodeFilter.SHOW_TEXT);
	for (let node = walker.nextNode(); node; node = walker.nextNode()) {
		const el = node.parentElement;
		if (!el || !drawn(el)) continue;
		const box = el.closest('.ut-cell, .ut-host, .ut-app')?.getBoundingClientRect();
		for (const m of node.data.matchAll(/\S+/g)) {
			const range = document.createRange();
			range.setStart(node, m.index);
			range.setEnd(node, m.index + m[0].length);
			const rects = [...range.getClientRects()].filter((r) => r.width > 0);
			const lines = new Set(rects.map((r) => Math.round(r.top)));
			if (lines.size > 1) broken.push(m[0]);
			if (box && rects.some((r) => r.left < box.left - 0.5 || r.right > box.right + 0.5)) spill.push(m[0]);
		}
	}
	const headEls = [...table.querySelectorAll('.ut-host')];
	const heads = headEls.map((h) => h.getBoundingClientRect());
	const overlap = heads.some((a, i) => heads.some((b, j) => j > i && a.left < b.right - 0.5 && b.left < a.right - 0.5));
	const shown = (el) => {
		const out = [];
		if (!el) return '';
		const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
		for (let n = w.nextNode(); n; n = w.nextNode()) if (n.parentElement && drawn(n.parentElement)) out.push(n.data.trim());
		return out.filter(Boolean).join(' ');
	};
	const hosts = headEls.map((h) => shown(h));
	// a head as a reader sees it: the name drawn in it, its tip and the state drawn under it
	const headInfo = headEls.map((h) => ({ name: shown(h.querySelector('.ut-hname')), title: h.title, state: [...h.children].filter((c) => !c.matches('.ut-hname')).map(shown).filter(Boolean).join(' ') }));
	const ctx = document.createElement('canvas').getContext('2d');
	const rows = [...table.querySelectorAll('.ut-item')].map((item) => ({
		app: shown(item.querySelector('.ut-app')),
		cells: [...item.querySelectorAll('.ut-cell')].map((c) => {
			// the computer a cell is drawn under: the head whose column holds the cell's middle, by the name it shows
			const r = c.getBoundingClientRect();
			const mid = (r.left + r.right) / 2;
			const at = headEls.findIndex((h) => { const b = h.getBoundingClientRect(); return mid >= b.left && mid <= b.right; });
			const word = [...c.querySelectorAll('.ut-w, .ut-t')].find(drawn);
			const widths = {};
			if (word) {
				const cs = getComputedStyle(word);
				ctx.font = `${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`;
				for (const w of words) widths[w] = ctx.measureText(w).width;
			}
			return { text: shown(c), full: c.querySelector('.ut-w')?.textContent.trim() || '', label: c.getAttribute('aria-label') || '',
				name: headInfo[at]?.name || '', host: headInfo[at]?.title || '', col: r.width, widths,
				crit: c.classList.contains('w-crit'), warn: c.classList.contains('w-warn'), tip: c.title };
		}),
	}));
	// a drawn head, name or state its box cuts off (an ellipsis or a clip, sideways or below)
	const cut = [...table.querySelectorAll('.ut-host *, .ut-app *, .ut-app, .ut-cell, .ut-cell *')]
		.filter((el) => drawn(el) && el.textContent.trim() && getComputedStyle(el).overflow !== 'visible' && (el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1))
		.map((el) => el.textContent.trim());
	return { broken, spill, overlap, cut, hosts, heads: headInfo, rows, compact: table.classList.contains('compact'), sideways: document.documentElement.scrollWidth > innerWidth + 1, appHead: shown(table.querySelector('.ut-head .ut-app')) };
}

// The computers in the table's order (Mac and Windows first) and each one's short head (update.rs `short_labels`):
// its first word, the first three letters of a longer one; two that would share a head keep their full names.
const TABLE_HOSTS = ['mac', 'windows', ...HOST_IDS.filter((id) => id !== 'mac' && id !== 'windows')];
const SHORT_LABELS = (() => {
	const short = TABLE_HOSTS.map((id) => { const first = HOST_LABELS[id].split(/\s+/)[0] || ''; return [...first].length > 6 ? [...first].slice(0, 3).join('') : first; });
	return Object.fromEntries(TABLE_HOSTS.map((id, i) => [id, !short[i] || short.filter((s) => s === short[i]).length > 1 ? HOST_LABELS[id] : short[i]]));
})();
// The computer a visible head name stands for ('' when none): its full name on a desktop, its short head on a phone.
const hostByName = (name, compact) => TABLE_HOSTS.find((id) => name === (compact ? SHORT_LABELS[id] : HOST_LABELS[id])) || '';
// The versions an updated cell may name, in both forms: compact "→v", wide "→ v".
const updateWords = (job) => job.results.filter((r) => r.status === 'updated' && r.version).flatMap((r) => [`→${r.version}`, `→ ${r.version}`]);

// Deep review 2026-11-26 (Astra, Medium): before any cell is judged, every column's visible name, its full name (the
// head's tip) and its computer-level state must agree, in the table's order: a header whose shown name was swapped
// while its tip stayed, or a "no results" moved under another computer, is caught here.
function updateHeadMismatches(m, job) {
	const bad = [];
	const compact = m?.compact !== false;
	const ids = job.scope ? TABLE_HOSTS.filter((id) => job.scope[id]) : TABLE_HOSTS;
	if ((m?.heads.length ?? -1) !== ids.length) bad.push(`${m?.heads.length} heads, expected ${ids.length}`);
	ids.forEach((id, i) => {
		const h = m?.heads[i];
		if (!h) return;
		const name = compact ? SHORT_LABELS[id] : HOST_LABELS[id];
		if (h.name !== name || h.title !== HOST_LABELS[id]) bad.push(`column ${i + 1}: shows "${h.name}" titled "${h.title}", expected "${name}" titled "${HOST_LABELS[id]}"`);
		const none = !job.results.some((r) => r.platform === id);
		const state = none ? (compact ? 'no results' : 'No results from this computer') : '';
		if (h.state !== state) bad.push(`column ${i + 1} (${HOST_LABELS[id]}): state "${h.state}", expected "${state}"`);
	});
	return bad;
}

// What each app x computer cell must say, from the fixture job: the phone's short word or the desktop's words, and its
// tone. A computer without a row for an app shows "–". An update names its version where the whole version fits its
// column in the cell's font, else says "updated" (near the edge either is right: the page's estimate errs towards
// "updated"); its full version stays in the cell's label.
function expectedUpdateCells(job, phone) {
	const want = new Map();
	for (const r of job.results) {
		const host = HOST_LABELS[r.platform];
		const catalog = /not registered yet/.test(r.message || '');
		const failed = r.status === 'failed' || r.status === 'restart_failed';
		const words = catalog
			? (r.status === 'restart_failed' ? ['updated models failed', 'updated, model refresh failed'] : ['models failed', 'model refresh failed'])
			: {
				current: ['✓', 'current'],
				updated: [`→${r.version}`, `→ ${r.version}`],
				action_required: ['quit', 'quit to finish'],
				failed: ['failed', r.diagnostics?.cause === 'installer_failed' ? 'installer failed' : 'failed'],
				restart_failed: ['failed', 'restart failed'],
				not_installed: ['absent', 'not installed'],
				unknown: /^Timed out/.test(r.message || '') ? ['timed out', 'timed out'] : ['unknown', 'unknown'],
			}[r.status];
		const tone = failed ? 'crit' : r.status === 'action_required' || (r.status === 'unknown' && /timed out/i.test(r.message || '')) ? 'warn' : '';
		want.set(`${r.appLabel} | ${host}`, { text: words[phone ? 0 : 1], tone, version: r.status === 'updated' ? r.version : '' });
	}
	return want;
}

// The words an updated cell may show in its column: its version while that fits with room to spare, "updated" once it
// does not fit, either within 10% of the edge.
function versionWords(c, w) {
	const width = c.widths[w.text];
	if (typeof width !== 'number') return [w.text];
	if (width > c.col) return ['updated'];
	return width >= c.col * 0.9 - 1 ? [w.text, 'updated'] : [w.text];
}

// Every drawn cell against what the job says for its app under the computer whose visible head it sits beneath; every
// app x computer pair must be drawn exactly once. An updated cell must keep its full version in its label.
function updateCellMismatches(m, want) {
	const bad = [];
	const seen = new Set();
	const compact = m?.compact !== false;
	for (const r of m?.rows || []) {
		for (const c of r.cells) {
			const id = hostByName(c.name, compact);
			const key = `${r.app} | ${id ? HOST_LABELS[id] : `?${c.name}`}`;
			const w = want.get(key) || { text: '–', tone: '' };
			const tone = c.crit ? 'crit' : c.warn ? 'warn' : '';
			const texts = w.version ? versionWords(c, w) : [w.text];
			if (seen.has(key)) bad.push(`${key} drawn twice`);
			seen.add(key);
			if (!texts.includes(c.text) || tone !== w.tone) bad.push(`${key}: "${c.text}" ${tone || 'plain'}, expected "${texts.join('" or "')}" ${w.tone || 'plain'}`);
			if (w.version && c.label !== `Updated to ${w.version}`) bad.push(`${key}: label "${c.label}", expected "Updated to ${w.version}"`);
		}
	}
	const apps = [...new Set([...want.keys()].map((k) => k.split(' | ')[0]))];
	for (const app of apps) for (const id of HOST_IDS) if (!seen.has(`${app} | ${HOST_LABELS[id]}`)) bad.push(`${app} | ${HOST_LABELS[id]} not drawn`);
	return bad;
}

export const tests = {
	// Astra max M1 (post-landing fix): the frame's dashboard controller stays in the window after sign-out, unlike the
	// old iframe, so a save queued behind a held reply must die with its session. Ordinary clicks; synthetic answers.
	async 'accounts: a save queued before sign-out is never sent with the next session'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		await serve(t, fakeServer());
		let signedIn = true;
		let epoch = 1;
		let release;
		const gate = new Promise((resolve) => { release = resolve; });
		const patches = [];
		const hidden = new Set();
		const json = (body, extra = {}) => ({ status: 200, contentType: 'application/json', body: JSON.stringify(body), ...extra });
		await t.page.route('**/api/auth/check', (r) => r.fulfill(json({ authenticated: signedIn, authRequired: true, configured: true, accessMode: 'login', username: 'reviewer', secureTransport: true, trustedLocalNetwork: true })));
		await t.page.route('**/api/auth/logout', (r) => { signedIn = false; return r.fulfill(json({ success: true })); });
		await t.page.route('**/api/auth/login', (r) => { signedIn = true; epoch = 2; return r.fulfill(json({ success: true, username: 'reviewer' }, { headers: { 'set-cookie': 'fixture_epoch=two; Path=/; SameSite=Lax' } })); });
		await t.page.route('**/api/accounts/dashboard?*', (r) => {
			const d = dashboard();
			d.settings.hiddenProviders = [...hidden];
			d.providers.forEach((p) => { p.visible = !hidden.has(p.id); });
			d.accounts.forEach((a) => { a.hidden = hidden.has(a.provider); });
			return r.fulfill(json(d));
		});
		await t.page.route('**/api/accounts/visibility', async (r) => {
			const body = r.request().postDataJSON();
			const cookie = await r.request().headerValue('cookie');
			patches.push({ epoch, body, nextSession: /fixture_epoch=two/.test(cookie || '') });
			for (const p of body.hiddenProviders?.hide || []) hidden.add(p);
			for (const p of body.hiddenProviders?.show || []) hidden.delete(p);
			const answer = { hiddenProviders: [...hidden], hiddenAccountIds: [], trayHiddenProviders: [], trayHiddenAccountIds: [] };
			if (patches.length === 1) await gate;
			await r.fulfill(json(answer)).catch(() => {});
		});
		try {
			await t.goto('/app/accounts?e2e', { settleMs: 300 });
			await t.page.waitForSelector('[data-probe="show:codex"]');
			await t.page.evaluate(() => { window.__oldController = window.aacFrameDashboard; });
			// Claude's Show in tray off (its reply is held), then Codex's Show on dashboard off: queued behind it
			await t.page.click('[data-probe="tray:claude"]');
			await t.page.click('[data-probe="show:codex"]');
			await t.equal('only the first save has reached the server', patches.length, 1);
			await t.page.click('.chrome .select-btn[aria-label="Account"]');
			await t.page.locator('.acct-menu .menu-item', { hasText: 'Sign out' }).click();
			await t.page.waitForSelector('.signin-card');
			await t.page.fill('input[name="username"]', 'reviewer');
			await t.page.fill('input[name="password"]', 'Fixture-password-2026!');
			await t.page.click('.signin-card button[type="submit"]');
			await t.page.waitForSelector('[data-probe="show:codex"]');
			await t.check('the old controller stopped; a new one owns the page', await t.page.evaluate(() => window.__oldController.state().stopped && window.__oldController.id !== window.aacFrameDashboard.id));
			// the new session hides Codex, then shows it again, before the old reply arrives
			await t.page.click('[data-probe="show:codex"]');
			await t.until('the new session hides Codex', () => document.querySelector('[data-probe="show:codex"]')?.getAttribute('aria-checked') === 'false', null, { timeout: 5000, hard: true });
			await t.page.click('[data-probe="show:codex"]');
			await t.until('and shows it again', () => document.querySelector('[data-probe="show:codex"]')?.getAttribute('aria-checked') === 'true', null, { timeout: 5000, hard: true });
			const before = patches.length;
			release();
			await t.page.waitForTimeout(400);
			await t.page.click('.chrome button[aria-label="Refresh"]');
			await t.page.waitForTimeout(500);
			const shown = await t.page.evaluate(() => document.querySelector('[data-probe="show:codex"]')?.getAttribute('aria-checked'));
			await t.equal('the old queue sends no save after the new session has saved', patches.length, before, { note: JSON.stringify(patches.map((p) => ({ epoch: p.epoch, nextSession: p.nextSession, body: p.body }))) });
			await t.equal('the latest explicit Show choice stands', shown, 'true');
		} finally {
			release();
		}
	},

	// Sol max L1 (post-landing fix): a layout callback the Accounts page queued for its next frame must do nothing when the
	// page has gone by then (a quick sign-out disposes it first). Before the fix it read the disposed NodeRef and the
	// runtime trapped ("already been disposed", then "RuntimeError: unreachable").
	async 'accounts: a sign-out just after Accounts lays out leaves no uncaught error'(t) {
		const server = fakeServer();
		await open(t, server);
		const errors = [];
		t.page.on('pageerror', (e) => errors.push(`pageerror: ${String(e?.message || e).slice(0, 200)}`));
		t.page.on('console', (m) => { if (m.type() === 'error' && /disposed|unreachable|panicked/i.test(m.text())) errors.push(`console: ${m.text().slice(0, 200)}`); });
		for (let round = 1; round <= 3; round++) {
			await t.page.click('.chrome .nav a[href="/app/home"]');
			await t.page.waitForTimeout(250);
			// back to Accounts, then signed out before its next frame: its layout callbacks are already queued. The next
			// frames come late here (as on a busy phone), so the page is gone when they run, every time.
			await t.page.evaluate(async () => {
				window.__kitFrame ||= window.requestAnimationFrame;
				const frame = window.__kitFrame.bind(window);
				window.requestAnimationFrame = (cb) => setTimeout(() => frame(cb), 400);
				document.querySelector('.chrome .nav a[href="/app/accounts"]').click();
				await new Promise((resolve) => setTimeout(resolve, 0));
				window.dispatchEvent(new CustomEvent('aac-dash', { detail: { key: 'signed-out', json: JSON.stringify({ state: 'default', reason: '' }) } }));
			});
			await t.page.waitForSelector('.signin-card', { timeout: 10000 });
			// the late frames run now, on the page that went
			await t.page.waitForTimeout(900);
			await t.page.evaluate(() => { window.requestAnimationFrame = window.__kitFrame; });
			// the server session is still valid: the frame's sign-in finds it again
			await t.page.evaluate(() => window.aacFrameSession.recheck());
			await t.until(`round ${round}: Accounts is drawn again`, () => document.querySelectorAll('.prov[data-provider]').length >= 9, null, { timeout: 30000, hard: true });
		}
		await t.page.waitForTimeout(500);
		await t.equal('no uncaught error from a callback of the page that went', errors, []);
	},

	async 'accounts: every card is drawn natively, no Slint dashboard is loaded'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.check('nine provider cards and the settings cards', await t.page.evaluate(() =>
			document.querySelectorAll('.prov').length === 9 && !!document.querySelector('[data-probe="signin-card"]') && !!document.querySelector('[data-probe="update-card"]')));
		await t.check('no dashboard iframe, no bridge.js, no dashboard wasm', await t.page.evaluate(() => !document.querySelector('iframe.slint-frame') && !performance.getEntriesByType('resource').some((e) => /\/bridge\.js|ccs_account_dashboard/.test(e.name))));
		await t.check('Update apps spans the page above the columns', await t.page.evaluate(() => document.querySelector('.ac-page .ac-card')?.dataset.probe === 'update-card' && !!document.querySelector('.ac-top [data-probe="update-card"]')));
		await t.check('three columns at 1440', await t.page.evaluate(() => document.querySelectorAll('.ac-col').length === 3));
		await t.text(`${row('codex:one')} .acc-email`, 'codex.active@example.invalid');
		await t.check('the live Codex account carries no Active mark', await t.page.evaluate(() =>
			!document.querySelector('.acc-item[data-row="codex:one"] .active-mark, .acc-item[data-row="codex:one"].active')));
		await t.check('no account offers Activate any more', await t.page.evaluate(() => !document.querySelector('[data-probe^="activate:"]') && !document.querySelector('.active-mark')));
		await t.audit('accounts 1440');
	},

	async 'accounts: Codex device-code sign-in, from the name to Done'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click('[data-probe="add:codex"]');
		await t.page.waitForSelector('.prov[data-provider="codex"] .flow .field-input');
		await t.page.fill('.prov[data-provider="codex"] .flow .field-input', 'codex-3');
		await t.page.click('.prov[data-provider="codex"] .flow [data-probe="flow-submit:codex"]');
		await t.until('the code and the verification page show', () => document.querySelector('.prov[data-provider="codex"] .cb-text')?.textContent === 'ABCD-12345', null, { timeout: 15000 });
		await t.text('.prov[data-provider="codex"] .cb-url', 'https://auth.openai.com/codex/device');
		await t.visible('.prov[data-provider="codex"] [data-probe="flow-open-url:codex"]', 'Open verification page');
		await t.visible('.prov[data-provider="codex"] [data-probe="flow-copy:codex"]', 'Copy code');
		// both need the tap's own user activation, so the frame does them (the dashboard's hidden page could not)
		await t.page.context().route((u) => String(u).startsWith('https://auth.openai.com/'), (r) => r.fulfill({ status: 200, contentType: 'text/html', body: '<title>verification</title>' }));
		const popup = t.page.waitForEvent('popup', { timeout: 10000 });
		await t.page.click('.prov[data-provider="codex"] [data-probe="flow-open-url:codex"]');
		const opened = await popup;
		await t.equal('Open verification page opens it from the tap', new URL(opened.url()).origin, 'https://auth.openai.com');
		await opened.close();
		await t.page.click('.prov[data-provider="codex"] [data-probe="flow-copy:codex"]');
		await t.until('Copy code says what happened', () => [...document.querySelectorAll('.toast')].some((el) => /Code copied|Copy the code by hand/.test(el.textContent)), null, { timeout: 5000 });
		await t.until('the sign-in finishes', () => /is signed in as new\.codex@example\.invalid/.test(document.querySelector('.prov[data-provider="codex"] .flow-done')?.textContent || ''), null, { timeout: 15000 });
		const adds = changes(server).filter((r) => r.path === '/api/accounts/add');
		await t.equal('one add request with the typed profile name', JSON.stringify(adds.map((r) => r.body)), JSON.stringify([{ provider: 'codex', profileName: 'codex-3' }]));
		await t.page.click('.prov[data-provider="codex"] [data-probe="flow-done:codex"]');
		await t.until('Done closes the panel', () => !document.querySelector('.prov[data-provider="codex"] .flow'));
	},

	async 'accounts: an API key is sent once and leaves the field at once'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click('[data-probe="add-key:kimi-code"]');
		const key = '.prov[data-provider="kimi-code"] .flow input[type="password"]';
		await t.page.waitForSelector(key);
		await t.page.fill(key, 'sk-test-not-a-real-key-7c2e');
		await t.page.fill('.prov[data-provider="kimi-code"] .flow .label-field input', 'Work');
		await t.page.click('.prov[data-provider="kimi-code"] [data-probe="flow-submit:kimi-code"]');
		await t.equal('the field is empty right after the submit', await t.page.evaluate((sel) => document.querySelector(sel)?.value ?? '', key), '');
		await t.until('the key is stored', () => /Key stored, ending in 7c2e/.test(document.querySelector('.prov[data-provider="kimi-code"] .flow-done')?.textContent || ''), null, { timeout: 15000 });
		const adds = changes(server).filter((r) => r.path === '/api/accounts/add');
		await t.equal('one add request with the key and its label', JSON.stringify(adds.map((r) => r.body)), JSON.stringify([{ provider: 'kimi-code', key: 'sk-test-not-a-real-key-7c2e', label: 'Work' }]));
		await t.check('the key is nowhere on the page', await t.page.evaluate(() => !document.body.innerHTML.includes('sk-test-not-a-real-key')));
	},

	async 'accounts: a pasted code goes once and the field clears; a running job opens again after a reload'(t) {
		const job = { id: 'job_bbbbbbbbbbbbbbbb', provider: 'antigravity', kind: 'supervised-cli', mode: 'add', state: 'awaiting_code', profileName: 'agy-2', verification: { url: 'https://accounts.google.com/o/oauth2/auth', userCode: null, expiresAt: null } };
		const server = fakeServer({ runningJobs: [job] });
		server.s.jobs.set(job.id, [job]);
		await open(t, server);
		const field = '.prov[data-provider="antigravity"] .flow .field-input';
		await t.until('the running job opened its flow on load', () => !!document.querySelector('.prov[data-provider="antigravity"] .flow .field-input'), null, { timeout: 15000 });
		await t.page.fill(field, '4/0AbCdEf-test-code');
		await t.page.click('.prov[data-provider="antigravity"] [data-probe="flow-submit:antigravity"]');
		await t.equal('the code field is empty right after the submit', await t.page.evaluate((sel) => document.querySelector(sel)?.value ?? '', field), '');
		await t.until('it waits for the dashboard to save it', () => /Saving the sign-in/.test(document.querySelector('.prov[data-provider="antigravity"] .flow-wait')?.textContent || ''), null, { timeout: 15000 });
		const codes = changes(server).filter((r) => r.path.endsWith('/code'));
		await t.equal('one code request', JSON.stringify(codes.map((r) => r.body)), JSON.stringify([{ code: '4/0AbCdEf-test-code' }]));
	},

	async 'accounts: remove asks first, then sends its one-use token once'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click(`${row('codex:two')} [data-probe="remove:codex:two"]`);
		await t.until('the confirmation line names the effect', () => /Its saved Codex login is deleted\./.test(document.querySelector('.acc-item[data-row="codex:two"] .row-line.confirm')?.textContent || ''), null, { timeout: 10000 });
		await t.check('nothing is removed yet', !server.s.removed.has('codex:two'));
		await t.page.click(`${row('codex:two')} [data-probe="remove-commit:codex:two"]`);
		await t.until('the row goes', () => !document.querySelector('.acc-item[data-row="codex:two"]'), null, { timeout: 15000 });
		const removes = changes(server).filter((r) => r.path === '/api/accounts/codex%3Atwo/remove');
		await t.equal('an ask, then one commit with the token', JSON.stringify(removes.map((r) => r.body)), JSON.stringify([{}, { confirmationToken: 'tok-remove-codex:two' }]));
	},

	async 'accounts: restore from the trash asks first, then sends its token once'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click('[data-probe="restore:tr_0123456789abcdef"]');
		await t.until('the restore line names the effect', () => /moves back on Mac and Windows/.test(document.querySelector('.trash .row-line.confirm')?.textContent || ''), null, { timeout: 10000 });
		await t.page.click('[data-probe="restore-commit:tr_0123456789abcdef"]');
		await t.until('the trash row goes', () => !document.querySelector('.trash-head'), null, { timeout: 15000 });
		const restores = changes(server).filter((r) => r.path.endsWith('/restore'));
		await t.equal('an ask, then one commit with the token', JSON.stringify(restores.map((r) => r.body)), JSON.stringify([{}, { confirmationToken: 'tok-restore' }]));
	},

	async 'accounts: the live Codex login signs in again in place'(t) {
		const server = fakeServer();
		await open(t, server);
		const again = `${row('codex:one')} [data-probe="signin-again:codex:one"]`;
		await t.equal('Sign in again is enabled on the live row', await t.page.$eval(again, (el) => !el.disabled && el.getAttribute('aria-disabled') !== 'true'), true);
		await t.page.click(again);
		await t.until('the device code shows', () => document.querySelector('.prov[data-provider="codex"] .cb-text')?.textContent === 'WXYZ-67890', null, { timeout: 15000 });
		await t.equal('one sign-in request for the live row', JSON.stringify(changes(server).map((r) => r.path)), JSON.stringify(['/api/accounts/codex%3Aone/signin-again']));
		await t.equal('no refusal line', await t.page.$$eval(`${row('codex:one')} .row-line.refuse`, (els) => els.length), 0);
	},

	async 'accounts: the live Antigravity login signs in again in place'(t) {
		// The Antigravity row is one of the rich fixture's accounts.
		const server = fakeServer({ rich: true });
		await open(t, server);
		const again = `${row('antigravity:a1')} [data-probe="signin-again:antigravity:a1"]`;
		await t.equal('Sign in again is enabled on the live row', await t.page.$eval(again, (el) => !el.disabled && el.getAttribute('aria-disabled') !== 'true'), true);
		await t.page.click(again);
		await t.until('the Google sign-in page shows', () => document.querySelector('.prov[data-provider="antigravity"] .cb-url')?.textContent === 'https://accounts.google.com/o/oauth2/auth?fixture=a1', null, { timeout: 15000 });
		await t.equal('one sign-in request for the live row', JSON.stringify(changes(server).map((r) => r.path)), JSON.stringify(['/api/accounts/antigravity%3Aa1/signin-again']));
		await t.equal('no refusal line', await t.page.$$eval(`${row('antigravity:a1')} .row-line.refuse`, (els) => els.length), 0);
	},

	// Deep review 2026-11-26 (Medium): a refresh draws the new dashboard and inventory before the registry answers.
	// A saved Antigravity profile that has just become the live login on Ubuntu keeps the "renewed too" tip while the
	// registry's reply is held (its last reply had no refusal yet), and keeps it once the reply lands with the refusal.
	async 'accounts: a newly live Antigravity login keeps its renewal tip while the registry catches up'(t) {
		const server = fakeServer({ rich: true });
		const id = 'antigravity:profile:a1';
		let live = false;
		let hold = null;
		let registryReads = 0;
		const json = (body) => ({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
		const board = () => {
			const d = dashboard({ rich: true });
			const a = d.accounts.find((x) => x.id === 'antigravity:a1');
			Object.assign(a, { id, isActive: false, platform: 'ubuntu', capabilities: { antigravityProfileId: 'a1' } });
			return d;
		};
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		await serve(t, server);
		await t.page.route((u) => new URL(u).pathname === '/api/accounts/dashboard', (r) => r.fulfill(json(board())));
		await t.page.route((u) => new URL(u).pathname === '/api/antigravity/profiles', (r) => r.fulfill(json({ schemaVersion: 1, hostId: 'ubuntu',
			profiles: [{ id: 'a1', email: 'agy.workspace@example.invalid', plan: 'pro', hostId: 'ubuntu', available: true, selected: live }] })));
		await t.page.route((u) => new URL(u).pathname === '/api/accounts/registry', async (r) => {
			registryReads++;
			const refused = live;   // the answer as of when it was asked
			if (hold) await hold;
			const accounts = board().accounts.map((a) => ({ id: a.id, provider: a.provider, actions: { remove: true, signInAgain: true, replaceKey: true, recheck: true },
				removeRefusal: a.id === 'codex:one' || (a.id === id && refused) ? 'account_active' : null }));
			await r.fulfill(json({ providers: [], accounts, jobs: [], trash: [] })).catch(() => {});
		});
		await t.goto('/app/accounts?e2e', { settleMs: 300 });
		const again = `${row(id)} [data-probe="signin-again:${id}"]`;
		const tip = () => t.page.$eval(again, (el) => el.closest('[title]')?.getAttribute('title') || '').catch(() => '');
		await t.until('the saved Antigravity profile is drawn with its Sign in again', (sel) => !!document.querySelector(sel), again, { timeout: 45000, hard: true });
		await t.until('a saved, not live, profile is told to sign in from a terminal', (sel) => /Sign in from a terminal on Ubuntu/.test(document.querySelector(sel)?.closest('[title]')?.getAttribute('title') || ''), again, { timeout: 15000 });
		// it becomes the live login on Ubuntu; the next registry reply is held
		let release;
		hold = new Promise((resolve) => { release = resolve; });
		live = true;
		const reads = registryReads;
		await t.page.evaluate(() => window.aacFrameDashboard.accountsChanged());
		const deadline = Date.now() + 15000;
		while (registryReads === reads && Date.now() < deadline) await t.page.waitForTimeout(100);
		await t.check('the registry was asked again after the new dashboard and inventory', registryReads > reads);
		await t.check('while the registry is held, the newly live login says signing in renews the live login', /live Antigravity login there is renewed too/.test(await tip()), await tip());
		await t.check('its Remove is not refused yet (the registry has not said so)', await t.page.$$eval(`${row(id)} .row-line.refuse`, (els) => els.length) === 0);
		release();
		hold = null;
		await t.page.click(`${row(id)} [data-probe="remove:${id}"]`);
		await t.until('once the registry lands, Remove is refused for the live login', (sel) => /live login/.test(document.querySelector(sel)?.textContent || ''), `${row(id)} .row-line.refuse`, { timeout: 15000 });
		await t.check('and Sign in again still says it renews the live login', /live Antigravity login there is renewed too/.test(await tip()), await tip());
		await t.equal('no change was sent', changes(server).length, 0);
	},

	async 'accounts: a refused control says why under its row and sends nothing'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click(`${row('codex:one')} [data-probe="remove:codex:one"]`);
		await t.until('the remove refusal', () => /is the live login on its computer, so it cannot be removed\./.test(document.querySelector('.acc-item[data-row="codex:one"] .row-line.refuse')?.textContent || ''), null, { timeout: 10000 });
		await t.equal('no switch-first advice', /another account/i.test(await t.page.$eval('.acc-item[data-row="codex:one"] .row-line.refuse', (el) => el.textContent || '')), false);
		await t.equal('no change was sent', changes(server).length, 0);
	},

	async 'accounts: Details opens from a row, swaps, and closes'(t) {
		const server = fakeServer();
		await open(t, server);
		await t.page.click(`${row('codex:two')} .acc-email`);
		await t.until('Details shows the account', () => document.querySelector('.details .dt-title')?.textContent === 'codex.second@example.invalid', null, { timeout: 10000 });
		await t.check('its row is selected', await t.page.evaluate(() => document.querySelector('.acc-item[data-row="codex:two"]').classList.contains('sel')));
		await t.check('its windows are listed', await t.page.evaluate(() => document.querySelectorAll('.details .dm').length >= 2));
		await t.page.click(`${row('claude:one')} .acc-email`);
		await t.until('a row click swaps it', () => document.querySelector('.details .dt-title')?.textContent === 'one.claude@example.invalid', null, { timeout: 10000 });
		await t.visible('.details .dt-acts button:has-text("Open on Mac")', 'Claude Details has Open on Mac');
		await t.page.keyboard.press('Escape');
		await t.until('Escape closes it', () => !document.querySelector('.details'), null, { timeout: 5000 });
		await t.page.click(`${row('claude:one')} .acc-email`);
		await t.page.waitForSelector('.details');
		await t.page.click('[data-probe="details-close"]');
		await t.until('the close button closes it', () => !document.querySelector('.details'), null, { timeout: 5000 });
	},

	async 'accounts: an Update apps row shows the diagnostic summary and hides raw installer output'(t) {
		const server = fakeServer({ update: updateJob() });
		await open(t, server);
		await t.until('the Update apps table is open', () => !!document.querySelector('[data-probe="update-card"] .upd-table'), null, { timeout: 15000, hard: true });
		const cell = await t.page.evaluate(() => {
			const item = [...document.querySelectorAll('.ut-item')].find((i) => i.querySelector('.ut-app')?.textContent.trim() === 'Antigravity CLI');
			const cells = item ? [...item.querySelectorAll('.ut-cell')] : [];
			const failed = cells.find((c) => c.innerText.trim() === 'installer failed');
			return failed ? { title: failed.getAttribute('title') || '', crit: failed.classList.contains('w-crit') } : null;
		});
		await t.check('the failed cell says "installer failed" in red, not just "failed"', !!cell && cell.crit);
		const summary = "Installer output isn't saved; cause: its installer failed, exit code 1";
		await t.check('its tip names the cause, tool, exit code and step times',
			!!cell && cell.title.includes('Cause: its installer failed · ran agy · checking 900ms, updating 4.2s · ' + summary));
		await t.check('its tip hides saved command text and raw output',
			!!cell && !cell.title.includes('agy update') && !cell.title.includes('Error: connection reset by peer'));
		await t.page.evaluate(() => [...document.querySelectorAll('.ut-item')].find((i) => i.querySelector('.ut-app')?.textContent.trim() === 'Antigravity CLI')?.querySelector('button.ut-row')?.click());
		await t.until('its details show the diagnostic summary for Nas1', (text) => [...document.querySelectorAll('.ut-details li')].some((li) => li.textContent.includes('Nas1') && li.textContent.includes(text)), summary, { timeout: 5000 });
		await t.check('its details hide saved command text and raw output', await t.page.$eval('.ut-details', (el) =>
			!el.textContent.includes('agy update') && !el.textContent.includes('Error: connection reset by peer')));
		await t.check('rows without diagnostics keep their old words', await t.page.evaluate(() => [...document.querySelectorAll('.ut-cell')].some((c) => c.innerText.trim() === 'quit to finish')));
	},

	async 'accounts: Retry failed reruns only the last run\'s failed rows'(t) {
		const server = fakeServer({ update: updateJob() });
		await open(t, server);
		await t.until('the Update apps table is open', () => !!document.querySelector('[data-probe="update-card"] .upd-table'), null, { timeout: 15000, hard: true });
		await t.check('the head counts updated, current and failed rows', await t.page.evaluate(() => {
			const line = document.querySelector('[data-probe="update-card"] .uh-line')?.textContent || '';
			return /3 updated, 32 current, 1 failed/.test(line);
		}));
		await t.check('Retry failed is offered for the failed row', await t.page.evaluate(() => {
			const button = document.querySelector('[data-probe="update-retry"]');
			return !!button && !button.disabled && button.textContent.trim() === 'Retry failed';
		}));
		await t.shot('update-retry-before', 'Update apps counts and Retry failed before the retry');
		await t.page.click('[data-probe="update-retry"]');
		await t.until('the retry job draws only the failed computer', () => {
			const hosts = [...document.querySelectorAll('[data-probe="update-card"] .ut-host .cap')].map((h) => h.textContent.trim());
			return hosts.length === 1 && hosts[0] === 'Nas1';
		}, null, { timeout: 15000 });
		await t.check('the retried row is updated and Retry failed is gone', await t.page.evaluate(() => {
			const cell = document.querySelector('[data-probe="update-card"] .ut-cell');
			return cell?.innerText.trim() === '→ 1.5.9' && !document.querySelector('[data-probe="update-retry"]');
		}));
		await t.check('the page sent the retry action', changes(server).some((r) => r.path === '/api/app-updates/retry'));
		await t.shot('update-retry-after', 'After Retry failed: only Nas1 and Antigravity CLI, no retry offered');
	},

	// Device pass 2026-11-25 (item 5): on a 402 px iPhone the seven computer heads ran together ("WINDOWSUBUNTUNAS1")
	// and cells broke inside words ("curren/t", "install/er failed"). At phone and folded-Duo widths every head and
	// state stays inside its own column and no word is broken; every app x computer state is still shown, failures as
	// words. Desktop keeps its full words.
	// Fix round 2 (device recheck): the open Duo (669 px, a 640 px card) drew the wide table and cut its failures with an
	// ellipsis ("installer …", "quit to fi…"); a 760 px window did the same. Below the width where every computer column
	// holds its full words the table is compact, and no head, name or state is ever cut.
	// Deep review 2026-11-26: the real results are in the job (the Windows MSIX 26.1113.36452.0, the T3 nightly
	// 0.1.10-nightly.20261113.4616, a timed-out computer, the two Muse model-refresh failures), down to a 375 px phone.
	// No version, "timed out" or chip is cut: a version that does not fit says "updated" and keeps its full words in the
	// cell's label and the details; a failed model refresh reads as a failure that says whether the app updated; every
	// column's shown name, title and state agree before any cell is judged by the name above it.
	async 'accounts: Update apps on a phone: no head overlap, no word broken, every state shown'(t) {
		const sizes = [[375, 812, true, false, 'small phone'], [390, 844, true, false, 'phone'], [402, 874, true, false, 'phone'], [466, 678, true, true, 'folded Duo'], [669, 869, true, false, 'open Duo'], [760, 900, false, false, 'window']];
		const job = updateJob({ restartFailed: true, contract: true });
		const words = updateWords(job);
		const restartRow = job.results.find((r) => r.status === 'restart_failed' && !/not registered yet/.test(r.message));
		const measure = () => t.page.evaluate(measureUpdateTable, words);
		const appRow = (m, app) => m?.rows.find((r) => r.app === app);
		const toggle = (app) => t.page.evaluate((app) => [...document.querySelectorAll('.ut-item')].find((i) => i.querySelector('.ut-app')?.textContent.trim() === app)?.querySelector('button.ut-row')?.click(), app);
		for (const [width, height, phone, duo, what] of sizes) {
			const at = `${width} px`;
			await open(t, fakeServer({ update: job }), { width, height, phone, duo });
			await t.page.click('[data-probe="update-card"] .upd-head');
			await t.until(`${at}: the Update apps table opens`, () => !!document.querySelector('[data-probe="update-card"] .upd-table'), null, { timeout: 10000, hard: true });
			await t.until(`${at}: the card has stopped moving`, () => document.querySelector('[data-probe="update-card"]').getAnimations({ subtree: true }).every((a) => a.playState !== 'running'), null, { timeout: 5000 });
			await t.page.evaluate(() => document.fonts.ready);
			const m = await measure();
			await t.check(`${at}: seven computers are too many for full words here, so the table is compact`, m?.compact === true);
			await t.check(`${at}: no head, name or state is cut off`, !!m && m.cut.length === 0, m && `cut: ${m.cut.join(', ')}`);
			await t.check(`${at}: no word in the table is broken across lines`, !!m && m.broken.length === 0, m && `broken: ${m.broken.join(', ')}`);
			await t.check(`${at}: every word stays inside its own column`, !!m && m.spill.length === 0, m && `spills: ${m.spill.join(', ')}`);
			await t.check(`${at}: the computer heads do not overlap`, !!m && !m.overlap && m.hosts.length === 7 && m.hosts.every(Boolean), m && JSON.stringify(m.hosts));
			const heads = updateHeadMismatches(m, job);
			await t.check(`${at}: every column shows its computer's name, title and state, in order`, heads.length === 0, heads.slice(0, 6).join('; '));
			await t.check(`${at}: every app shows a state on every computer`, !!m && m.rows.length >= 10 && m.rows.every((r) => r.app && r.cells.length === 7 && r.cells.every((c) => c.text)),
				m && JSON.stringify(m.rows.filter((r) => !r.app || r.cells.length !== 7 || r.cells.some((c) => !c.text)).map((r) => r.app)));
			const agy = appRow(m, 'Antigravity CLI');
			await t.check(`${at}: the failed update is shown as a red word with its cause in the tip`, !!agy && agy.cells.some((c) => c.crit && c.text === 'failed' && c.tip.startsWith('Installer failed')));
			await t.check(`${at}: the step for the user is shown as a word`, !!m && m.rows.some((r) => r.app === 'Codex Desktop' && r.cells.some((c) => c.warn && c.text === 'quit')));
			await t.check(`${at}: a computer without results says so once, under its head`, !!m && m.hosts.filter((h) => /no results$/.test(h)).length === 3);
			await t.check(`${at}: no sideways page scroll`, !!m && !m.sideways);
			// Review r1 (Medium), deep review (Medium): each cell is judged under the computer whose visible head name it is
			// drawn beneath, so a failure under the wrong computer fails, by cells or by heads.
			const want = expectedUpdateCells(job, m?.compact !== false);
			const bad = updateCellMismatches(m, want);
			await t.check(`${at}: every cell says what the run reported for its app on the computer above it`, bad.length === 0, bad.slice(0, 6).join('; '));
			const swap = () => t.page.evaluate(() => {
				const item = [...document.querySelectorAll('.ut-item')].find((i) => i.querySelector('.ut-app')?.textContent.trim() === 'Antigravity CLI');
				const cells = [...item.querySelectorAll('.ut-cell')];
				const [a, b] = [cells[2], cells[3]];   // Ubuntu, Nas1
				const mark = document.createComment('swap');
				a.replaceWith(mark); b.replaceWith(a); mark.replaceWith(b);
			});
			await swap();
			const swapped = updateCellMismatches(await measure(), want);
			await swap();
			await t.check(`${at}: the cell check catches a failure moved under the wrong computer`,
				swapped.some((b) => b.startsWith('Antigravity CLI | Ubuntu: "failed"')) && swapped.some((b) => b.startsWith('Antigravity CLI | Nas1: "✓"')), swapped.join('; '));
			// only the shown Ubuntu and Nas1 names trade places; their tips stay
			const swapNames = () => t.page.evaluate(() => {
				const heads = [...document.querySelectorAll('[data-probe="update-card"] .ut-host')];
				const [a, b] = [heads[2], heads[3]];   // Ubuntu, Nas1
				for (const sel of ['.ut-hname .cap', '.ut-hs']) {
					const [x, y] = [a.querySelector(sel), b.querySelector(sel)];
					[x.textContent, y.textContent] = [y.textContent, x.textContent];
				}
			});
			await swapNames();
			const renamed = await measure();
			await swapNames();
			const renamedCells = updateCellMismatches(renamed, want);
			const renamedHeads = updateHeadMismatches(renamed, job);
			await t.check(`${at}: swapping only the shown computer names puts Nas1's failure under "Ubuntu" and is caught`,
				renamedCells.some((b) => b.startsWith('Antigravity CLI | Ubuntu: "failed"')) && renamedHeads.some((b) => b.startsWith('column 3:')) && renamedHeads.some((b) => b.startsWith('column 4:')),
				[...renamedHeads, ...renamedCells].slice(0, 8).join('; '));
			// Odroid's "no results" moved under Ubuntu
			const moveNone = (back) => t.page.evaluate((back) => {
				const heads = [...document.querySelectorAll('[data-probe="update-card"] .ut-host')];
				const odroid = heads.find((h) => h.title === 'Odroid');
				const ubuntu = heads.find((h) => h.title === 'Ubuntu');
				const [from, to] = back ? [ubuntu, odroid] : [odroid, ubuntu];
				for (const el of [...from.children].filter((c) => !c.matches('.ut-hname'))) to.append(el);
			}, back);
			await moveNone(false);
			const moved = updateHeadMismatches(await measure(), job);
			await moveNone(true);
			await t.check(`${at}: a "no results" moved under another computer is caught`,
				moved.some((b) => b.includes('(Ubuntu): state')) && moved.some((b) => b.includes('(Odroid): state')), moved.join('; '));
			const restored = await measure();
			await t.check(`${at}: the swaps were put back`, updateCellMismatches(restored, want).join('; ') === bad.join('; ') && updateHeadMismatches(restored, job).join('; ') === heads.join('; '));
			// Deep review (High): long versions never cut; "updated" where one does not fit, the version in the label
			for (const r of job.results.filter((r) => r.status === 'updated' && r.version.length > 8)) {
				const cell = appRow(m, r.appLabel)?.cells.find((c) => c.host === HOST_LABELS[r.platform]);
				await t.check(`${at}: ${r.appLabel} on ${HOST_LABELS[r.platform]} names ${r.version} only if it fits, else says "updated", and keeps the version in its label and tip`,
					!!cell && (cell.text === 'updated' || cell.text === `→${r.version}`) && cell.label === `Updated to ${r.version}` && cell.tip.includes(r.version), JSON.stringify(cell));
			}
			const timedOut = appRow(m, 'OMP')?.cells.find((c) => c.host === 'Nas1');
			await t.check(`${at}: the timed-out computer reads "timed out" in full, as a warning`, timedOut?.text === 'timed out' && timedOut.warn && timedOut.label === 'Timed out', JSON.stringify(timedOut));
			// Deep review (High): both Muse model-refresh failures are failures, told apart
			const muse = appRow(m, 'Muse Code');
			const moved2 = muse?.cells.find((c) => c.host === 'Ubuntu');
			const stale = muse?.cells.find((c) => c.host === 'Mac');
			await t.check(`${at}: Ubuntu's Muse (updated, models failed) and Mac's (models failed) read as two different red failures`,
				moved2?.crit && stale?.crit && moved2.text === 'updated models failed' && stale.text === 'models failed'
					&& moved2.label === 'Updated, model refresh failed' && stale.label === 'Model refresh failed', JSON.stringify([moved2, stale]));
			await toggle('Muse Code');
			await t.until(`${at}: Muse Code's details give each computer's full result and the repair step`, () => {
				const li = [...document.querySelectorAll('.ut-details li')].map((l) => l.textContent);
				return li.some((l) => l.startsWith('Ubuntu') && l.includes('Updated, model refresh failed') && l.includes('1.2.3 to 1.2.4') && l.includes('Open Muse once'))
					&& li.some((l) => l.startsWith('Mac') && l.includes('Model refresh failed') && l.includes('Open Muse once'));
			}, null, { timeout: 5000 });
			await toggle('Muse Code');
			await t.until(`${at}: Muse Code's details close`, () => !document.querySelector('.ut-details'), null, { timeout: 5000 });
			// Review r1 (High): a failed restart reads as a failure, never as "restart" (an instruction)
			const restart = appRow(m, restartRow.appLabel)?.cells.find((c) => c.host === 'Windows');
			await t.check(`${at}: a failed restart shows "failed" in red`, restart?.text === 'failed' && restart.crit, JSON.stringify(restart));
			await t.check(`${at}: its cell still says "restart failed" to screen readers and in its tip`,
				restart?.full === 'restart failed' && restart.label === 'Updated, restart failed' && restart.tip.startsWith('Updated, restart failed'), JSON.stringify(restart));
			await toggle('Codex Desktop');
			await t.until(`${at}: Codex Desktop's details give Windows' full version`, () => [...document.querySelectorAll('.ut-details li')].some((li) => li.textContent.startsWith('Windows') && li.textContent.includes('26.1101.9.0 to 26.1113.36452.0')), null, { timeout: 5000 });
			await toggle(restartRow.appLabel);
			await t.until(`${at}: ${restartRow.appLabel}'s details say Windows' restart failed`, () => [...document.querySelectorAll('.ut-details li')].some((li) => li.textContent.startsWith('Windows') && li.textContent.includes('restart failed')), null, { timeout: 5000 });
			await t.until(`${at}: the details have stopped moving`, () => document.querySelector('[data-probe="update-card"]').getAnimations({ subtree: true }).every((a) => a.playState !== 'running'), null, { timeout: 5000 });
			await t.check(`${at}: the open details keep every word whole`, await t.page.evaluate(() => {
				const uls = [...document.querySelectorAll('.ut-details')];
				return uls.length === 2 && uls.every((ul) => ul.scrollWidth <= ul.clientWidth + 1) && document.documentElement.scrollWidth <= innerWidth + 1;
			}));
			await t.shot(`update-apps-phone-${width}-details`, `Codex Desktop and ${restartRow.appLabel} open on a ${width} px ${what}: the full MSIX version, and Windows says its restart failed`);
			await toggle('Codex Desktop');
			await toggle(restartRow.appLabel);
			await t.until(`${at}: the details close`, () => !document.querySelector('.ut-details'), null, { timeout: 5000 });
			await t.shot(`update-apps-phone-${width}`, `Update apps open on a ${width} px ${what}`);
		}
	},

	// 1920 stays as it was (both long versions fit there); 1280 is the narrowest common desktop where seven computers
	// still get the wide table, whose longer words (the no-results line) wrap between words instead of being cut.
	async 'accounts: Update apps on a 1920 desktop keeps its full words in one line per app'(t) {
		for (const [width, height] of [[1920, 1080], [1280, 800]]) {
			const at = `${width} px`;
			const job = updateJob({ restartFailed: true, contract: true });
			await open(t, fakeServer({ update: job }), { width, height });
			await t.until(`${at}: the Update apps table is open`, () => !!document.querySelector('[data-probe="update-card"] .upd-table'), null, { timeout: 15000, hard: true });
			await t.page.evaluate(() => document.fonts.ready);
			const m = await t.page.evaluate(measureUpdateTable, updateWords(job));
			const bad = updateCellMismatches(m, expectedUpdateCells(job, false));
			await t.check(`${at}: every cell says its full words under the computer above it ("restart failed" on Windows)`, bad.length === 0, bad.slice(0, 6).join('; '));
			const heads = updateHeadMismatches(m, job);
			await t.check(`${at}: every column shows its computer's full name, title and state, in order`, heads.length === 0, heads.slice(0, 6).join('; '));
			await t.check(`${at}: the App column head is shown`, m?.appHead === 'App');
			await t.check(`${at}: a desktop has room for every full word, so the table is wide`, m?.compact === false);
			await t.check(`${at}: no head, name or state is cut off`, !!m && m.cut.length === 0, m && `cut: ${m.cut.join(', ')}`);
			await t.check(`${at}: the computers keep their full names`, !!m && m.hosts.slice(0, 2).join('|') === 'Mac|Windows' && m.hosts.some((h) => h.startsWith('Alexis PC')));
			await t.check(`${at}: cells keep their full words`, !!m && m.rows.some((r) => r.cells.some((c) => c.text === 'current')) && m.rows.some((r) => r.cells.some((c) => c.text === 'quit to finish')));
			await t.check(`${at}: no word broken and nothing spills`, !!m && m.broken.length === 0 && m.spill.length === 0 && !m.overlap, m && `${m.broken} | ${m.spill}`);
			if (width === 1920) {
				const cells = m?.rows.flatMap((r) => r.cells) || [];
				await t.check(`${at}: the MSIX and the T3 nightly are named in full`, cells.some((c) => c.text === '→ 26.1113.36452.0') && cells.some((c) => c.text === '→ 0.1.10-nightly.20261113.4616'));
			}
			await t.shot(`update-apps-desktop-${width}`, `Update apps on a ${width} desktop`);
		}
	},

	async 'accounts: on a phone the cards stack, Update apps folds to its line, nothing runs sideways'(t) {
		const server = fakeServer({ update: updateJob() });
		await open(t, server, { width: 390, height: 844, phone: true });
		await t.check('one column', await t.page.evaluate(() => document.querySelectorAll('.ac-col').length === 1));
		await t.check('Update apps first and folded', await t.page.evaluate(() => {
			const card = document.querySelector('.ac-col .ac-card');
			return card?.dataset.probe === 'update-card' && card.querySelector('.upd-head')?.getAttribute('aria-expanded') === 'false';
		}));
		await t.check('no sideways page scroll', await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
		await t.check('Retry failed is offered and fits the phone', await t.page.evaluate(() => {
			const button = document.querySelector('[data-probe="update-retry"]');
			if (!button || !button.offsetParent) return false;
			const rect = button.getBoundingClientRect();
			return rect.width > 0 && rect.left >= 0 && rect.right <= innerWidth;
		}));
		await t.shot('update-retry-phone', 'Update apps on a 390 px phone: the counts line and Retry failed fit');
		await t.audit('accounts phone');
		await t.page.click(`${row('codex:two')} .acc-email`);
		await t.until('Details is a bottom sheet', () => {
			const d = document.querySelector('.details');
			return !!d && Math.abs(d.getBoundingClientRect().bottom - innerHeight) < 2 && d.getBoundingClientRect().left === 0;
		}, null, { timeout: 10000 });
	},

	async 'accounts: Details reaches the right edge beside a left rail'(t) {
		const server = fakeServer();
		await open(t, server, { width: 678, height: 466, phone: true, duo: true, safe: '0,0,34,84' });
		await t.page.click(`${row('codex:two')} .acc-email`);
		await t.until('Details shows the account', () => document.querySelector('.details .dt-title')?.textContent === 'codex.second@example.invalid', null, { timeout: 10000 });
		const rect = await t.page.evaluate(() => {
			const box = (el) => { const r = el.getBoundingClientRect(); return { x: r.x, r: r.right, y: r.y, w: r.width }; };
			return { details: box(document.querySelector('.details')), rail: box(document.querySelector('.rail')), w: innerWidth };
		});
		await t.check('Details starts at the rail and reaches the right edge, with no rail-width gap', rect.details.x >= rect.rail.r - 0.5 && Math.abs(rect.details.r - rect.w) < 1, JSON.stringify(rect));
		await t.page.keyboard.press('Escape');
	},
};
