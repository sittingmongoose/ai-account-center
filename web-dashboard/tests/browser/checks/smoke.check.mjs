// Dashboard smoke on a temporary CCS_HOME: the signed-out panel, then a real loopback sign-in. Every page keeps its
// probed controls inside the viewport at the widths where overflow bugs have shown up, and the canvas paints.
// One full frame for Home; the rest are saved for `browser-check diff`.
// Note: a ProbeMark reports its rectangle even while its control is hidden, so a reported probe is not proof of
// visibility; judge visibility from pixels (t.crop at the rectangle, t.painted) or a frame.
// Probes each page must report (an empty or foreign set fails): the shared top bar plus the page's own headings/controls
const PAGES = {
	home: ['update-apps', 'home-legend'],
	analytics: ['update-apps', 'trend-head:title', 'quota-head:title', 'daily-head:title'],
	accounts: ['update-apps', 'add:claude', 'pw-new', 'logout']
};

async function eachPage(t, label, extra = []) {
	for (const [page, require] of Object.entries(PAGES)) {
		await t.step(`${label} ${page}`, async () => {
			await t.app.open({ page });
			await t.painted('#canvas', 'canvas painted');
			await t.app.noProbeOverflow('no control past the edge', { require: [...require, ...extra] });
			await t.snap(`${label}-${page}`);
		});
	}
}

export const tests = {
	async 'signed out'(t) {
		await t.newContext();
		await t.app.open({ signIn: false });
		for (const id of ['signin-user', 'signin-pass', 'signin-submit']) await t.app.probeVisible(id);
		await t.app.noProbeOverflow();
		await t.snap('signed-out-1920');
	},

	async desktop(t) {
		await t.app.open();
		await t.app.probeVisible('update-apps');
		await t.shot('home-1920', 'Home at 1920x1080, signed in, no accounts: shell, empty states, type');
		await eachPage(t, '1920');
	},

	async widths(t) {
		for (const w of [1360, 1430, 1600, 2200]) {
			await t.viewport(w, 1080);
			await eachPage(t, String(w));
		}
		await t.viewport(1920, 1080);
	},

	// Phone at DPR 1: Slint under DPR emulation has rendered at double scale before; judge HiDPI on a real device
	async phone(t) {
		await t.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, isMobile: true, hasTouch: true });
		await t.app.open();
		for (const id of ['tab-home', 'tab-analytics', 'tab-accounts']) await t.app.probeVisible(id);
		await eachPage(t, '390', ['tab-home', 'tab-analytics', 'tab-accounts']);
	}
};
