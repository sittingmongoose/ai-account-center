// Accounts & Settings clicks: clicking a provider row (or the Settings jump) must never push the
// clicked row or any probed control off the page — reported 2026-11-01 as "clicking providers can
// sometimes go off the page". Synthetic accounts only, shaped like the dashboard payload.
// A probe reports its rectangle even while its control is hidden, so "on screen" here means the
// rectangle sits inside the viewport; visibility itself is judged by the canvas shots.
const at = (minutes) => new Date(Date.parse('2026-11-01T15:00:00Z') + minutes * 60_000).toISOString();

const quota = (key, label, used, minutes) => ({
	key, label, usedPercent: used, remainingPercent: 100 - used,
	resetAt: at(90), windowMinutes: minutes,
});

const account = (id, provider, email, plan, extra = {}) => ({
	id, provider, providerLabel: provider, label: email, email, plan,
	platform: 'ubuntu', host: 'ubuntu', source: 'fixture', status: 'ok', message: null,
	fetchedAt: at(-2), sampledAt: at(-2), isActive: false, hidden: false, trayHidden: false,
	windows: [], capabilities: {}, signInNeeded: [], ...extra,
});

	const FIXTURE = {
	schemaVersion: 1,
	updatedAt: at(-1),
	// The Antigravity policy box under the section: reported pools dedupe to one chip per label
	// and the cooldown segments must stay inside the card (2026-11-01 screenshot: five chips
	// pushed "60 min" past the card edge).
	antigravityAutoSwitch: {
		enabled: false, thresholdUsedPercent: 95, pollIntervalSeconds: 60, maxQuotaAgeSeconds: 300,
		cooldownSeconds: 300, selectedHostIds: ['ubuntu'], requestedPoolId: null,
		outcome: 'disabled', activationInProgress: false, message: '',
	},
	accounts: [
		account('claude-1', 'claude', 'one@example.invalid', 'max', {
			windows: [quota('five_hour', 'Five-hour usage', 41, 300), quota('seven_day', 'Weekly usage', 22, 10080), quota('seven_day_fable', 'Weekly Fable usage', 9, 10080)],
		}),
		account('claude-2', 'claude', 'two@example.invalid', 'pro', {
			windows: [quota('five_hour', 'Five-hour usage', 61, 300), quota('seven_day', 'Weekly usage', 33, 10080)],
		}),
		account('codex-1', 'codex', 'one@example.invalid', 'plus', {
			isActive: true,
			windows: [quota('five_hour', '5h', 55, 300), quota('seven_day', 'week', 44, 10080)],
		}),
		account('codex-2', 'codex', 'two@example.invalid', 'pro', {
			windows: [quota('five_hour', '5h', 12, 300), quota('seven_day', 'week', 31, 10080)],
		}),
		account('codex-3', 'codex', 'three@example.invalid', 'free', {
			windows: [quota('seven_day', 'week', 88, 10080)],
		}),
		account('antigravity-1', 'antigravity', 'one@example.invalid', 'pro', {
			windows: [
				quota('gemini-5h', 'Gemini Models · 5-hour', 3, 300), quota('gemini-weekly', 'Gemini Models · Weekly', 12, 10080),
				quota('3p-5h', 'Claude and GPT models · 5-hour', 66, 300), quota('3p-weekly', 'Claude and GPT models · Weekly', 71, 10080),
				quota('argon-weekly', 'Gemini 4 Argon · Weekly', 98.8, 10080),
			].map((w, i) => ({ ...w, poolId: ['gemini', 'gemini', 'claude-gpt', 'claude-gpt', 'argon'][i], poolLabel: ['Gemini Models', 'Gemini Models', 'Claude and GPT models', 'Claude and GPT models', 'Gemini 4 Argon'][i] })),
			antigravityPlan: { class: 'pro', quotaPolicy: 'five-hour-weekly', models: ['gemini'], thirdPartyModels: true, creditsOverage: true },
		}),
		account('antigravity-2', 'antigravity', 'two@example.invalid', 'free', {
			// The same pools under this account's own pool ids: the labels repeat, which is
			// exactly the watch-pool row that pushed the cooldown buttons off the card.
			windows: [quota('gemini-weekly', 'Gemini Models · Weekly', 44, 10080), quota('3p-weekly', 'Claude and GPT models · Weekly', 8, 10080)]
				.map((w, i) => ({ ...w, poolId: ['party-gemini', 'party-3p'][i], poolLabel: ['Gemini Models', 'Claude and GPT models'][i] })),
			antigravityPlan: { class: 'free', quotaPolicy: 'weekly', models: ['gemini'], thirdPartyModels: false },
		}),
		account('cursor-1', 'cursor', 'one@example.invalid', 'premium', {
			windows: [quota('plan-reported', 'Grok and Composer', 90.34, 43200)],
		}),
	],
	settings: {},
};

// D tier (1280+), the F tiers in between (760-1279, where Details is a side panel or sheet),
// and the phone tier separately below: every layout branch the page re-flows through.
// 1710 is the width of the window the watch-pool overflow was reported at.
const WIDTHS = [1920, 1710, 1600, 1430, 1360, 1280, 1160, 1024, 900, 780];

// Below D the Details side panel parks past the right edge while closed and its probe reports
// even hidden, so the page-level overflow check must skip it there (the open panel is asserted
// on its own right after a row click).
const overflowIgnore = (width) => (width < 1280 ? ['details-panel', 'details-sheet'] : []);
// The settings column's "Usage log sources" rows also answer to row: probes; they are
// provider toggles, not provider rows, and clicking one opens no Details.
const accountRows = (probes) => Object.keys(probes).filter((id) => id.startsWith('row:') && !id.includes(':usage'));

export const tests = {
	async 'provider clicks keep the page on screen'(t) {
		await t.app.stub('/api/accounts/dashboard', FIXTURE);
		for (const width of WIDTHS) {
			await t.viewport(width, 1080);
			await t.app.open({ page: 'accounts' });
			await t.painted('#canvas', `canvas painted at ${width}`);
			await t.step(`${width}: every account row survives its own click`, async () => {
				const rows = accountRows(await t.app.probes());
				if (!rows.length) throw new Error('no account rows rendered');
				for (const id of rows) {
					const before = await scrollIntoView(t, id);
					await t.app.click(id);
					await t.page.waitForTimeout(250);
					const after = await t.app.rect(id);
					const height = t.page.viewportSize().height;
					for (const [when, rect] of [['before', before], ['after', after]]) {
						if (rect.y < 0 || rect.y + rect.height > height) {
							throw new Error(`${id} ${name(when)} the click went off the page at ${width}: ${JSON.stringify(rect)}`);
						}
					}
					// The open panel (and every control on the page) stays inside the viewport.
					await t.app.noProbeOverflow(`no control past the edge after clicking ${id}`, { ignore: overflowIgnore(width) });
					await t.page.keyboard.press('Escape');
					await t.page.waitForTimeout(400);
				}
			});
			await assertCooldownInsidePolicyBox(t, width);
			await t.snap(`accounts-clicks-${width}`);
		}
	},

	// The header controls re-render a provider's section in place; the row layout below them
	// (and the page) must keep every probed control where it was, not push it off the page.
	async 'provider header toggles keep the page on screen'(t) {
		await t.app.stub('/api/accounts/dashboard', FIXTURE);
		await t.app.open({ page: 'accounts' });
		const width = t.page.viewportSize().width;
		for (const id of ['show:claude', 'tray:claude', 'show:codex', 'acct-show:codex-1', 'acct-tray:codex-1']) {
			await t.step(`${id} at ${width}`, async () => {
				const rect = await scrollIntoView(t, id);
				await t.app.click(id);
				await t.page.waitForTimeout(400);
				const after = await t.app.rect(id);
				const height = t.page.viewportSize().height;
				if (after && (after.y < 0 || after.y + after.height > height)) {
					throw new Error(`${id} was pushed off the page by its own click: ${JSON.stringify({ rect, after })}`);
				}
				await t.app.noProbeOverflow(`no control past the edge after ${id}`);
			});
		}
		await t.snap('accounts-clicks-toggles');
	},

	// The phone tier stacks the rows, adds the page head with the Settings jump and ends in the
	// tab bar: the narrowest layout, where "went off the page" shows first.
	async 'provider clicks keep the phone page on screen'(t) {
		await t.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, isMobile: true, hasTouch: true });
		await t.app.stub('/api/accounts/dashboard', FIXTURE);
		await t.app.open({ page: 'accounts' });
		await t.app.probeVisible('tab-accounts');
		await t.step('390: every account row survives its own click', async () => {
			const rows = accountRows(await t.app.probes());
			if (!rows.length) throw new Error('no account rows rendered');
			for (const id of rows) {
				const before = await scrollIntoView(t, id);
				await t.app.click(id);
				await t.page.waitForTimeout(250);
				const after = await t.app.rect(id);
				const height = t.page.viewportSize().height;
				for (const [when, rect] of [['before', before], ['after', after]]) {
					if (rect.y < 0 || rect.y + rect.height > height) {
						throw new Error(`${id} ${name(when)} the click went off the page at 390: ${JSON.stringify(rect)}`);
					}
				}
				await t.app.noProbeOverflow(`no control past the edge after clicking ${id}`);
				await t.page.keyboard.press('Escape');
				await t.page.waitForTimeout(150);
			}
		});
		await t.snap('accounts-clicks-390');
	},
};

// The reported bug lived entirely inside the Antigravity policy card: five watch-pool
// chips pushed the cooldown segments past the card's right edge while the page and
// viewport still had room. The card and both segmented rows carry probes, so the
// assertion is in code, not only in the snapshots.
async function assertCooldownInsidePolicyBox(t, width) {
	await scrollIntoView(t, 'pmark:antigravity');
	const all = await t.app.probes();
	const box = all['ag-policy'];
	if (!box) throw new Error(`policy-box probe missing at ${width}`);
	// the box's own 14 px inner padding is each segment row's right limit
	for (const id of ['watch-pool-segments', 'cooldown-segments']) {
		const row = all[id];
		if (!row) throw new Error(`probe "${id}" missing at ${width}`);
		if (row.x + row.width > box.x + box.width - 13) {
			throw new Error(`${id} ran past the policy box at ${width}: row ends at ${Math.round(row.x + row.width)}, box at ${Math.round(box.x + box.width)}`);
		}
	}
}

// The page scroller is Slint's own Flickable on one canvas, so a row is brought into view with
// wheel events, not scrollIntoView — and the pointer must sit over the page, not the header,
// or the wheel goes nowhere. Returns the row's rectangle once it sits inside the viewport.
async function scrollIntoView(t, id) {
	const size = t.page.viewportSize();
	const height = size.height;
	await t.page.mouse.move(size.width / 2, height / 2);
	let rect = await t.app.rect(id);
	for (let tries = 0; rect && (rect.y < 0 || rect.y + rect.height > height) && tries < 12; tries += 1) {
		const delta = rect.y + rect.height > height ? rect.y + rect.height - height + 60 : rect.y - 60;
		await t.page.mouse.wheel(0, delta);
		await t.page.waitForTimeout(200);
		rect = await t.app.rect(id);
	}
	if (!rect) throw new Error(`no probe "${id}" after scrolling`);
	return rect;
}

function name(when) {
	return when === 'before' ? 'sat' : 'was pushed';
}
