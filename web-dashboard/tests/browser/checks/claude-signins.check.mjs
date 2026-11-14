// Claude sign-in renewal on the dashboard (dashboard payload `claudeSignIns`, written by the
// renewal runner in scripts/claude-signin): the Claude header says when a person is needed, a
// notice is raised once, each Claude row names its renewal state, and Details lists the runner,
// the browser session and every computer's sign-in. In the app frame (ported from the classic
// canvas when Slint was retired). Synthetic accounts and emails only.
const NOW = Date.parse('2026-11-03T15:00:00Z');
const at = (hours) => new Date(NOW + hours * 3600_000).toISOString().replace(/\.\d{3}Z$/, 'Z');

const quota = (key, label, used, minutes) => ({
	key, label, usedPercent: used, remainingPercent: 100 - used, resetAt: at(2), windowMinutes: minutes,
});
const account = (id, email, plan) => ({
	id, provider: 'claude', providerLabel: 'Claude', label: email, email, plan,
	platform: 'mac', host: 'mac', source: 'fixture', status: 'ok', message: null,
	fetchedAt: at(-0.05), sampledAt: at(-0.05), isActive: false, hidden: false, trayHidden: false,
	windows: [quota('five_hour', 'Five-hour usage', 30, 300), quota('seven_day', 'Weekly usage', 20, 10080)],
	capabilities: {}, signInNeeded: [],
});
const signIn = (host, profile, email, endsInDays, extra = {}) => ({
	host, profile, email, state: 'ok', signInEndsAt: at(endsInDays * 24), logoutAt: at(endsInDays * 24 + 5),
	renewFrom: at((endsInDays - 4) * 24), autoRenew: true, lastRenewAt: at(-24), lastResult: 'renewed',
	nextVerifyAt: null, retryAfter: null, reverts: 0, readAt: at(-0.1), ...extra,
});

const FIXTURE = {
	schemaVersion: 1,
	updatedAt: at(-0.02),
	accounts: [account('claude-1', 'one@example.invalid', 'max'), account('claude-2', 'two@example.invalid', 'pro')],
	codexAutoSwitch: { enabled: false, thresholdUsedPercent: 95, outcome: 'disabled', message: '' },
	claudeSignIns: {
		runner: 'mac', runnerVersion: '1.0.0', generatedAt: at(-0.1), stale: false, renewWithinHours: 96,
		needsYou: [{
			email: 'two@example.invalid', reason: 'captcha',
			how: 'Solve the CAPTCHA in the Chrome window for two@example.invalid on the Mac; renewal carries on by itself after.',
			since: at(-0.2),
		}],
		hosts: [{ host: 'mac', readAt: at(-0.1), error: null }, { host: 'nas1', readAt: at(-0.1), error: null }],
		signIns: [
			signIn('mac', 'one', 'one@example.invalid', 27),
			signIn('nas1', 'one', 'one@example.invalid', 29),
			signIn('windows', 'default', 'one@example.invalid', 26, { lastResult: 'end_not_moved', lastRenewAt: null }),
			signIn('mac', 'two', 'two@example.invalid', 2, { lastResult: 'waiting_for_you' }),
		],
		browserSessions: [
			{ email: 'one@example.invalid', signedIn: true, sessionEndsAt: at(24 * 27), checkedAt: at(-1), visitExtends: null },
			{ email: 'two@example.invalid', signedIn: false, sessionEndsAt: null, checkedAt: at(-1), visitExtends: null },
		],
	},
};

const toasts = (t) => t.page.evaluate(() => (globalThis.__aacToasts || []).map((item) => item.title));

/** Nothing on the page runs past the right edge, and the page does not scroll sideways. */
const noSideways = async (t, name) => t.check(name, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));

export const tests = {
	async 'sign-in renewal: header, notice, rows and Details'(t) {
		await t.app.stub('/api/accounts/dashboard', FIXTURE);
		await t.app.noResets();
		await t.viewport(1920, 1080);
		await t.app.open({ page: 'home' });
		await t.page.waitForSelector('.route:not([hidden]) .page.home .hsec', { timeout: 30000 });
		await t.step('a notice names the account that needs a person, once', async () => {
			await t.until('the notice was raised', () => (globalThis.__aacToasts || []).length > 0);
			const titles = (await toasts(t)).filter((title) => title.startsWith('Claude sign-in needs you'));
			if (titles.length !== 1 || !titles[0].includes('two@example.invalid')) {
				throw new Error(`expected one sign-in notice for two@example.invalid, got ${JSON.stringify(titles)}`);
			}
		});
		await t.shot('home-signin-renewal',
			'Claude header ends "1 sign-in step needs you" (warning tone); row metas read "Auto-renews · next end ..." and "Sign-in renewal needs you"; the red notice names two@example.invalid');
		await t.audit('home with sign-in renewal');
		await noSideways(t, 'no sideways scroll with sign-in renewal lines');
		// The Accounts page rows open the same Details as Home's.
		await t.app.open({ page: 'accounts' });
		await t.until('both Claude rows are on the Accounts page', () => document.querySelectorAll('.route:not([hidden]) .acc-item[data-row^="claude-"]').length === 2, null, { timeout: 30000 });
		for (const id of ['claude-1', 'claude-2']) {
			await t.page.click(`.route:not([hidden]) .acc-item[data-row="${id}"] .acc-email`);
			await t.page.waitForSelector('.details', { timeout: 10000 });
			await t.page.waitForTimeout(500);
			await t.crop(`details-${id}`, '.details',
				`Details for ${id}: Needs you (two only), Sign-in renewal, Browser session, then one "Sign-in · <computer>" line per profile with its end and renewal state`);
			await noSideways(t, `no sideways scroll with Details open for ${id}`);
			await t.page.keyboard.press('Escape');
			await t.until('Details closed', () => !document.querySelector('.details'), null, { timeout: 5000 });
		}
	},
};
