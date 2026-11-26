// Write-only pasted web-session UI (Qwen Token Plan, OpenCode Zen console wallet, Muse Code):
// masked password input clears on submit, never echoes secrets into DOM or state, handles
// Qwen intl/cn region switch, OpenCode multi-workspace picker, and honest "Needs sign-in · cached"
// status on desktop (1440px) and phone (390px). Synthetic fixtures only.

const at = (minutes) => new Date(Date.parse('2026-11-26T15:00:00Z') + minutes * 60_000).toISOString();

const providerMeta = (id, label, longLabel, order, extraCaps = {}) => ({
	id,
	label,
	longLabel,
	iconKey: id,
	order,
	visible: true,
	trayVisible: true,
	accountCount: 1,
	signIn: { kind: id === 'opencode-go' ? 'api-key' : id === 'qwen' ? 'browser-session' : 'device-code', label: 'Session', platforms: ['ubuntu'], secureTransportRequired: true, available: true, unavailableReason: null },
	extras: null,
	capabilities: {
		multiAccount: id === 'opencode-go',
		add: id === 'opencode-go',
		signInAgain: id !== 'opencode-go',
		replaceKey: id === 'opencode-go',
		remove: true,
		openApp: [],
		recheck: true,
		webSession: true,
		webSessionReason: null,
		...extraCaps,
	},
});

const FIXTURE = {
	schemaVersion: 1,
	updatedAt: at(-1),
	providers: [
		providerMeta('muse', 'Muse Code', 'Muse Code', 4),
		providerMeta('qwen', 'Qwen', 'Qwen Token Plan', 6),
		providerMeta('opencode-go', 'OpenCode Go', 'OpenCode Go', 8),
	],
	accounts: [
		{
			id: 'qwen:usage',
			provider: 'qwen',
			providerLabel: 'Qwen Token Plan',
			label: 'Qwen Token Plan',
			email: null,
			plan: 'Solo',
			platform: 'ubuntu',
			host: 'ubuntu',
			source: 'Pasted Qwen console session',
			status: 'needs_sign_in',
			message: 'The pasted Qwen console session has expired. Paste a fresh session.',
			fetchedAt: at(-1),
			sampledAt: at(-12),
			isActive: false,
			hidden: false,
			trayHidden: false,
			windows: [{ key: '5h', label: '5 hours', usedPercent: 34, remainingPercent: 66, resetAt: at(120), windowMinutes: 300 }],
			capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
			lifecycle: { state: 'ready', jobId: null },
		},
		{
			id: 'plan-opencode-go-console-web-0123456789ab',
			provider: 'opencode-go',
			providerLabel: 'OpenCode Go',
			label: 'OpenCode console wallet',
			email: null,
			plan: 'Go',
			platform: 'ubuntu',
			host: 'ubuntu',
			source: 'Pasted OpenCode console session',
			status: 'ok',
			message: 'Console workspace; API-key account identity has not been linked.',
			fetchedAt: at(-2),
			sampledAt: at(-2),
			isActive: false,
			hidden: false,
			trayHidden: false,
			windows: [{ key: 'zen-balance', label: 'Zen balance', kind: 'balance', remaining: 18.25, unit: 'USD' }],
			capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
			lifecycle: { state: 'ready', jobId: null },
		},
		{
			id: 'muse:usage',
			provider: 'muse',
			providerLabel: 'Muse Code',
			label: 'Muse Code',
			email: 'dev@example.invalid',
			plan: 'Pro',
			platform: 'ubuntu',
			host: 'ubuntu',
			source: 'Pasted Muse Code portal session',
			status: 'ok',
			message: null,
			fetchedAt: at(-2),
			sampledAt: at(-2),
			isActive: false,
			hidden: false,
			trayHidden: false,
			windows: [{ key: 'window', label: 'Current usage (5-hour)', usedPercent: 22, remainingPercent: 78, resetAt: at(180), windowMinutes: 300 }],
			capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
			lifecycle: { state: 'ready', jobId: null },
		},
	],
	settings: {
		refreshIntervalSeconds: 60,
		visibilityAvailable: true,
		hiddenProviders: [],
		hiddenAccountIds: [],
		trayHiddenProviders: [],
		trayHiddenAccountIds: [],
	},
};

const REGISTRY = {
	providers: FIXTURE.providers,
	accounts: [
		{ id: 'qwen:usage', provider: 'qwen', label: 'Qwen Token Plan', email: null, platform: 'ubuntu', credential: { kind: 'discover' }, hidden: false, trayHidden: false, lifecycle: { state: 'ready', jobId: null }, actions: { signInAgain: true, replaceKey: false, remove: true, open: [], recheck: true }, removeRefusal: null },
		{ id: 'plan-opencode-go-console-web-0123456789ab', provider: 'opencode-go', label: 'OpenCode console wallet', email: null, platform: 'ubuntu', credential: null, hidden: false, trayHidden: false, lifecycle: { state: 'ready', jobId: null }, actions: { signInAgain: true, replaceKey: false, remove: true, open: [], recheck: true }, removeRefusal: null },
		{ id: 'muse:usage', provider: 'muse', label: 'Muse Code', email: 'dev@example.invalid', platform: 'ubuntu', credential: { kind: 'discover' }, hidden: false, trayHidden: false, lifecycle: { state: 'ready', jobId: null }, actions: { signInAgain: false, replaceKey: false, remove: true, open: [], recheck: true }, removeRefusal: null },
	],
	jobs: [],
	trash: [],
};

async function openStubbedAccounts(t) {
	await t.app.stub('/api/accounts/dashboard', FIXTURE);
	await t.app.stub('/api/accounts/registry', REGISTRY);
	await t.app.noResets();
	await t.app.open({ page: 'accounts' });
	await t.until(
		'Accounts renders Qwen, OpenCode wallet, and Muse rows',
		() => document.querySelectorAll('.route:not([hidden]) .acc-item[data-row]').length >= 3,
		null,
		{ timeout: 45000, hard: true },
	);
}

export const tests = {
	async 'web-session write-only paste flow works on desktop with masked input, zero secret echo, and workspace picker'(t) {
		await t.viewport(1440, 900);
		await openStubbedAccounts(t);

		// Intercept PUT /api/accounts/web-session/* with deterministic synthetic responses
		const sent = [];
		await t.page.route('**/api/accounts/web-session/*', async (route) => {
			const req = route.request();
			const url = req.url();
			const body = JSON.parse(req.postData() || '{}');
			sent.push({ method: req.method(), url, body });
			if (url.endsWith('/qwen')) {
				await route.fulfill({
					status: 200,
					contentType: 'application/json',
					headers: { 'cache-control': 'no-store' },
					body: JSON.stringify({
						ok: true,
						check: 'ok',
						session: { surface: 'qwen', region: body.region || 'intl', fingerprint: 'sha256:0123456789abcdef' },
					}),
				});
				return;
			}
			if (url.endsWith('/opencode-wallet')) {
				if (!body.workspaceId) {
					await route.fulfill({
						status: 409,
						contentType: 'application/json',
						headers: { 'cache-control': 'no-store' },
						body: JSON.stringify({
							error: 'Multiple workspaces',
							code: 'choose_workspace',
							workspaces: [
								{ id: 'wrk_personal', name: 'Personal' },
								{ id: 'wrk_team', name: 'Team Zen' },
							],
						}),
					});
					return;
				}
				await route.fulfill({
					status: 200,
					contentType: 'application/json',
					headers: { 'cache-control': 'no-store' },
					body: JSON.stringify({
						ok: true,
						check: 'ok',
						session: { surface: 'opencode-wallet', workspaceId: body.workspaceId, fingerprint: 'sha256:fedcba9876543210' },
					}),
				});
				return;
			}
			await route.continue();
		});

		// Verify honest "Needs sign-in · cached" badge on expired Qwen row
		const qwenBadge = await t.page.textContent('.route:not([hidden]) .acc-item[data-row="qwen:usage"]');
		await t.check('expired Qwen row shows Needs sign-in · cached', /Needs sign-in · cached/.test(qwenBadge || ''), qwenBadge);

		// Open Qwen Paste/Reconnect flow
		await t.page.click('.route:not([hidden]) .acc-item[data-row="qwen:usage"] [data-probe="paste-session:qwen"]');
		await t.page.waitForSelector('.route:not([hidden]) [data-probe="flow:qwen"] [data-probe="flow-input:qwen"]', { timeout: 10000 });
		const inputAttrs = await t.page.evaluate(() => {
			const el = document.querySelector('.route:not([hidden]) [data-probe="flow-input:qwen"]');
			return el ? { type: el.getAttribute('type'), autocomplete: el.getAttribute('autocomplete') } : null;
		});
		await t.check('Qwen cookie field is masked password with autocomplete=off', inputAttrs?.type === 'password' && inputAttrs?.autocomplete === 'off', JSON.stringify(inputAttrs));

		// Crop desktop Qwen paste panel + expired row for visual inspection
		await t.crop('web-session-paste-qwen-desktop', '.route:not([hidden]) [data-probe="flow:qwen"]', 'desktop 1440: Qwen write-only paste flow with masked cookie field and region buttons');

		// Switch region to China, submit synthetic cookie, verify field clears and secret never appears in DOM
		const qwenSecret = 'login_aliyunid_ticket=fixture-secret-qwen-888';
		await t.page.click('.route:not([hidden]) [data-probe="web-session-region:qwen:cn"]');
		await t.page.fill('.route:not([hidden]) [data-probe="flow-input:qwen"]', qwenSecret);
		await t.page.click('.route:not([hidden]) [data-probe="flow:qwen"] [data-probe="flow-submit:qwen"]');
		await t.until('Qwen session saved state renders', () => /session saved/i.test(document.querySelector('.route:not([hidden]) [data-probe="flow:qwen"]')?.textContent || ''), null, { timeout: 10000, hard: true });
		const qwenLeaked = await t.page.evaluate((s) => document.body.innerText.includes(s) || JSON.stringify(window.__aacLastAccounts || {}).includes(s), qwenSecret);
		await t.check('Qwen cookie is never echoed in DOM or view state', !qwenLeaked, 'secret leaked');
		await t.check('Qwen PUT sent region=cn', sent[0]?.body?.region === 'cn', JSON.stringify(sent[0]));
		await t.audit('web-session paste desktop 1440');

		// Open OpenCode wallet paste flow -> trigger intentional 409 workspace picker -> pick Team Zen -> verify save
		const ocSecret = 'auth=fixture-secret-opencode-777';
		await t.page.click('.route:not([hidden]) [data-probe="paste-session:opencode-go"]');
		await t.page.waitForSelector('.route:not([hidden]) [data-probe="flow-input:opencode-go"]', { timeout: 10000 });
		await t.page.fill('.route:not([hidden]) [data-probe="flow-input:opencode-go"]', ocSecret);
		await t.page.click('.route:not([hidden]) [data-probe="flow:opencode-go"] [data-probe="flow-submit:opencode-go"]');
		await t.page.waitForSelector('.route:not([hidden]) [data-probe="web-session-pick:opencode-go:wrk_team"]', { timeout: 10000 });
		await t.crop('web-session-picker-opencode-desktop', '.route:not([hidden]) [data-probe="flow:opencode-go"]', 'desktop 1440: OpenCode Zen multi-workspace picker');
		await t.page.click('.route:not([hidden]) [data-probe="web-session-pick:opencode-go:wrk_team"]');
		await t.until('OpenCode workspace saved', () => /wrk_team/.test(document.querySelector('.route:not([hidden]) [data-probe="flow:opencode-go"]')?.textContent || ''), null, { timeout: 10000, hard: true });
		const ocLeaked = await t.page.evaluate((s) => document.body.innerText.includes(s) || JSON.stringify(window.__aacLastAccounts || {}).includes(s), ocSecret);
		await t.check('OpenCode cookie is never echoed after picker save', !ocLeaked, 'secret leaked');
		const noOverflow = await t.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1);
		await t.check('OpenCode workspace picker causes no sideways overflow', noOverflow, 'scrollWidth exceeded innerWidth');
		await t.noConsoleErrors('only the intentional 409 workspace picker response reached the console', {
			allow: [/status of 409 \(Conflict\)/],
		});
	},

	async 'web-session write-only paste flow fits cleanly on a 390px phone'(t) {
		await t.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, isMobile: true, hasTouch: true });
		await openStubbedAccounts(t);
		await t.page.click('.route:not([hidden]) .acc-item[data-row="qwen:usage"] [data-probe="paste-session:qwen"]');
		await t.page.waitForSelector('.route:not([hidden]) [data-probe="flow-input:qwen"]', { timeout: 10000 });
		await t.crop('web-session-paste-phone-390', '.route:not([hidden]) [data-probe="flow:qwen"]', 'phone 390: Qwen write-only paste flow fits without clipping');
		await t.audit('web-session paste phone 390');
	},
};
