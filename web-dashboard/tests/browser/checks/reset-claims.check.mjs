// Rate-limit resets in Details (docs/reset-claims.md, leptos/src/resets.rs): the Resets and balances fact group, the
// confirm dialog, the result in place, the history, and the phone and Duo sheet. Every reset route is answered in the
// page by a fixture (made-up accounts, keys and labels). A reset route the fixture doesn't answer is aborted, never
// passed to the server, so no check can reach a real claim. Nothing here spends anything.
import { fakeServer, serve, changes, at } from '../accounts-fixture.mjs';

const DAY = 24 * 60;
const quota = (key, label, used, minutes, reset) => ({ key, label, usedPercent: used, remainingPercent: 100 - used, resetAt: at(reset), windowMinutes: minutes });
const grantWindow = (n, state, left, days, clears) => ({ key: `reset_credit_${state}_grant_${n}`, label: `x ${n}`, kind: 'balance', unit: 'resets', remaining: left, limit: 1, used: 1 - left, expiresAt: at(days * DAY), clears });
const account = (id, provider, email, windows, extra = {}) => ({
	id, provider, providerLabel: provider, label: email, email, plan: 'max', platform: 'windows', host: 'windows', source: 'fixture', status: 'cached',
	message: 'Showing the last successful reading; live usage is unavailable.', fetchedAt: at(-1), sampledAt: at(-1), isActive: false, hidden: false,
	trayHidden: false, windows, capabilities: {}, signInNeeded: [], ...extra,
});
const CLAUDE = account('claude:one', 'claude', 'one.claude@example.invalid', [
	quota('seven_day', 'Weekly usage', 84, 10080, 6 * DAY),
	{ key: 'reset_credits_available', label: 'Rate-limit resets available', kind: 'balance', unit: 'resets', remaining: 2 },
	grantWindow(1, 'available', 1, 29, ['seven_day', 'seven_day_overage_included']),
	grantWindow(2, 'available', 1, 34, ['seven_day', 'seven_day_overage_included']),
	grantWindow(3, 'used', 0, 118, ['seven_day_overage_included']),
	{ key: 'extra_usage', label: 'Extra usage', kind: 'extra_usage', enabled: false },
], { capabilities: { claudeProfileId: 'one', claudePlatforms: ['windows'] } });
const ZAI = account('zai:z1', 'zai', 'zai.coding.plan@example.invalid', [
	quota('five_hour', 'Five-hour usage', 27, 300, 120),
	{ key: 'reset-packs-5h', label: '5-hour reset packs', kind: 'balance', unit: 'packs', remaining: 2, used: 1, limit: 3, expiresAt: at(3 * DAY) },
]);
const QWEN = account('qwen:q1', 'qwen', 'qwen.token.plan@example.invalid', [quota('monthly', 'Monthly usage', 41, 43200, 15000)]);
const DASHBOARD = {
	schemaVersion: 1, updatedAt: at(-1), accounts: [CLAUDE, ZAI, QWEN],
	providers: ['claude', 'codex', 'antigravity', 'cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go'].map((id) => ({ id, accountCount: id === 'claude' || id === 'zai' || id === 'qwen' ? 1 : 0, signIn: { available: true }, capabilities: {} })),
	settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: [], trayHiddenProviders: [], trayHiddenAccountIds: [] },
	codexAutoSwitch: { enabled: false, thresholdPercent: 5 },
};

const KEY = (c) => c.repeat(64);
const MS = (days) => Date.now() + days * 86_400_000;
const grant = (key, kind, label, left, days, claimable, reason = null, words = null) => ({
	grantKey: key, kind, label, refillsText: kind === 'unknown' ? 'AAC does not know what this refills yet.' : 'Refills your weekly allowance.',
	providerWords: words, resetsLeft: left, expiresAtMs: days === null ? null : MS(days), claimable, claimBlockedReason: reason,
});
/** The inventory, before and after a confirmed claim of the first grant. */
const inventory = (spent = false, pending = null) => ({
	status: 'ok', observedAtMs: Date.now(), sourceAgeMs: 900, pendingAttempt: pending,
	grants: [
		grant(KEY('a'), 'weekly', 'Weekly reset', spent ? 0 : 1, 29, !spent),
		grant(KEY('b'), 'weekly', 'Weekly reset', 1, 34, true),
		grant(KEY('c'), 'fable', 'Fable reset', 0, 118, false, 'grant-unavailable'),
		grant(KEY('d'), 'unknown', 'Other reset', 1, 40, false, 'unknown-grant-kind', 'seven_day_opus'),
	],
	history: spent ? [
		{ atMs: Date.now() - 5000, event: 'attempt', targetHash: KEY('e'), outcome: 'claimed-unconfirmed', details: { kind: 'weekly', idempotencyHash: KEY('f') } },
		{ atMs: Date.now() - 4000, event: 'verification', targetHash: KEY('e'), outcome: 'claimed', details: { kind: 'weekly', idempotencyHash: KEY('f') } },
	] : [],
});

/** The reset routes, answered in the page. `claim` picks the claim's answer; every request is recorded. */
function resetServer({ claim = 'claimed' } = {}) {
	const server = fakeServer();
	const r = { spent: false, pending: null, claim };
	const json = (status, payload) => ({ status, contentType: 'application/json', body: JSON.stringify(payload) });
	server.routes.unshift(
		['GET', /^\/api\/accounts\/dashboard$/, () => json(200, DASHBOARD)],
		['GET', /^\/api\/accounts\/registry$/, () => json(200, {
			providers: [], jobs: [], trash: [],
			accounts: DASHBOARD.accounts.map((a) => ({ id: a.id, provider: a.provider, actions: { remove: true, signInAgain: true, replaceKey: true, recheck: true }, removeRefusal: null })),
		})],
		['GET', /^\/api\/accounts\/claude%3Aone\/reset-claims$/, () => json(200, inventory(r.spent, r.pending))],
		['POST', /^\/api\/accounts\/claude%3Aone\/reset-claims\/prepare$/, (body) => json(200, {
			account: { label: 'Claude: fresh.label@example.invalid (Max)' }, confirmationToken: KEY('1'), expiresAtMs: Date.now() + 120_000,
			grant: inventory().grants.find((g) => g.grantKey === body.grantKey), costText: 'Spends one selected reset. This cannot be undone.',
		})],
		['POST', /^\/api\/accounts\/claude%3Aone\/reset-claims$/, (body) => {
			if (body.confirmationToken !== KEY('1')) return json(409, { code: 'reset-state-unavailable', action: 'check-again', reason: 'confirmation-invalid', sent: false });
			if (r.claim === 'expired') return json(409, { code: 'reset-state-unavailable', action: 'check-again', reason: 'confirmation-invalid', sent: false });
			if (r.claim === 'unconfirmed') {
				r.pending = KEY('9');
				return json(200, { attemptKey: KEY('9'), outcome: 'claimed-unconfirmed', verification: 'unconfirmed', providerSuccess: true, windows: [] });
			}
			r.spent = true;
			return json(200, { attemptKey: KEY('9'), outcome: 'claimed', verification: 'confirmed', providerSuccess: true, windows: [{ id: 'seven_day', usedPercent: 0 }] });
		}],
		['POST', /^\/api\/accounts\/claude%3Aone\/reset-claims\/check$/, () => {
			r.pending = null;
			r.spent = true;
			return json(200, { attemptKey: KEY('9'), outcome: 'claimed', verification: 'confirmed', providerSuccess: true, windows: [{ id: 'seven_day', usedPercent: 3 }] });
		}],
	);
	return { server, r };
}

/** Serves the fixture, and aborts any reset route it doesn't answer (never the real server). */
async function serveResets(t, server) {
	await serve(t, server);
	await t.page.route((u) => /\/reset-claims/.test(new URL(u).pathname), async (route) => {
		const url = new URL(route.request().url());
		for (const [method, re, answer] of server.routes) {
			const m = url.pathname.match(re);
			if (route.request().method() === method && m) {
				let body = {};
				try { body = route.request().postData() ? JSON.parse(route.request().postData()) : {}; } catch {}
				server.s.requests.push({ method, path: url.pathname, body });
				return route.fulfill(answer(body, m));
			}
		}
		server.s.requests.push({ method: route.request().method(), path: url.pathname, body: null, aborted: true });
		return route.abort();
	});
}

async function open(t, server, { width = 1440, height = 900, phone = false, duo = false, safe = '' } = {}) {
	await t.newContext({ viewport: { width, height }, ...(duo ? { screen: { width: 466, height: 678 } } : {}), ...(phone ? { isMobile: true, hasTouch: true } : {}), deviceScaleFactor: 1, serviceWorkers: 'block' });
	await t.app.signIn();
	await serveResets(t, server);
	await t.goto(`/app/accounts?e2e${safe ? `&safe=${safe}` : ''}`, { settleMs: 300 });
	await t.until('Accounts drawn', () => document.querySelectorAll('.prov[data-provider]').length >= 9, null, { timeout: 45000, hard: true });
}

async function openDetails(t, id) {
	await t.page.click(`.acc-item[data-row="${id}"] .acc-email`);
	await t.until('Details open', (want) => !!document.querySelector('.details [data-probe="resets"]') && document.querySelector('.details')?.getAttribute('aria-label')?.length > 0, id, { timeout: 10000 });
}

const resetRequests = (server) => server.s.requests.filter((q) => q.path.includes('/reset-claims'));
const claims = (server) => resetRequests(server).filter((q) => q.method === 'POST' && /\/reset-claims$/.test(q.path));
const tableRows = (t) => t.page.evaluate(() => [...document.querySelectorAll('.details .resets-table tbody tr')].map((tr) => ({
	probe: tr.dataset.probe, spent: tr.classList.contains('spent'), text: tr.innerText.replace(/\s+/g, ' ').trim(), button: !!tr.querySelector('[data-probe="reset-use"]'),
})));
const line = (t) => t.page.evaluate(() => document.querySelector('.details [data-probe="reset-result"]')?.innerText.trim() || '');

export const tests = {
	async 'resets: the fact group, one row per grant, used last, unknown kinds unusable'(t) {
		const { server } = resetServer();
		await open(t, server);
		await openDetails(t, 'claude:one');
		await t.until('the inventory replaced the reading rows', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
		await t.text('.details [data-probe="resets"] .fg-sum b', '2 rate-limit resets ready.');
		const rows = await tableRows(t);
		await t.check('four rows: two usable, the unknown one, the used one last', rows.length === 4
			&& rows[0].button && rows[1].button && !rows[2].button && !rows[3].button && rows[3].spent, JSON.stringify(rows));
		await t.check('the unknown grant shows the provider\'s words and why it cannot be used',
			rows[2].text.includes('seven_day_opus') && rows[2].text.includes("AAC doesn't know what this refills yet; it can't be used from here"), rows[2].text);
		await t.check('opening Details read the inventory only', resetRequests(server).every((q) => q.method === 'GET'), JSON.stringify(resetRequests(server)));
		await t.visible('.details [data-probe="details-reading"]', 'Reading is its own fact group');
		await t.shot('resets-desktop', 'Resets and balances: 2 ready rows with Use reset, the unknown kind in quotes with its sentence, the used row grey at the bottom; Extra usage as a pair; Reading as Read from / Profile');
	},

	async 'resets: the dialog asks prepare only; Escape and Cancel close it and send nothing'(t) {
		const { server } = resetServer();
		await open(t, server);
		await openDetails(t, 'claude:one');
		await t.until('usable', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
		await t.page.click('.details [data-probe="reset-use"]');
		await t.until('the dialog shows', () => !!document.querySelector('[data-probe="reset-dialog"]'), null, { timeout: 5000 });
		await t.text('[data-probe="reset-dialog-account"]', 'Claude: fresh.label@example.invalid (Max)');
		await t.check('the dialog names the cost', await t.page.evaluate(() => document.querySelector('[data-probe="reset-dialog"]').innerText.includes("You have 2 resets; this spends one and can't be undone.")));
		await t.check('prepare named the first row\'s exact grant', resetRequests(server).some((q) => q.path.endsWith('/prepare') && q.body.grantKey === KEY('a')));
		await t.shot('resets-dialog', 'the confirm dialog: the fresh account label, Weekly reset, what it refills, the expiry, the cost sentence, the countdown, Cancel / Use this reset');
		await t.page.keyboard.press('Escape');
		await t.until('Escape closes the dialog only', () => !document.querySelector('[data-probe="reset-dialog"]') && !!document.querySelector('.details'), null, { timeout: 3000 });
		await t.page.click('.details [data-probe="reset-use"]');
		await t.until('the dialog shows again', () => !!document.querySelector('[data-probe="reset-dialog"]'), null, { timeout: 5000 });
		await t.page.click('[data-probe="reset-dialog-cancel"]');
		await t.until('Cancel closes it', () => !document.querySelector('[data-probe="reset-dialog"]') && !!document.querySelector('.details'), null, { timeout: 3000 });
		await t.check('no claim was sent', claims(server).length === 0, JSON.stringify(changes(server)));
	},

	async 'resets: Use this reset sends one claim and the panel shows the verification read'(t) {
		const { server } = resetServer();
		await open(t, server);
		await openDetails(t, 'claude:one');
		await t.until('usable', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
		await t.page.click('.details [data-probe="reset-use"]');
		await t.until('the dialog shows', () => !!document.querySelector('[data-probe="reset-dialog-use"]'), null, { timeout: 5000 });
		await t.page.click('[data-probe="reset-dialog-use"]');
		await t.until('the result shows in place', () => /^Reset applied: Weekly is back to 0% used\./.test(document.querySelector('.details [data-probe="reset-result"]')?.innerText || ''), null, { timeout: 5000 });
		const sent = claims(server);
		await t.check('exactly one claim, naming the chosen grant and its confirmation', sent.length === 1 && sent[0].body.grantKey === KEY('a') && sent[0].body.confirmationToken === KEY('1'), JSON.stringify(sent));
		await t.until('the fresh read shows the grant used', () => document.querySelector('.details [data-probe="resets"] .fg-sum b')?.textContent === '1 rate-limit reset ready.', null, { timeout: 5000 });
		await t.visible('.details [data-probe="reset-history"]', 'the claim is in the Reset claims history');
		await t.shot('resets-applied', 'after the claim: "Reset applied: Weekly is back to 0% used." under the table, 1 ready, the used grant grey at the bottom, Reset claims history with one row');
	},

	async 'resets: an expired confirmation says so; an unconfirmed claim blocks another until Check again'(t) {
		const { server, r } = resetServer({ claim: 'expired' });
		await open(t, server);
		await openDetails(t, 'claude:one');
		await t.until('usable', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
		await t.page.click('.details [data-probe="reset-use"]');
		await t.until('the dialog shows', () => !!document.querySelector('[data-probe="reset-dialog-use"]'), null, { timeout: 5000 });
		await t.page.click('[data-probe="reset-dialog-use"]');
		await t.until('the expiry is named', () => (document.querySelector('.details [data-probe="reset-result"]')?.innerText || '').includes('This confirmation expired, open it again.'), null, { timeout: 5000 });
		r.claim = 'unconfirmed';
		await t.page.click('.details [data-probe="reset-use"]');
		await t.until('the dialog shows', () => !!document.querySelector('[data-probe="reset-dialog-use"]'), null, { timeout: 5000 });
		await t.page.click('[data-probe="reset-dialog-use"]');
		await t.until('the unconfirmed words', () => (document.querySelector('.details [data-probe="reset-result"]')?.innerText || '').includes('Reset was applied, but AAC could not confirm the new limits. Check again.'), null, { timeout: 5000 });
		await t.check('every Use reset waits on it', await t.page.evaluate(() => [...document.querySelectorAll('.details [data-probe="reset-use"]')].every((b) => b.disabled)));
		await t.shot('resets-unconfirmed', 'the unconfirmed claim: the sentence and Check again under the table, every Use reset disabled');
		const before = claims(server).length;
		await t.page.click('.details [data-probe="resets-check"]');
		await t.until('Check again settles it', () => (document.querySelector('.details [data-probe="reset-result"]')?.innerText || '').startsWith('Reset applied: Weekly is back to 3% used.'), null, { timeout: 5000 });
		await t.check('Check again sent only a check, never a claim', claims(server).length === before
			&& resetRequests(server).some((q) => q.path.endsWith('/check') && q.body.attemptKey === KEY('9')));
	},

	async 'resets: Z.ai lists its cards with Use in Z.ai, Qwen has none, neither reads the claim routes'(t) {
		const { server } = resetServer();
		await open(t, server);
		await openDetails(t, 'zai:z1');
		await t.text('.details [data-probe="resets"] .fg-sum b', '2 reset cards ready.');
		await t.visible('.details [data-probe="resets"] a[href^="https://z.ai/"]', 'Use in Z.ai');
		await t.check('no Use reset for Z.ai', await t.page.evaluate(() => !document.querySelector('.details [data-probe="reset-use"]')));
		await t.page.keyboard.press('Escape');
		await openDetails(t, 'qwen:q1');
		await t.text('.details [data-probe="resets"] .fg-sum b', 'No resets on this plan.');
		await t.check('neither read a reset route', resetRequests(server).length === 0, JSON.stringify(resetRequests(server)));
	},

	async 'resets: on a 390 phone and the folded Duo the sheet keeps every button reachable and unclipped'(t) {
		for (const pose of [{ name: 'phone-390', width: 390, height: 844, phone: true, safe: '0,0,34,0' }, { name: 'duo-folded', width: 466, height: 678, phone: true, duo: true, safe: '0,84,34,0' }]) {
			await t.step(pose.name, async () => {
				const { server } = resetServer();
				await open(t, server, pose);
				await openDetails(t, 'claude:one');
				await t.until('usable', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
				await t.page.evaluate(() => document.querySelector('.details [data-probe="resets"]').scrollIntoView({ block: 'start' }));
				await t.page.waitForTimeout(300);
				const fit = await t.page.evaluate(() => {
					const st = getComputedStyle(document.documentElement);
					const right = innerWidth - (parseFloat(st.getPropertyValue('--safe-right')) || 0);
					const table = document.querySelector('.details .resets-table');
					const buttons = [...document.querySelectorAll('.details [data-probe="reset-use"]')].map((b) => b.getBoundingClientRect());
					const clipped = [...document.querySelectorAll('.details .fg *')].filter((el) => el.scrollWidth > el.clientWidth + 1 && getComputedStyle(el).overflowX === 'hidden').length;
					return { wide: table.scrollWidth > table.parentElement.clientWidth + 1, buttons: buttons.map((r) => [Math.round(r.left), Math.round(r.right), Math.round(r.height)]), inside: buttons.every((r) => r.left >= 0 && r.right <= right + 0.5 && r.height >= 24), clipped, right };
				});
				await t.check(`${pose.name}: the table fits the sheet`, !fit.wide, JSON.stringify(fit));
				await t.check(`${pose.name}: every Use reset is inside the screen and at least 24 px tall`, fit.inside, JSON.stringify(fit));
				await t.check(`${pose.name}: no clipped text in the fact groups`, fit.clipped === 0, JSON.stringify(fit));
				await t.shot(`resets-${pose.name}`, `${pose.name}: Details sheet at the Resets and balances group: State / Refills / Expires, Use reset under each usable expiry, nothing cut off`);
				await t.page.click('.details [data-probe="reset-use"]');
				await t.until('the dialog shows', () => !!document.querySelector('[data-probe="reset-dialog-use"]'), null, { timeout: 5000 });
				const dialog = await t.page.evaluate(() => [...document.querySelectorAll('.reset-dialog .a-btns .btn')].map((b) => { const r = b.getBoundingClientRect(); return r.left >= 0 && r.right <= innerWidth && r.bottom <= innerHeight && r.height >= 24; }));
				await t.check(`${pose.name}: Cancel and Use this reset are on screen`, dialog.length === 2 && dialog.every(Boolean), JSON.stringify(dialog));
				await t.shot(`resets-dialog-${pose.name}`, `${pose.name}: the confirm dialog with full-width Cancel and Use this reset`);
				await t.page.click('[data-probe="reset-dialog-cancel"]');
				await t.check(`${pose.name}: nothing was claimed`, claims(server).length === 0);
			});
		}
	},
};
