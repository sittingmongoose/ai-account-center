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
	quota('five_hour', '5-hour usage', 11, 300, 120),
	{ key: 'reset_credits_available', label: 'Rate-limit resets available', kind: 'balance', unit: 'resets', remaining: 2 },
	grantWindow(1, 'available', 1, 29, ['five_hour', 'seven_day', 'seven_day_overage_included']),
	grantWindow(2, 'available', 1, 34, ['seven_day', 'seven_day_overage_included']),
	grantWindow(3, 'used', 0, 118, ['seven_day_overage_included']),
	{ key: 'extra_usage', label: 'Extra usage', kind: 'extra_usage', enabled: false },
], { plan: 'pro', capabilities: { claudeProfileId: 'one', claudePlatforms: ['windows'] } });
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
	grantKey: key, kind, label, refillsText: { full: 'Refills your 5-hour and weekly allowances.', unknown: 'AAC does not know what this refills yet.' }[kind] ?? 'Refills your weekly allowance.',
	providerWords: words, resetsLeft: left, expiresAtMs: days === null ? null : MS(days), claimable, claimBlockedReason: reason,
});
/** The inventory, before and after a confirmed claim of the first grant. */
const inventory = (spent = false, pending = null) => ({
	status: 'ok', observedAtMs: Date.now(), sourceAgeMs: 900, pendingAttempt: pending,
	grants: [
		grant(KEY('a'), 'full', 'Full reset', spent ? 0 : 1, 29, !spent),
		grant(KEY('b'), 'weekly', 'Weekly reset', 1, 34, true),
		grant(KEY('c'), 'fable', 'Fable reset', 1, 118, true),
		grant(KEY('d'), 'unknown', 'Other reset', 1, 40, false, 'unknown-grant-kind', 'seven_day_opus'),
	],
	history: spent ? [
		{ sequence: '1', atMs: Date.now() - 5000, event: 'attempt', targetHash: KEY('e'), outcome: 'claimed-unconfirmed', details: { kind: 'full', idempotencyHash: KEY('f') } },
		{ sequence: '2', atMs: Date.now() - 4000, event: 'verification', targetHash: KEY('e'), outcome: 'claimed', details: { kind: 'full', idempotencyHash: KEY('f') } },
	] : [],
});

/** The reset routes, answered in the page. `claim` picks the claim's answer; every request is recorded. */
function resetServer({ claim = 'claimed', emptyWeekly = false, confirmMs = 120_000 } = {}) {
	const server = fakeServer();
	const r = { spent: false, pending: null, claim };
	const dashboard = emptyWeekly ? {
		...DASHBOARD,
		accounts: DASHBOARD.accounts.map((account) => account === CLAUDE ? {
			...account,
			windows: account.windows.map((window) => window.key === 'seven_day'
				? { ...window, usedPercent: 0, remainingPercent: 100 } : window),
		} : account),
	} : DASHBOARD;
	const canonicalAccountKey = KEY('7');
	const confirmationKey = KEY('2');
	let expiresAtMs = null;
	let revision = 1;
	let latestAttempt = null;

	const evidence = () => ({
		canonicalAccountKey, revision: String(revision), latestAttempt,
		pendingAttempts: r.pending ? [{ attemptKey: r.pending, attemptOrder: '1' }] : [],
		claimTokens: { untilMs: expiresAtMs, tokenKeys: expiresAtMs ? [confirmationKey] : [] },
	});
	const receipt = (pending, weekly = 0) => {
		r.pending = pending ? KEY('9') : null;
		r.spent = !pending;
		latestAttempt = {
			attemptKey: KEY('9'), attemptOrder: '1', pending, providerSuccess: true,
			outcome: pending ? 'claimed-unconfirmed' : 'claimed',
			verification: pending ? 'unconfirmed' : 'confirmed',
			windows: pending ? [] : [{ id: 'seven_day', usedPercent: weekly }, { id: 'five_hour', usedPercent: 0 }],
			...(pending ? {} : { unobservedWindows: ['seven_day_overage_included'] }),
		};
		revision++;
		return { ...latestAttempt, evidence: evidence() };
	};
	const json = (status, payload) => ({ status, contentType: 'application/json', body: JSON.stringify(payload) });
	server.routes.unshift(
		['GET', /^\/api\/accounts\/dashboard$/, () => json(200, dashboard)],
		['GET', /^\/api\/accounts\/registry$/, () => json(200, {
			providers: [], jobs: [], trash: [],
			accounts: DASHBOARD.accounts.map((a) => ({ id: a.id, provider: a.provider, actions: { remove: true, signInAgain: true, replaceKey: true, recheck: true }, removeRefusal: null })),
		})],
		['GET', /^\/api\/accounts\/preferences$/, () => json(200, {})],
		['GET', /^\/api\/(?:(?:antigravity|codex)\/profiles|claude\/desktop-profiles)$/, () => json(200, { profiles: [] })],
		['GET', /^\/api\/accounts\/claude%3Aone\/reset-claims$/, () => {
			const current = inventory(r.spent, r.pending);
			if (emptyWeekly) {
				current.grants = current.grants.map((g) => g.claimable
					? { ...g, claimable: false, claimBlockedReason: 'no-weekly-usage' } : g);
			}
			return json(200, { ...current, canonicalAccountKey, bindingRevision: '1', evidence: evidence() });
		}],
		['POST', /^\/api\/accounts\/claude%3Aone\/reset-claims\/prepare$/, (body) => {
			expiresAtMs = Date.now() + confirmMs;
			revision++;
			return json(200, {
				account: { label: 'Claude: fresh.label@example.invalid (Pro)' },
				confirmationToken: KEY('1'), confirmationKey, expiresAtMs, evidence: evidence(),
				grant: inventory().grants.find((g) => g.grantKey === body.grantKey),
				costText: 'Spends one selected reset. This cannot be undone.',
			});
		}],
		['POST', /^\/api\/accounts\/claude%3Aone\/reset-claims$/, (body) => {
			expiresAtMs = null;
			revision++;
			if (body.confirmationToken !== KEY('1') || r.claim === 'expired') {
				return json(409, {
					code: 'reset-state-unavailable', action: 'check-again', reason: 'confirmation-invalid',
					sent: false, evidence: evidence(),
				});
			}
			return json(200, receipt(r.claim === 'unconfirmed'));
		}],
		['POST', /^\/api\/accounts\/claude%3Aone\/reset-claims\/check$/, () => json(200, receipt(false, 3))],
	);
	const expireToken = () => {
		expiresAtMs = null;
		revision++;
	};
	return { server, r, expireToken };
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
const PARTIAL = 'Reset confirmed from the limits AAC could check. No comparable reading for Fable included usage.';

export const tests = {
	async 'resets: Pro full and Fable resets are offered without a Fable reading'(t) {
		const { server } = resetServer();
		await open(t, server);
		await openDetails(t, 'claude:one');
		await t.until('the inventory replaced the reading rows', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
		await t.text('.details [data-probe="resets"] .fg-sum b', '3 rate-limit resets ready.');
		const rows = await tableRows(t);
		await t.check('four rows: three usable and the blocked unknown grant', rows.length === 4
			&& rows[0].button && rows[1].button && rows[2].button && !rows[3].button, JSON.stringify(rows));
		await t.check('the full reset is available on Pro', rows[0].text.includes('5-hour and weekly limits'), rows[0].text);
		const fable = rows.find((row) => row.text.includes('Fable weekly usage'));
		await t.check('a missing Fable reading still allows the Fable reset', fable?.button === true, fable?.text);
		const unknown = rows.find((row) => row.text.includes('seven_day_opus'));
		await t.check('the unknown grant shows the provider\'s words and why it cannot be used',
			unknown?.text.includes("AAC doesn't know what this refills yet; it can't be used from here"), unknown?.text);
		await t.check('opening Details read the inventory only', resetRequests(server).every((q) => q.method === 'GET'), JSON.stringify(resetRequests(server)));
		await t.visible('.details [data-probe="details-reading"]', 'Reading is its own fact group');
		await t.shot('resets-desktop', 'Resets and balances: three usable rows and the explanation for the unknown grant; Extra usage as a pair; Reading as Read from / Profile');
	},

	async 'resets: a zero weekly reading explains why there is no Use reset button'(t) {
		const { server } = resetServer({ emptyWeekly: true });
		await open(t, server);
		await openDetails(t, 'claude:one');
		const why = 'Overall weekly usage is at 0%. AAC needs weekly usage to verify this reset.';
		await t.until('the reset row explains the zero reading',
			(words) => document.querySelector('.details .resets-table')?.innerText.includes(words), why, { timeout: 10000 });
		const rows = await tableRows(t);
		await t.check('there is no Use reset button at zero weekly usage', rows.length === 4 && rows.every((row) => !row.button), JSON.stringify(rows));
		await t.check('the explanation needs no reset POST', resetRequests(server).every((q) => q.method === 'GET'));
	},

	async 'resets: the dialog asks prepare only; Escape and Cancel close it and send nothing'(t) {
		for (const dismiss of ['Escape', 'Cancel']) {
			const { server } = resetServer();
			await open(t, server);
			await openDetails(t, 'claude:one');
			await t.until('usable', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
			await t.page.click('.details [data-probe="reset-use"]');
			await t.until('the dialog shows', () => !!document.querySelector('[data-probe="reset-dialog"]'), null, { timeout: 5000 });
			await t.text('[data-probe="reset-dialog-account"]', 'Claude: fresh.label@example.invalid (Pro)');
			await t.check('the dialog names the cost', await t.page.evaluate(() => document.querySelector('[data-probe="reset-dialog"]').innerText.includes("You have 3 resets; this spends one and can't be undone.")));
			await t.check('prepare named the first row\'s exact grant', resetRequests(server).some((q) => q.path.endsWith('/prepare') && q.body.grantKey === KEY('a')));
			if (dismiss === 'Escape') await t.shot('resets-dialog', 'the confirmation for the full reset shows the fresh account label, cost, countdown, Cancel and Use this reset');
			if (dismiss === 'Escape') await t.page.keyboard.press('Escape');
			else await t.page.click('[data-probe="reset-dialog-cancel"]');
			await t.until(dismiss + ' closes the dialog only', () => !document.querySelector('[data-probe="reset-dialog"]') && !!document.querySelector('.details'), null, { timeout: 3000 });
			await t.check('no claim was sent', claims(server).length === 0, JSON.stringify(changes(server)));
		}
	},

	async 'resets: Use this reset sends one claim and the panel shows the verification read'(t) {
		const { server } = resetServer();
		await open(t, server);
		await openDetails(t, 'claude:one');
		await t.until('usable', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
		await t.page.click('.details [data-probe="reset-use"]');
		await t.until('the dialog shows', () => !!document.querySelector('[data-probe="reset-dialog-use"]'), null, { timeout: 5000 });
		await t.page.click('[data-probe="reset-dialog-use"]');
		await t.until('the result shows in place', (words) => document.querySelector('.details [data-probe="reset-result"]')?.innerText.includes(words), PARTIAL, { timeout: 5000 });
		const sent = claims(server);
		await t.check('exactly one claim, naming the chosen grant and its confirmation', sent.length === 1 && sent[0].body.grantKey === KEY('a') && sent[0].body.confirmationToken === KEY('1'), JSON.stringify(sent));
		await t.until('the fresh read shows the grant used', () => document.querySelector('.details [data-probe="resets"] .fg-sum b')?.textContent === '2 rate-limit resets ready.', null, { timeout: 5000 });
		await t.visible('.details [data-probe="reset-history"]', 'the claim is in the Reset claims history');
		await t.shot('resets-applied', 'after the claim: AAC names the unobserved Fable limit beneath the table');
	},

	async 'resets: an expired confirmation says so; an unconfirmed claim blocks another until Check again'(t) {
		const { server, r } = resetServer({ claim: 'expired' });
		await open(t, server);
		await openDetails(t, 'claude:one');
		await t.until('usable', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
		await t.page.click('.details [data-probe="reset-use"]');
		await t.until('the dialog shows', () => !!document.querySelector('[data-probe="reset-dialog-use"]'), null, { timeout: 5000 });
		await t.page.click('[data-probe="reset-dialog-use"]');
		await t.until('the expiry is named', () => (document.querySelector('.details [data-probe="reset-result"]')?.innerText || '').includes('this confirmation expired or was already used'), null, { timeout: 5000 });
		await t.noConsoleErrors('only the injected reset conflict reached the console', {
			allow: [/^Failed to load resource: the server responded with a status of 409 \(Conflict\) http:\/\/127\.0\.0\.1:\d+\/api\/accounts\/claude%3Aone\/reset-claims$/],
		});
		r.claim = 'unconfirmed';
		await t.page.click('.details [data-probe="reset-use"]');
		await t.until('the dialog shows', () => !!document.querySelector('[data-probe="reset-dialog-use"]'), null, { timeout: 5000 });
		await t.page.click('[data-probe="reset-dialog-use"]');
		await t.until('the unconfirmed words', () => (document.querySelector('.details [data-probe="reset-result"]')?.innerText || '').includes('Reset was applied, but AAC could not confirm the new limits. Check again.'), null, { timeout: 5000 });
		await t.check('every Use reset waits on it', await t.page.evaluate(() => [...document.querySelectorAll('.details [data-probe="reset-use"]')].every((b) => b.disabled)));
		await t.shot('resets-unconfirmed', 'the unconfirmed claim: the sentence and Check again under the table, every Use reset disabled');
		const before = claims(server).length;
		await t.page.click('.details [data-probe="resets-check"]');
		await t.until('Check again settles it', (words) => document.querySelector('.details [data-probe="reset-result"]')?.innerText.includes(words), PARTIAL, { timeout: 5000 });
		await t.check('Check again sent only a check, never a claim', claims(server).length === before
			&& resetRequests(server).some((q) => q.path.endsWith('/check') && q.body.attemptKey === KEY('9')));
	},

	async 'resets: Open it again reads after the confirmation token expired'(t) {
		const { server, expireToken } = resetServer({ confirmMs: 1_000 });
		await open(t, server);
		await openDetails(t, 'claude:one');
		await t.until('usable', () => !!document.querySelector('.details [data-probe="reset-use"]'), null, { timeout: 10000 });
		await t.page.click('.details [data-probe="reset-use"]');
		await t.until('the expired confirmation offers Open it again', () => !!document.querySelector('[data-probe="reset-dialog-reopen"]'), null, { timeout: 5000 });
		expireToken();
		const before = resetRequests(server).length;
		await t.page.click('[data-probe="reset-dialog-reopen"]');
		await t.until('Open it again started a reset read', () => resetRequests(server).length > before, null, { timeout: 5000 });
		const after = resetRequests(server).slice(before);
		await t.check('the recovery read hit inventory or prepare', after.some((q) => q.path.includes('/reset-claims')), JSON.stringify(after));
		await t.until('the dialog reopened or named that nothing was sent', () => {
			const dialog = document.querySelector('[data-probe="reset-dialog"]');
			const said = document.querySelector('.details [data-probe="reset-result"]')?.innerText || '';
			return !!dialog || said.includes('could not confirm') && said.includes('Nothing was sent');
		}, null, { timeout: 5000 });
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
