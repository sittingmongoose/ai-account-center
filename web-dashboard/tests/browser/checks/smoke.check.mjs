// Dashboard smoke on a temporary CCS_HOME: the signed-out page, then a real loopback sign-in. Every page of the app
// frame keeps its controls inside the viewport at the widths where overflow bugs have shown up (t.audit: covered
// controls, overflow, clipped text, console errors, failed requests). One full frame for Home; the rest are saved for
// `browser-check diff`. (Until Leptos finish step 9 this drew the classic Slint canvas through ?e2e probes.)

/** Each page's own content and the controls it must offer (by accessible name or selector). */
const PAGES = {
	home: { ready: '.route:not([hidden]) .page.home', controls: ['.chrome .upd-btn'] },
	analytics: { ready: '.route:not([hidden]) .ax-page', controls: ['.chrome .upd-btn'] },
	accounts: { ready: '.route:not([hidden]) .ac-page .ac-slot', controls: ['.chrome .upd-btn'] },
};

async function eachPage(t, label, { phone = false } = {}) {
	for (const [page, want] of Object.entries(PAGES)) {
		await t.step(`${label} ${page}`, async () => {
			await t.app.open({ page });
			await t.page.waitForSelector(want.ready, { timeout: 30000 });
			if (!phone) for (const control of want.controls) await t.visible(control, `${label} ${page}: ${control}`);
			else await t.visible('.tabbar a[href="/app/home"]', `${label} ${page}: the tab bar`);
			await t.audit(`${label} ${page}`);
			await t.check(`${label} ${page}: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
			await t.snap(`${label}-${page}`);
		});
	}
}

export const tests = {
	async 'signed out'(t) {
		await t.newContext();
		await t.app.open({ signIn: false });
		for (const field of ['input[name="username"]', 'input[name="password"]', '.signin-card button[type="submit"]']) await t.visible(field, `signed out: ${field}`);
		await t.audit('signed out');
		await t.snap('signed-out-1920');
	},

	async desktop(t) {
		// Another file's widths must not carry over to this one
		await t.viewport(1920, 1080);
		await t.app.open();
		await t.visible('.chrome .upd-btn', 'Update apps in the header');
		await t.shot('home-1920', 'Home at 1920x1080, signed in, no accounts: frame, empty states, type');
		await eachPage(t, '1920');
	},

	async widths(t) {
		for (const w of [1360, 1430, 1600, 2200]) {
			await t.viewport(w, 1080);
			await eachPage(t, String(w));
		}
		await t.viewport(1920, 1080);
	},

	async phone(t) {
		await t.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, isMobile: true, hasTouch: true });
		await t.app.open();
		for (const tab of ['home', 'analytics', 'accounts']) await t.visible(`.tabbar a[href="/app/${tab}"]`, `tab ${tab}`);
		await eachPage(t, '390', { phone: true });
	},
};
