// Accounts & Settings clicks: clicking an account row (it opens Details) or a provider's header switch must never push
// the clicked control, or anything on the page, off the page; reported 2026-11-01 as "clicking providers can sometimes
// go off the page". Ported from the classic canvas (?e2e probes) to the app frame's DOM when Slint was retired.
// Synthetic accounts only, shaped like the dashboard payload.

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

// D tier (1280+), the F tiers in between (760-1279, where Details is a side panel or sheet), and the phone tier
// separately below: every layout branch the page re-flows through. 1710 is the width the watch-pool overflow was
// reported at.
const WIDTHS = [1920, 1710, 1600, 1430, 1360, 1280, 1160, 1024, 900, 780];

async function openAccounts(t) {
	await t.app.open({ page: 'accounts' });
	await t.until('the Accounts page is drawn from the dashboard', () => document.querySelectorAll('.route:not([hidden]) .acc-item[data-row]').length >= 3, null, { timeout: 45000, hard: true });
}

/** The page's rectangle of a control, after scrolling it into view. */
const rectOf = (t, selector) => t.page.evaluate((sel) => {
	const el = document.querySelector(sel);
	if (!el) return null;
	el.scrollIntoView({ block: 'center', inline: 'nearest' });
	const r = el.getBoundingClientRect();
	return { x: r.x, y: r.y, width: r.width, height: r.height };
}, selector);

/** Nothing on the page runs past the left or right edge, and the page does not scroll sideways. */
async function noSideways(t, name) {
	const r = await t.page.evaluate(() => {
		const w = innerWidth;
		const bad = [...document.querySelectorAll('.route:not([hidden]) button, .route:not([hidden]) .acc-item, .details, .ag-box')]
			.filter((el) => el.getClientRects().length > 0)
			.map((el) => ({ el, r: el.getBoundingClientRect() }))
			.filter(({ r }) => r.width > 0 && (r.x < -1 || r.right > w + 1))
			.map(({ el, r }) => `${el.className || el.tagName} ${Math.round(r.x)}..${Math.round(r.right)} of ${w}`);
		return { scroll: document.documentElement.scrollWidth <= w + 1, bad: bad.slice(0, 6) };
	});
	return t.check(name, r.scroll && r.bad.length === 0, JSON.stringify(r));
}

async function closeDetails(t) {
	await t.page.keyboard.press('Escape');
	await t.until('Details closed', () => !document.querySelector('.details'), null, { timeout: 5000 });
}

async function everyRowSurvivesItsClick(t, width) {
	const rows = await t.page.evaluate(() => [...document.querySelectorAll('.route:not([hidden]) .acc-item[data-row]')].map((el) => el.dataset.row));
	if (!rows.length) throw new Error('no account rows rendered');
	const height = t.page.viewportSize().height;
	for (const id of rows) {
		const sel = `.route:not([hidden]) .acc-item[data-row="${id}"]`;
		const before = await rectOf(t, sel);
		await t.page.click(`${sel} .acc-email`);
		await t.page.waitForSelector('.details', { timeout: 10000 });
		await t.page.waitForTimeout(250);
		const after = await t.page.evaluate((s) => { const r = document.querySelector(s).getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; }, sel);
		for (const [when, rect] of [['before', before], ['after', after]]) {
			if (!rect || rect.y + rect.height < 0 || rect.y > height) throw new Error(`${id} ${when} the click went off the page at ${width}: ${JSON.stringify(rect)}`);
		}
		await noSideways(t, `${width}: nothing past the edge with Details open for ${id}`);
		await closeDetails(t);
	}
}

/** The reported bug lived inside the Antigravity policy box: watch-pool choices pushed the cooldown past its edge. */
async function cooldownInsidePolicyBox(t, width) {
	const r = await t.page.evaluate(() => {
		const box = document.querySelector('.route:not([hidden]) .ag-box');
		if (!box) return null;
		box.scrollIntoView({ block: 'center' });
		const b = box.getBoundingClientRect();
		const params = [...box.querySelectorAll('.ag-param')].map((p) => p.getBoundingClientRect().right);
		return { right: b.right, params };
	});
	if (!r) throw new Error(`the Antigravity policy box is missing at ${width}`);
	await t.check(`${width}: the policy box's choices stay inside it`, r.params.every((right) => right <= r.right + 0.5), JSON.stringify(r));
}

export const tests = {
	async 'provider clicks keep the page on screen'(t) {
		await t.app.stub('/api/accounts/dashboard', FIXTURE);
		for (const width of WIDTHS) {
			await t.viewport(width, 1080);
			await openAccounts(t);
			await t.step(`${width}: every account row survives its own click`, () => everyRowSurvivesItsClick(t, width));
			await cooldownInsidePolicyBox(t, width);
			await t.audit(`accounts ${width}`);
			await t.snap(`accounts-clicks-${width}`);
		}
		// The next test (and file) starts at the default width, not the last one here
		await t.viewport(1920, 1080);
	},

	// The header switches re-render a provider's card in place; nothing may move off the page.
	async 'provider header switches keep the page on screen'(t) {
		await t.app.stub('/api/accounts/dashboard', FIXTURE);
		await t.viewport(1920, 1080);
		await openAccounts(t);
		const height = t.page.viewportSize().height;
		for (const probe of ['show:claude', 'tray:claude', 'show:codex']) {
			await t.step(probe, async () => {
				const sel = `.route:not([hidden]) [data-probe="${probe}"]`;
				const rect = await rectOf(t, sel);
				if (!rect) throw new Error(`no switch ${probe}`);
				await t.page.click(sel);
				await t.page.waitForTimeout(400);
				const after = await rectOf(t, sel);
				if (after && (after.y < 0 || after.y + after.height > height)) throw new Error(`${probe} was pushed off the page by its own click: ${JSON.stringify({ rect, after })}`);
				await noSideways(t, `nothing past the edge after ${probe}`);
				// back as it was: these switches are saved by the server
				await t.page.click(sel);
				await t.page.waitForTimeout(400);
			});
		}
		await t.snap('accounts-clicks-switches');
	},

	// The phone tier stacks the rows and ends in the tab bar: the narrowest layout, where "went off the page" shows first.
	async 'provider clicks keep the phone page on screen'(t) {
		await t.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, isMobile: true, hasTouch: true });
		await t.app.stub('/api/accounts/dashboard', FIXTURE);
		await openAccounts(t);
		await t.visible('.tabbar a[href="/app/accounts"]', 'the Accounts tab');
		await t.step('390: every account row survives its own click', () => everyRowSurvivesItsClick(t, 390));
		await t.audit('accounts 390');
		await t.snap('accounts-clicks-390');
	},
};
