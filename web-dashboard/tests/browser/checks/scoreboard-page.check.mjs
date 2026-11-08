// The Model Scoreboard page (Leptos, /app/scoreboard) against the real server, which reads the kit's fixture runs
// and catalog (scripts/scoreboard-fixture.mjs) in its temporary home. Synthetic data only.
const PHONE = { viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 };

async function openBoard(t) {
	await t.app.signIn();
	await t.goto('/app/scoreboard', { settleMs: 300 });
	await t.page.waitForSelector('.sb-task');
}

export const tests = {
	async 'scoreboard desktop'(t) {
		await t.viewport(1440, 900);
		await openBoard(t);
		await t.count('.sb-uses .chip-btn', 3);
		await t.check('ranked rows show their verdict and n', async () =>
			t.page.evaluate(() => [...document.querySelectorAll('.sb-row')].some((r) => /#1/.test(r.textContent) && /n = \d+/.test(r.textContent)))
		);
		await t.check('observed and benchmark columns are labelled apart', async () =>
			t.page.evaluate(() => {
				const heads = [...document.querySelectorAll('.sb-col h3')].map((h) => h.textContent);
				return heads.some((h) => h.startsWith('Observed in real work')) && heads.some((h) => h.startsWith('Benchmark:'));
			})
		);
		await t.text('.sb-sources', /unreadable|NUL/i);
		await t.audit('scoreboard 1440');
		await t.shot('scoreboard-desktop', 'Scoreboard at 1440 px: planning view, ranked rows, measures with n and intervals');
		// Building: a row with too little data sits in the folded group, unranked.
		await t.page.click('.sb-uses .chip-btn >> text=Building WorkNodes');
		await t.page.waitForSelector('.sb-task[data-task="worknode-build"]');
		await t.check('too-little-data rows are folded apart', async () =>
			t.page.evaluate(() => !!document.querySelector('.sb-task[data-task="worknode-build"] .sb-thin summary')?.textContent.includes('Too little data'))
		);
		await t.audit('scoreboard building 1440');
	},

	async 'scoreboard phone'(t) {
		await t.newContext(PHONE);
		await openBoard(t);
		await t.audit('scoreboard phone');
		await t.shot('scoreboard-phone', 'Scoreboard on a 402 px phone: uses wrap, rows readable, no sideways scroll');
	},
};
