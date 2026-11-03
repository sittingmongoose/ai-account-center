// Claude sign-in renewal on the dashboard (dashboard payload `claudeSignIns`, written by the
// renewal runner in scripts/claude-signin): the Claude header says when a person is needed, a
// notice is raised once, each Claude row names its renewal state, and Details lists the runner,
// the browser session and every computer's sign-in. Synthetic accounts and emails only.
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
			how: 'Solve the CAPTCHA in the Chrome window for two@example.invalid on the mac; renewal carries on by itself after.',
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

export const tests = {
	async 'sign-in renewal: header, notice, rows and Details'(t) {
		await t.app.stub('/api/accounts/dashboard', FIXTURE);
		await t.viewport(1920, 1080);
		await t.app.open({ page: 'home' });
		await t.painted('#canvas', 'home painted with sign-in renewal data');
		await t.step('a notice names the account that needs a person, once', async () => {
			await t.until('the notice was raised', () => (globalThis.__aacToasts || []).length > 0);
			const titles = (await toasts(t)).filter((title) => title.startsWith('Claude sign-in needs you'));
			if (titles.length !== 1 || !titles[0].includes('two@example.invalid')) {
				throw new Error(`expected one sign-in notice for two@example.invalid, got ${JSON.stringify(titles)}`);
			}
		});
		const probes = await t.app.probes();
		const rows = Object.keys(probes).filter((id) => /^row:claude-/.test(id));
		if (rows.length) {
			const first = probes[rows[0]];
			const last = probes[rows.at(-1)];
			await t.crop('claude-section', { x: 0, y: Math.max(0, first.y - 90), width: 1920, height: last.y + last.height - first.y + 110 },
				'Claude header ends "1 sign-in step needs you"; row metas read "Sign-ins renew by themselves" and "Sign-in renewal needs you"');
		}
		await t.audit('home with sign-in renewal');
		await t.app.noProbeOverflow('no control past the edge with sign-in renewal lines');
		for (const id of rows.slice(0, 2)) {
			await t.app.click(id);
			await t.page.waitForTimeout(600);
			const panel = (await t.app.probes())['details-panel'];
			if (panel) await t.crop(`details-${id}`, panel, `Details for ${id}: Needs you / Sign-in renewal / Browser session / one line per computer`);
			await t.app.noProbeOverflow(`no control past the edge with Details open for ${id}`);
			await t.page.keyboard.press('Escape');
			await t.page.waitForTimeout(400);
		}
	},
};
