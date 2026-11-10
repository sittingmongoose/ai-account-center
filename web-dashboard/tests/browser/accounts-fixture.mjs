// A fake server for the account routes, answered inside the page (Playwright routes reach the dashboard's iframe
// too): made-up accounts, keys, codes and app results only. Used by checks/accounts-frame.check.mjs (flows) and
// accounts-images/ (whole-page images). Every request that reaches it is recorded, so a check can say exactly what
// the page sent.

const NOW = Date.now();
export const at = (minutes) => new Date(NOW + minutes * 60_000).toISOString();
const quota = (key, label, used, minutes, reset) => ({ key, label, usedPercent: used, remainingPercent: 100 - used, resetAt: at(reset), windowMinutes: minutes });
const account = (id, provider, email, plan, windows, extra = {}) => ({
	id, provider, providerLabel: provider, label: email, email, plan, platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok', message: null,
	fetchedAt: at(-2), sampledAt: at(-2), isActive: false, hidden: false, trayHidden: false, windows, capabilities: {}, signInNeeded: [], ...extra,
});
const caps = (more = {}) => ({ add: true, signInAgain: true, replaceKey: true, remove: true, recheck: true, multiAccount: true, ...more });
const provider = (id, accountCount, more = {}) => ({ id, accountCount, signIn: { available: true }, capabilities: caps(more) });

/** The dashboard response: every provider, long made-up names, one hidden account. `rich` adds the other providers' accounts. */
export function dashboard({ rich = false } = {}) {
	const accounts = [
		account('claude:one', 'claude', 'one.claude@example.invalid', 'max', [quota('five_hour', 'Five-hour usage', 30, 300, 120), quota('seven_day', 'Weekly usage', 20, 10080, 3000)], { capabilities: { claudeProfileId: 'one', claudePlatforms: ['mac', 'windows'] } }),
		account('codex:one', 'codex', 'codex.active@example.invalid', 'pro', [quota('five_hour', '5h', 55, 300, 130), quota('seven_day', 'week', 44, 10080, 2300)], { isActive: true, capabilities: { codexProfile: 'one' } }),
		account('codex:two', 'codex', 'codex.second@example.invalid', 'plus', [quota('five_hour', '5h', 12, 300, 250), quota('seven_day', 'week', 31, 10080, 7000)], { capabilities: { codexProfile: 'two' } }),
		account('kimi-code:k1', 'kimi-code', 'kimi.key@example.invalid', 'pro', [quota('weekly', 'Weekly usage', 63, 10080, 4000)]),
	];
	if (rich) {
		accounts.splice(1, 0,
			account('claude:two', 'claude', 'a.very.long.example.name@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 9, 300, 160), quota('seven_day', 'Weekly usage', 93, 10080, 3500)], { capabilities: { claudeProfileId: 'two', claudePlatforms: ['mac'] } }),
			account('claude:three', 'claude', 'third-claude-with-a-long-name@example.invalid', 'max', [quota('five_hour', 'Five-hour usage', 94, 300, 60)], { capabilities: { claudeProfileId: 'three', claudePlatforms: ['mac', 'windows'] }, sampledAt: at(-45), fetchedAt: at(-45) }));
		accounts.push(
			account('antigravity:a1', 'antigravity', 'agy.workspace@example.invalid', 'pro', [quota('gemini-pro', 'Gemini Pro', 22, 300, 200)], { capabilities: { antigravityProfileId: 'a1' }, isActive: true }),
			account('cursor:c1', 'cursor', 'cursor.team.seat@example.invalid', 'pro', [quota('monthly', 'Monthly usage', 18, 43200, 20000)]),
			account('cursor:c2', 'cursor', 'cursor.second.seat@example.invalid', 'pro', [quota('monthly', 'Monthly usage', 71, 43200, 26000)]),
			account('muse:m1', 'muse', 'muse.code@example.invalid', 'pro', [quota('weekly', 'Weekly usage', 12, 10080, 9000)]),
			account('qwen:q1', 'qwen', 'qwen.token.plan@example.invalid', 'pro', [quota('monthly', 'Monthly usage', 41, 43200, 15000)]),
			account('zai:z1', 'zai', 'zai.coding.plan@example.invalid', 'pro', [quota('five_hour', 'Five-hour usage', 27, 300, 120)]),
			account('opencode-go:o1', 'opencode-go', 'opencode.go@example.invalid', 'go', [quota('monthly', 'Monthly usage', 5, 43200, 30000)]),
		);
	}
	const count = (id) => accounts.filter((a) => a.provider === id).length;
	return {
		schemaVersion: 1,
		updatedAt: at(-1),
		accounts,
		providers: ['claude', 'codex', 'antigravity', 'cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go'].map((id) => provider(id, count(id))),
		settings: { refreshIntervalSeconds: 60, hiddenProviders: [], hiddenAccountIds: rich ? ['cursor:c2'] : [], trayHiddenProviders: [], trayHiddenAccountIds: [] },
		codexAutoSwitch: { enabled: true, thresholdPercent: 5, outcome: 'below_threshold', message: 'The active account is below the switch point.' },
	};
}

const APPS = [['claude-code', 'Claude Code'], ['claude-desktop', 'Claude Desktop'], ['codex-cli', 'Codex CLI'], ['codex-desktop', 'Codex Desktop'], ['antigravity-cli', 'Antigravity CLI'], ['muse-code', 'Muse Code'], ['omp', 'OMP'], ['zcode', 'ZCode'], ['t3-code', 'T3 Code'], ['t3-acp-adapters', 'T3 ACP adapters']];
/** A finished Update apps run on four computers: mostly current, a few updated, one failed, one needing action. */
export function updateJob() {
	const results = [];
	for (const host of ['mac', 'windows', 'ubuntu', 'nas1']) {
		for (const [appId, appLabel] of APPS) {
			if (host === 'nas1' && appId.includes('desktop')) continue;
			let status = 'current', version = '1.4.2', previousVersion = '', message = '';
			if (appId === 'claude-code' && host !== 'windows') { status = 'updated'; previousVersion = '2.1.0'; version = '2.1.3'; }
			if (appId === 'codex-desktop' && host === 'mac') { status = 'action_required'; message = 'Quit Codex Desktop to finish the update.'; }
			if (appId === 'zcode' && host === 'windows') { status = 'not_installed'; version = ''; }
			if (appId === 'antigravity-cli' && host === 'nas1') { status = 'failed'; message = 'The download stopped: connection reset.'; }
			results.push({ platform: host, appId, appLabel, status, version, previousVersion, message });
		}
	}
	return { id: '0123abcd-0000-4000-8000-00000000abcd', state: 'succeeded', startedAt: at(-190), finishedAt: at(-182), expectedResults: results.length, results };
}

const regAccount = (id, provider, more = {}) => ({ id, provider, actions: { remove: true, signInAgain: true, replaceKey: true, recheck: true }, removeRefusal: null, ...more });

/** The fake server's state and every request the page made to it. */
export function fakeServer(over = {}) {
	const s = {
		requests: [],
		jobs: new Map(),
		trash: [{ trashId: 'tr_0123456789abcdef', provider: 'claude', label: 'old-profile', state: 'trashed', trashedAt: at(-60 * 24), purgeAfter: at(60 * 24 * 29) }],
		removed: new Set(),
		runningJobs: [],
		rich: false,
		update: null,
		...over,
	};
	const registry = () => ({
		providers: [],
		accounts: dashboard({ rich: s.rich }).accounts.map((a) => regAccount(a.id, a.provider,
			a.id === 'codex:one' ? { removeRefusal: 'account_active' }
				: a.provider === 'kimi-code' ? { credential: { kind: 'aac-key', last4: '9f3a' } } : {}))
			.filter((a) => !s.removed.has(a.id)),
		jobs: s.runningJobs,
		trash: s.trash,
	});
	const json = (status, payload) => ({ status, contentType: 'application/json', body: JSON.stringify(payload) });
	const routes = [
		['GET', /^\/api\/accounts\/dashboard$/, () => { const d = dashboard({ rich: s.rich }); d.accounts = d.accounts.filter((a) => !s.removed.has(a.id)); return json(200, d); }],
		['GET', /^\/api\/accounts\/registry$/, () => json(200, registry())],
		['GET', /^\/api\/app-updates\/status$/, () => json(200, { job: s.update })],
		['GET', /^\/api\/auth\/devices$/, () => json(200, { devices: s.rich ? [
			{ id: 'dev-mac-1', name: 'MacBook tray', platform: 'mac', appVersion: '3.2.0', pairedAt: at(-60 * 24 * 9), lastSeenAt: at(-3), lastSeenAddress: '192.168.10.31', idleExpiresAt: at(60 * 24 * 80) },
			{ id: 'dev-win-1', name: 'Windows PC tray', platform: 'windows', appVersion: '3.2.0', pairedAt: at(-60 * 24 * 20), lastSeenAt: at(-60 * 5), lastSeenAddress: '192.168.10.40' },
		] : [] })],
		['POST', /^\/api\/accounts\/add$/, (body) => {
			if (body.provider === 'codex') {
				const job = { id: 'job_0123456789abcdef', provider: 'codex', kind: 'device-code', mode: 'add', state: 'starting', profileName: body.profileName, verification: null, result: null, error: null };
				const waiting = { ...job, state: 'waiting', verification: { url: 'https://auth.openai.com/codex/device', userCode: 'ABCD-12345', expiresAt: at(14) } };
				s.jobs.set(job.id, [waiting, waiting, { ...job, state: 'succeeded', verification: null, result: { accountId: `codex:${body.profileName}`, email: 'new.codex@example.invalid', plan: 'plus' } }]);
				return json(202, { job });
			}
			if (body.provider === 'kimi-code') return json(200, { check: 'ok', account: { id: 'kimi-code:k2', credential: { last4: '7c2e' } } });
			return json(400, { error: 'not in this fixture' });
		}],
		['GET', /^\/api\/accounts\/signin-jobs\/([^/]+)$/, (_b, m) => {
			const list = s.jobs.get(m[1]) || [];
			const next = list.length > 1 ? list.shift() : list[0];
			return next ? json(200, next) : json(404, { code: 'not_found' });
		}],
		['POST', /^\/api\/accounts\/signin-jobs\/([^/]+)\/code$/, (_b, m) => {
			const job = { ...((s.jobs.get(m[1]) || [])[0] || {}), state: 'verifying' };
			s.jobs.set(m[1], [job]);
			return json(200, job);
		}],
		['POST', /^\/api\/accounts\/signin-jobs\/([^/]+)\/cancel$/, (_b, m) => json(200, { ...((s.jobs.get(m[1]) || [])[0] || {}), state: 'cancelled' })],
		['POST', /^\/api\/accounts\/([^/]+)\/remove$/, (body, m) => {
			const id = decodeURIComponent(m[1]);
			if (!body.confirmationToken) return json(200, { confirmation: { token: `tok-remove-${id}`, effects: ['Its saved Codex login is deleted.'] } });
			if (body.confirmationToken !== `tok-remove-${id}`) return json(409, { code: 'confirmation_invalid', error: 'That confirmation is no longer valid.' });
			s.removed.add(id);
			return json(200, { removed: true });
		}],
		['POST', /^\/api\/accounts\/trash\/([^/]+)\/restore$/, (body, m) => {
			if (!body.confirmationToken) return json(200, { confirmation: { token: 'tok-restore', effects: ['Its Claude data moves back on Mac and Windows.'] } });
			s.trash = s.trash.filter((t) => t.trashId !== m[1]);
			return json(200, { restored: true, accountId: 'claude:old-profile' });
		}],
		['POST', /^\/api\/codex\/profiles\/([^/]+)\/activate$/, () => json(409, {
			error: 'Confirm the switch', code: 'confirmation_required',
			confirmation: { token: 'A'.repeat(43), targetProfile: 'two', expiresAt: at(5), processes: [{ label: 'codex', pid: 4242, role: 'cli' }], warning: 'Active Codex work will stop.' },
		})],
	];
	return { s, routes };
}

/** Answers the fake server's routes in the page (and its iframes); anything else goes to the real test server. */
export async function serve(t, server) {
	await t.page.route((u) => new URL(u).pathname.startsWith('/api/'), async (route) => {
		const req = route.request();
		const url = new URL(req.url());
		let body = {};
		try { body = req.postData() ? JSON.parse(req.postData()) : {}; } catch {}
		for (const [method, re, answer] of server.routes) {
			const m = url.pathname.match(re);
			if (req.method() === method && m) {
				server.s.requests.push({ method, path: url.pathname, body });
				await route.fulfill(answer(body, m));
				return;
			}
		}
		await route.continue();
	});
}

/** Requests the page sent that change something (everything but GET). */
export const changes = (server) => server.s.requests.filter((r) => r.method !== 'GET');
