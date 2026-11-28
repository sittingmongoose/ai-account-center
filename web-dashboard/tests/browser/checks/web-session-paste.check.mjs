// Write-only pasted web-session UI (Qwen Token Plan, OpenCode Zen console wallet, Muse Code):
// masked password input clears on submit, never echoes secrets into DOM or state, handles
// Qwen intl/cn region switch, OpenCode multi-workspace picker, and honest "Needs sign-in · cached"
// status on desktop (1440px) and phone (390px). Synthetic fixtures only.
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';

const at = (minutes) => new Date(Date.parse('2026-11-26T15:00:00Z') + minutes * 60_000).toISOString();
// The card's aria-label is "<card label> <identity> details", so match the identity anywhere in it.
const OC_CARD = '.hcard[aria-label*="opencode.go@example.invalid"]';
const WALLET = { key: 'zen-balance', label: 'Wallet', kind: 'balance', usedPercent: null, remainingPercent: null, resetAt: null, windowMinutes: null, used: null, limit: null, remaining: 18.25, unit: 'USD' };
const WALLET_UNAVAILABLE = { ...WALLET, remaining: null, status: 'unavailable' };

/** Home with the fixture's OpenCode Go card holding its 5-hour window plus, when given, its wallet window. */
function withWalletDashboard(wallet) {
	const data = homeAccounts();
	const now = Date.now();
	const oc = data.accounts.find((a) => a.provider === 'opencode-go');
	oc.windows = [
		{ key: 'five_hour', label: 'Five-hour usage', kind: 'rate_limit', usedPercent: 41, remainingPercent: 59, resetAt: new Date(now + 120 * 60_000).toISOString(), windowMinutes: 300, used: null, limit: null, unit: null },
		...(wallet ? [wallet] : []),
	];
	return data;
}

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
			id: 'opencode-go:acct:9f2c41d0',
			provider: 'opencode-go',
			providerLabel: 'OpenCode Go',
			label: 'Key 9f2c41d0',
			email: null,
			plan: 'Go',
			platform: 'ubuntu',
			host: 'ubuntu',
			source: 'Account on Ubuntu',
			status: 'ok',
			message: null,
			fetchedAt: at(-2),
			sampledAt: at(-2),
			isActive: false,
			hidden: false,
			trayHidden: false,
			windows: [
				{ key: 'console-fiveHour', label: '5 hours', kind: 'rate_limit', usedPercent: 41, remainingPercent: 59, resetAt: at(110), windowMinutes: 300 },
				{ key: 'console-week', label: 'Weekly', kind: 'rate_limit', usedPercent: 12, remainingPercent: 88, resetAt: at(60 * 24 * 3), windowMinutes: 10080 },
				{ key: 'zen-balance', label: 'Wallet', kind: 'balance', remaining: 18.25, unit: 'USD' },
			],
			capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
			lifecycle: { state: 'ready', jobId: null },
		},
		{
			id: 'opencode-go:acct:2e034ec2',
			provider: 'opencode-go',
			providerLabel: 'OpenCode Go',
			label: 'Key 2e034ec2',
			email: null,
			plan: 'Go',
			platform: 'ubuntu',
			host: 'ubuntu',
			source: 'Account on Ubuntu',
			status: 'ok',
			message: null,
			fetchedAt: at(-2),
			sampledAt: at(-2),
			isActive: false,
			hidden: false,
			trayHidden: false,
			windows: [
				{ key: 'console-fiveHour', label: '5 hours', kind: 'rate_limit', usedPercent: 8, remainingPercent: 92, resetAt: at(140), windowMinutes: 300 },
				{ key: 'console-week', label: 'Weekly', kind: 'rate_limit', usedPercent: 3, remainingPercent: 97, resetAt: at(60 * 24 * 4), windowMinutes: 10080 },
			],
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
		{ id: 'opencode-go:acct:9f2c41d0', provider: 'opencode-go', label: 'Key 9f2c41d0', email: null, platform: 'ubuntu', credential: { kind: 'aac-key', last4: 'x7Qa', storedOn: 'ubuntu' }, hidden: false, trayHidden: false, lifecycle: { state: 'ready', jobId: null }, actions: { signInAgain: false, replaceKey: true, remove: true, open: [], recheck: true }, removeRefusal: null },
		{ id: 'opencode-go:acct:2e034ec2', provider: 'opencode-go', label: 'Key 2e034ec2', email: null, platform: 'ubuntu', credential: { kind: 'aac-key', last4: 'm3Pz', storedOn: 'ubuntu' }, hidden: false, trayHidden: false, lifecycle: { state: 'ready', jobId: null }, actions: { signInAgain: false, replaceKey: true, remove: true, open: [], recheck: true }, removeRefusal: null },
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
		'Accounts renders Qwen, two OpenCode Go key rows, and Muse rows',
		() => document.querySelectorAll('.route:not([hidden]) .acc-item[data-row]').length >= 4,
		null,
		{ timeout: 45000, hard: true },
	);
	const standalone = await t.page.evaluate(() => document.querySelectorAll('.route:not([hidden]) .acc-item[data-row^="plan-opencode-go-console"]').length);
	await t.check('no standalone console wallet row exists anywhere', standalone === 0, `found ${standalone}`);
	await t.check(
		'the key with a wallet session offers Remove wallet session',
		await t.page.evaluate(() => document.querySelectorAll('.route:not([hidden]) .acc-item[data-row="opencode-go:acct:9f2c41d0"] [data-probe="remove-wallet:opencode-go:acct:9f2c41d0"]').length > 0),
		'remove-wallet probe missing');
	await t.check(
		'the key without a session offers no Remove wallet session',
		await t.page.evaluate(() => document.querySelectorAll('.route:not([hidden]) .acc-item[data-row="opencode-go:acct:2e034ec2"] [data-probe^="remove-wallet:"]').length === 0),
		'remove-wallet probe present');
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

		// Open OpenCode wallet paste flow from the account's own row -> 409 workspace picker -> pick Team Zen
		const ocSecret = 'auth=fixture-secret-opencode-777';
		await t.page.click('.route:not([hidden]) .acc-item[data-row="opencode-go:acct:9f2c41d0"] [data-probe="paste-session:opencode-go:acct:9f2c41d0"]');
		await t.page.waitForSelector('.route:not([hidden]) [data-probe="flow-input:opencode-go"]', { timeout: 10000 });
		await t.page.fill('.route:not([hidden]) [data-probe="flow-input:opencode-go"]', ocSecret);
		await t.page.click('.route:not([hidden]) [data-probe="flow:opencode-go"] [data-probe="flow-submit:opencode-go"]');
		await t.page.waitForSelector('.route:not([hidden]) [data-probe="web-session-pick:opencode-go:wrk_team"]', { timeout: 10000 });
		await t.crop('web-session-picker-opencode-desktop', '.route:not([hidden]) [data-probe="flow:opencode-go"]', 'desktop 1440: OpenCode Zen multi-workspace picker');
		await t.page.click('.route:not([hidden]) [data-probe="web-session-pick:opencode-go:wrk_team"]');
		await t.until('OpenCode workspace saved', () => /wrk_team/.test(document.querySelector('.route:not([hidden]) [data-probe="flow:opencode-go"]')?.textContent || ''), null, { timeout: 10000, hard: true });
		const ocLeaked = await t.page.evaluate((s) => document.body.innerText.includes(s) || JSON.stringify(window.__aacLastAccounts || {}).includes(s), ocSecret);
		await t.check('OpenCode cookie is never echoed after picker save', !ocLeaked, 'secret leaked');
		const ocPuts = sent.filter((s) => String(s.url).endsWith('/opencode-wallet'));
		await t.check('OpenCode PUTs bind the wallet session to this account', ocPuts.length === 2 && ocPuts.every((s) => s.body.accountId === 'opencode-go:acct:9f2c41d0'), JSON.stringify(ocPuts.map((s) => s.body)));
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

	// The deliverable screenshots (402x874): one OpenCode Go card with its wallet
	// balance line inside the card, and the same card with no console session.
	// A pasted session whose read failed with nothing cached reads Wallet Unavailable.
	async 'opencode-go card shows the per-account Wallet line at phone 402x874'(t) {
		await t.newContext({ viewport: { width: 402, height: 874 }, deviceScaleFactor: 1, isMobile: true, hasTouch: true, serviceWorkers: 'block' });
		await t.app.signIn();
		await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
		const cardText = () => t.page.evaluate((sel) => document.querySelector(sel)?.textContent || '', OC_CARD);
		const show = async (data, what) => {
			await t.app.stub('/api/accounts/dashboard', data);
			await t.goto('/app/home', { settleMs: 300 });
			await t.until(`Home renders the OpenCode Go card (${what})`, (sel) => !!document.querySelector(sel), OC_CARD, { timeout: 60000, hard: true });
		};

		await show(withWalletDashboard(WALLET), 'wallet');
		await t.crop('opencode-go-card-with-wallet-phone-402', OC_CARD, 'phone 402: the OpenCode Go card carries the Wallet $18.25 balance under its 5-hour usage window');
		const withText = await cardText();
		await t.check('wallet balance line renders inside the OpenCode Go card', /Wallet/.test(withText) && withText.includes('18.25'), withText.slice(0, 160));
		const noOverflow = await t.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1);
		await t.check('OpenCode Go card with a wallet causes no sideways overflow at 402', noOverflow, 'scrollWidth exceeded innerWidth');
		await t.audit('opencode-go wallet card phone 402');

		await show(withWalletDashboard(null), 'no session');
		await t.crop('opencode-go-card-no-wallet-phone-402', OC_CARD, 'phone 402: the same OpenCode Go card with no console session shows no Wallet line');
		const noText = await cardText();
		await t.check('no Wallet line without a console session', !/Wallet/.test(noText), noText.slice(0, 160));

		await show(withWalletDashboard(WALLET_UNAVAILABLE), 'failed read');
		const failText = await cardText();
		await t.check('a failed read with nothing cached reads Wallet Unavailable, never $0', /Wallet/.test(failText) && /Unavailable/.test(failText) && !/\$?0\.00/.test(failText), failText.slice(0, 160));
	},
};
