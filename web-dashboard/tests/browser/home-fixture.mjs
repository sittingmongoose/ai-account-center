// Home's test data for the browser kit (checks/home-page.check.mjs, home-images/): a dashboard response with every
// kind of row and card Home draws. Made-up names only; never a real account.
//   Claude: Pro, Max with a Fable window, one past 100% (overage), one with no Fable reading, one that failed to refresh.
//   Codex: the active account, one past the 95% switch point (it asks "Activate anyway?"), one with no 5-hour window,
//   credits and banked resets; auto-switch on at 95% used.
//   Antigravity: one login (no native inventory here, so it can't be activated: the section says why).
//   Cards: Cursor (a meter and spend), Kimi Code, Z.ai (resets pack), Qwen, Muse Code, OpenCode Go (spend only).

export function homeAccounts(now = Date.now()) {
	const at = (minutes) => new Date(now + minutes * 60_000).toISOString();
	const quota = (key, label, used, minutes, reset, extra = {}) => ({
		key, label, kind: 'rate_limit', usedPercent: used, remainingPercent: used == null ? null : 100 - used, resetAt: reset == null ? null : at(reset),
		windowMinutes: minutes, used: null, limit: null, unit: null, ...extra,
	});
	const account = (id, provider, email, plan, windows, extra = {}) => ({
		id, provider, providerLabel: provider, label: email, email, plan, platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok',
		message: null, fetchedAt: at(-2), sampledAt: at(-2), isActive: false, hidden: false, trayHidden: false, windows, capabilities: {},
		signInNeeded: [], ...extra,
	});
	const claude = (id, email, plan, windows, extra = {}) =>
		account(`claude:${id}`, 'claude', email, plan, windows, {
			platform: 'mac', host: 'mac', capabilities: { claudeProfileId: id, claudePlatforms: ['mac', 'windows'] }, ...extra,
		});
	const codex = (id, email, plan, windows, extra = {}) =>
		account(`codex:${id}`, 'codex', email, plan, windows, { capabilities: { codexProfile: id }, ...extra });
	return {
		schemaVersion: 1,
		updatedAt: at(-1),
		settings: { refreshIntervalSeconds: 60 },
		codexAutoSwitch: {
			enabled: true, thresholdPercent: 5, pollIntervalSeconds: 60, outcome: 'healthy', message: 'The active account is below the switch point.',
			activationInProgress: false, checkedAt: at(-1),
		},
		accounts: [
			claude('pro-a', 'a.very.long.example.name@example.invalid', 'pro', [
				quota('five_hour', 'Five-hour usage', 9, 300, 160), quota('seven_day', 'Weekly usage', 93, 10080, 3500),
			]),
			claude('max-b', 'second.claude.account@example.invalid', 'max', [
				quota('five_hour', 'Five-hour usage', 104, 300, 210), quota('seven_day', 'Weekly usage', 7, 10080, 9000),
				quota('seven_day_fable', 'Fable weekly', 40, 10080, 9000),
			]),
			claude('max-c', 'third-claude-with-a-long-name@example.invalid', 'max', [
				quota('five_hour', 'Five-hour usage', 94, 300, 60), quota('seven_day', 'Weekly usage', 0, 10080, 9900),
			]),
			claude('max-d', 'fourth.claude@example.invalid', 'max', [
				quota('five_hour', 'Five-hour usage', 31, 300, 200), quota('seven_day', 'Weekly usage', 58, 10080, 4000),
				quota('seven_day_fable', 'Fable weekly', 12, 10080, 4000),
			], { status: 'cached', message: 'Refresh failed; showing the last reading.' }),
			codex('primary', 'codex.primary.workspace@example.invalid', 'pro', [
				quota('five_hour', '5h', 55, 300, 130), quota('seven_day', 'week', 44, 10080, 2300),
				{ key: 'credits_balance', label: 'Credits', kind: 'balance', unit: 'credits', remaining: 62500, used: null, limit: null },
				{ key: 'banked_resets', label: 'Banked resets', kind: 'balance', unit: 'resets', remaining: 1, used: null, limit: null },
			], { isActive: true }),
			codex('second', 'codex.second@example.invalid', 'plus', [
				quota('five_hour', '5h', 12, 300, 250), quota('seven_day', 'week', 97, 10080, 7000),
			]),
			codex('third', 'codex.third@example.invalid', 'plus', [quota('seven_day', 'week', 31, 10080, 7000)]),
			account('antigravity:one', 'antigravity', 'antigravity.login@example.invalid', 'pro', [
				quota('gemini-weekly', 'Gemini Models · Weekly', 30, 10080, 5000), quota('claude-weekly', 'Claude Models · Weekly', 70, 10080, 5000),
			]),
			account('cursor:one', 'cursor', 'cursor.team.seat@example.invalid', 'pro', [
				quota('monthly', 'Monthly usage', 18, 43200, 20000),
				{ key: 'on_demand', label: 'On-demand spend', kind: 'spend', unit: 'USD', used: 12.4, limit: 20, resetAt: at(20000) },
			]),
			account('kimi-code:one', 'kimi-code', 'kimi.code.example@example.invalid', 'pro', [quota('weekly', 'Weekly usage', 63, 10080, 4000)]),
			account('zai:one', 'zai', 'zai.coding.plan.example@example.invalid', 'pro', [
				quota('five_hour', 'Five-hour usage', 27, 300, 120),
				{ key: 'reset_pack', label: 'Reset pack', kind: 'balance', unit: 'resets', remaining: 2, used: 1, limit: 3, expiresAt: at(30000) },
			]),
			account('qwen:one', 'qwen', 'qwen.token.plan@example.invalid', 'pro', [quota('monthly', 'Monthly usage', 41, 43200, 15000)]),
			account('muse:one', 'muse', 'muse.code@example.invalid', 'pro', [quota('weekly', 'Weekly usage', 12, 10080, 6000)]),
			account('opencode-go:one', 'opencode-go', 'opencode.go@example.invalid', 'go', [
				{ key: 'monthly_spend', label: 'Monthly spend', kind: 'spend', unit: 'USD', used: 3.1, limit: 10, resetAt: at(15000) },
			]),
		],
	};
}

/** Claude desktop profiles for Open on Mac and Windows (GET /api/claude/desktop-profiles). */
export function homeProfiles() {
	return {
		profiles: [
			{ id: 'pro-a', mac: { canOpen: true }, windows: { canOpen: true } },
			{ id: 'max-b', mac: { canOpen: true }, windows: { canOpen: false } },
			{ id: 'max-c', mac: { canOpen: true }, windows: { canOpen: true } },
			{ id: 'max-d', mac: { canOpen: false }, windows: { canOpen: false } },
		],
	};
}
