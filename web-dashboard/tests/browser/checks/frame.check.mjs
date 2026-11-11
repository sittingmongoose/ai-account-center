// The app frame (PLAN step 4, web-dashboard/leptos/src/frame.rs): one chrome around all five tabs at /app/<page>.
// The frame draws all five tabs itself. The Slint dashboard stays loaded, content-only and hidden, in one same-origin
// iframe (/?content=1) for its bridge: it feeds Home, Analytics and Accounts their views (home-page.check.mjs,
// analytics-page.check.mjs, accounts-frame.check.mjs). These check the navigation, the chrome in each format (the Duo's strip and
// band rules, with `?safe=t,r,b,l` giving the measured insets), the setting and the way back, and the offline copy.
// Sign-in has its own folder (../frame-signin): its limiter step closes this server's sign-in for 15 minutes.
// Status and Scoreboard read the kit's fixture home (scripts/status-fixture.mjs, scoreboard-fixture.mjs).

const SLINT = new Set();
/** Tabs drawn here from the dashboard's views: the dashboard is on that page, signed in, but hidden. */
const BRIDGED = new Set(['home', 'analytics', 'accounts']);
const DASH_PATH = { home: '/', analytics: '/analytics', accounts: '/accounts' };
/** What each native tab shows once it has its data. */
const NATIVE = { status: '.status-grid, .q-card', home: '.home .hsec', scoreboard: '.page-title', accounts: '.ac-page .prov', analytics: '.ax-page' };
/** The dashboard's own header and tab bar probes: none may report inside the frame. */
const HEADER_PROBES = /^(nav|tab)-|^update-apps$|^status-(line|block)$/;
const NAV = { desk: '.chrome .nav a', phone: '.tabbar a', rail: '.rail > a', band: '.band-nav a' };
const FORMATS = [
	{ name: 'desktop', width: 1440, height: 900, safe: '0,0,0,0', mobile: false, chrome: 'desk' },
	{ name: 'iphone', width: 402, height: 874, safe: '62,0,34,0', mobile: true, chrome: 'phone' },
	{ name: 'duo-folded', width: 466, height: 678, safe: '0,84,34,0', mobile: true, chrome: 'rail' },
	{ name: 'duo-portrait', width: 669, height: 951, safe: '82,0,34,0', mobile: true, chrome: 'band' },
	{ name: 'duo-landscape', width: 951, height: 669, safe: '0,84,34,0', mobile: true, chrome: 'rail' },
];

async function slintFrame(t) {
	const el = await t.page.$('iframe.slint-frame');
	return el ? el.contentFrame() : null;
}

/** The tab is on screen: one route shown, and for the dashboard's tabs the iframe signed in on that page. */
async function routeShown(t, page) {
	await t.until(`${page} shown`, ({ slint, bridged, dash, native }) => {
		const shown = [...document.querySelectorAll('.route')].filter((r) => !r.hidden);
		if (shown.length !== 1) return false;
		if (bridged) {
			const f = document.querySelector('iframe.slint-frame');
			if (!f || shown[0].contains(f) || !shown[0].querySelector(native)) return false;
			try {
				const w = f.contentWindow;
				return !!(w.__aacLastAuth && w.__aacLastAuth.signedIn) && w.location.pathname === dash;
			} catch { return false; }
		}
		if (slint) {
			const f = document.querySelector('iframe.slint-frame');
			if (!f || !shown[0].contains(f)) return false;
			try {
				const w = f.contentWindow;
				return !!(w.__aacLastAuth && w.__aacLastAuth.signedIn) && w.location.pathname === dash;
			} catch { return false; }
		}
		return !!shown[0].querySelector(native);
	}, { page, slint: SLINT.has(page), bridged: BRIDGED.has(page), dash: DASH_PATH[page], native: NATIVE[page] }, { timeout: 60000, hard: true });
}

async function openFrame(t, page, safe = '') {
	await t.goto(`/app/${page}?e2e${safe ? `&safe=${safe}` : ''}`, { settleMs: 300 });
	await t.until(`${page}: the frame is drawn, signed in`, () => !!document.querySelector('.shell') && !document.querySelector('.signin-page'), null, { timeout: 30000, hard: true });
	await routeShown(t, page);
}

const where = (t) => t.page.evaluate(() => ({ path: location.pathname, current: [...document.querySelectorAll('a[aria-current="page"]')].map((a) => a.getAttribute('href')) }));

export const tests = {
	async 'frame: the tabs, the address and Back move together'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		await openFrame(t, 'status');
		await t.equal('desktop draws the header chrome', await t.page.evaluate(() => document.documentElement.dataset.chrome), 'desk');
		await t.equal('the header has the five tabs', await t.page.locator(NAV.desk).count(), 5);
		for (const page of ['home', 'analytics', 'accounts', 'scoreboard', 'status', 'accounts']) {
			await t.step(`to ${page}`, async () => {
				await t.page.click(`${NAV.desk}[href="/app/${page}"]`);
				await routeShown(t, page);
				const s = await where(t);
				await t.check(`${page}: the address and the marked tab follow`, s.path === `/app/${page}` && s.current.every((h) => h === `/app/${page}`) && s.current.length >= 1, JSON.stringify(s));
			});
		}
		await t.equal('one dashboard iframe for its three tabs', await t.page.locator('iframe.slint-frame').count(), 1);

		const dash = await slintFrame(t);
		await dash.evaluate(() => { window.__kitKept = 1; });
		await t.page.click(`${NAV.desk}[href="/app/status"]`);
		await routeShown(t, 'status');
		await t.page.click(`${NAV.desk}[href="/app/home"]`);
		await routeShown(t, 'home');
		await t.equal('the dashboard stays loaded across tab changes', await (await slintFrame(t)).evaluate(() => window.__kitKept || 0), 1);
		await t.check('on Home the dashboard is loaded but hidden: Home is drawn by the frame', await t.page.evaluate(() =>
			!!document.querySelector('iframe.slint-frame') && document.querySelector('.slint-host').hidden && document.querySelectorAll('.home .hsec').length > 0));

		await t.step('Back and Forward', async () => {
			await t.page.goBack();
			await routeShown(t, 'status');
			await t.page.goBack();
			await routeShown(t, 'accounts');
			await t.page.goForward();
			await routeShown(t, 'status');
			await t.page.goForward();
			await routeShown(t, 'home');
			await t.equal('Back and Forward end on Home', (await where(t)).path, '/app/home');
		});

		await t.step('the dashboard draws its page only', async () => {
			await t.page.click(`${NAV.desk}[href="/app/analytics"]`);
			await routeShown(t, 'analytics');
			const f = await slintFrame(t);
			const ids = await f.evaluate(async () => {
				window.__aacProbeNow && __aacProbeNow();
				await new Promise((r) => setTimeout(r, 900));
				return [...(window.__aacProbe || new Map()).keys()];
			});
			const header = ids.filter((id) => HEADER_PROBES.test(id));
			// (The sign-in panel's probes report even while it is hidden, as on the classic page: not a sign of it here.)
			await t.check('no dashboard header or tab bar inside the frame', ids.length > 0 && header.length === 0, `${ids.length} probes; header ones: ${header.join(', ') || 'none'}`);
			// every tab is drawn here now: the dashboard only feeds them, hidden
			await t.check('the dashboard is never on screen', await t.page.evaluate(() => document.querySelector('.slint-host')?.hidden === true));
		});

		await t.step("the dashboard's own links move the frame", async () => {
			const f = await slintFrame(t);
			await f.evaluate(() => window.ccsDashboardAction('navigate', 'accounts'));
			await routeShown(t, 'accounts');
			await t.equal('a dashboard link to Accounts: the address follows', (await where(t)).path, '/app/accounts');
			await f.evaluate(() => window.ccsDashboardAction('navigate', 'scoreboard'));
			await routeShown(t, 'scoreboard');
			await t.equal('a dashboard link to the Scoreboard: the frame shows it', (await where(t)).path, '/app/scoreboard');
		});

		await t.step('toasts come from the dashboard only', async () => {
			await t.page.evaluate(() => window.postMessage({ type: 'aac-content', kind: 'toast', toastKind: 'ok', title: 'Kit toast from the top', body: '', ms: 5000 }, location.origin));
			const f = await slintFrame(t);
			await f.evaluate(() => parent.postMessage({ type: 'aac-content', kind: 'toast', toastKind: 'ok', title: 'Kit toast from the dashboard', body: '', ms: 5000 }, location.origin));
			await t.until('the dashboard toast shows over the Scoreboard', () => /Kit toast from the dashboard/.test(document.querySelector('.toasts')?.textContent || ''), null, { timeout: 5000 });
			await t.check('a message from any other window is ignored', !(await t.page.evaluate(() => /Kit toast from the top/.test(document.querySelector('.toasts')?.textContent || ''))));
		});

		await t.step('an address opens its tab', async () => {
			await openFrame(t, 'accounts');
			const s = await where(t);
			await t.check('/app/accounts opens Accounts & Settings', s.path === '/app/accounts' && s.current.includes('/app/accounts'), JSON.stringify(s));
		});
	},

	async 'frame: each format draws its own chrome, and the Duo keeps its strip and band rules'(t) {
		for (const f of FORMATS) {
			for (const page of ['status', 'home', 'analytics']) {
				await t.step(`${f.name} ${page}`, async () => {
					await t.newContext({ viewport: { width: f.width, height: f.height }, isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
					await t.app.signIn();
					await openFrame(t, page, f.safe);
					const label = `${f.name} ${page}`;
					const s = await t.page.evaluate(({ nav, safe }) => {
						const [st, sr, sb] = safe.split(',').map(Number);
						const box = (el) => { const r = el.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, r: r.right, b: r.bottom }; };
						const visible = (el) => { const r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== 'hidden'; };
						const tabs = [...document.querySelectorAll(nav)].filter(visible).map(box);
						const main = document.querySelector('main.content');
						// The content area ends above the home indicator in the rail and band (its bottom padding).
						const cs = getComputedStyle(main);
						const content = { ...box(main), pad: parseFloat(cs.paddingBottom) || 0, padL: parseFloat(cs.paddingLeft) || 0, padT: parseFloat(cs.paddingTop) || 0 };
						const iframe = document.querySelector('.route:not([hidden]) iframe.slint-frame');
						const railTools = [...document.querySelectorAll('.rail a, .rail button')].filter(visible).map(box);
						const reserve = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--rail-reserve')) || 0;
						// Every point of the top band: what a tap there would land on.
						const tappable = 'a, button, input, select, textarea, label, summary, iframe, [role="button"], [tabindex]';
						const inBand = [];
						if (st >= 60) for (let y = 1; y < st; y += 6) for (let x = 2; x < innerWidth; x += 12) {
							const el = document.elementFromPoint(x, y);
							const hit = el && el.closest(tappable);
							if (hit) inBand.push(`${hit.tagName.toLowerCase()}.${hit.className} at ${x},${y}`);
						}
						// The visible tabs are on top where they are drawn (nothing, the iframe included, lies over them).
						const covered = [...document.querySelectorAll(nav)].filter(visible).filter((a) => {
							const r = a.getBoundingClientRect();
							const el = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
							return !el || !a.contains(el);
						}).map((a) => a.getAttribute('href'));
						// How tall each tab is to a tap: the rows above and below its drawn pill that still land on it.
						const reach = [...document.querySelectorAll(nav)].filter(visible).map((a) => {
							const r = a.getBoundingClientRect();
							const on = (y) => { const el = document.elementFromPoint(r.x + r.width / 2, y); return !!el && a.contains(el); };
							let top = r.y, bottom = r.bottom - 1;
							while (top > 0 && on(top - 1)) top--;
							while (bottom < innerHeight - 1 && on(bottom + 1)) bottom++;
							return bottom + 1 - top;
						});
						return { chrome: document.documentElement.dataset.chrome, w: innerWidth, h: innerHeight, st, sr, sb, tabs, content, iframe: iframe ? box(iframe) : null, railTools, reserve, inBand: inBand.slice(0, 6), bandCount: inBand.length, covered, reach, scrollW: document.documentElement.scrollWidth };
					}, { nav: NAV[f.chrome], safe: f.safe });
					await t.equal(`${label}: chrome`, s.chrome, f.chrome);
					const minTab = f.mobile ? 44 : 30;
					await t.check(`${label}: the five tabs inside the window, big enough to tap`, s.tabs.length === 5 && s.tabs.every((r, i) => r.x >= -0.5 && r.r <= s.w + 0.5 && r.y >= -0.5 && r.b <= s.h + 0.5 && s.reach[i] >= minTab - 0.5),
						s.tabs.map((r, i) => `${Math.round(r.x)},${Math.round(r.y)} ${Math.round(r.w)}x${Math.round(r.h)} (taps ${Math.round(s.reach[i])} px tall)`).join('; '));
					await t.check(`${label}: no tab is covered`, s.covered.length === 0, s.covered.join(', '));
					await t.check(`${label}: no sideways scroll`, s.scrollW <= s.w + 1, `${s.scrollW} > ${s.w}`);
					if (page === 'analytics') {
						// every tab is drawn here: the dashboard only feeds Analytics, hidden
						await t.check(`${label}: Analytics is drawn here, the dashboard stays hidden`, !s.iframe, s.iframe ? 'the dashboard iframe is on screen' : '');
					}
					if (f.chrome === 'rail') {
						const strip = s.w - s.sr;
						await t.check(`${label}: every rail control inside the ${s.sr} px strip, below the clock`, s.railTools.length >= 8 && s.railTools.every((r) => r.x >= strip - 0.5 && r.r <= s.w + 0.5 && r.h >= 44 - 0.5 && r.y >= s.reserve - 0.5),
							s.railTools.map((r) => `${Math.round(r.x)},${Math.round(r.y)} ${Math.round(r.w)}x${Math.round(r.h)}`).join('; '));
						const right = (s.iframe || s.content).r;
						await t.check(`${label}: the page stops at the strip`, right <= strip + 0.5, `content ends at ${Math.round(right)}, strip starts at ${strip}`);
					}
					if (f.chrome === 'band') {
						await t.check(`${label}: nothing tappable in the ${s.st} px band`, s.bandCount === 0, s.inBand.join('; '));
						await t.check(`${label}: the tab row starts below the band`, s.tabs.every((r) => r.y >= s.st - 0.5), s.tabs.map((r) => Math.round(r.y)).join(', '));
						const top = (s.iframe || s.content).y;
						await t.check(`${label}: the page starts below the tab row`, top >= Math.max(...s.tabs.map((r) => r.b)) - 0.5, `page top ${Math.round(top)}`);
					}
					if (f.chrome === 'phone') {
						await t.check(`${label}: the tab bar clears the home indicator`, s.tabs.every((r) => r.b <= s.h - s.sb + 0.5), s.tabs.map((r) => Math.round(r.b)).join(', '));
					}
				});
			}
		}
	},

	async 'frame: off by default, turned on from the classic app, and one tap back'(t) {
		await t.newContext({ viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' });
		await t.app.signIn();
		await t.equal('off by default', await t.page.evaluate(() => localStorage.getItem('aac-frame')), null);
		await t.goto('/', { settleMs: 1500 });
		await t.equal('off: the start page stays the classic app', await t.page.evaluate(() => location.pathname), '/');

		await t.step('Try it on the classic Status tab', async () => {
			await t.goto('/status?e2e', { settleMs: 500 });
			await t.until('the classic shell shows Status', () => {
				const f = [...document.querySelectorAll('iframe.aac-frame')].find((x) => (x.getAttribute('src') || '').startsWith('/app/status'));
				return !!f && f.style.visibility === 'visible';
			}, null, { timeout: 30000, hard: true });
			const status = t.page.frames().find((f) => f.url().includes('/app/status'));
			await status.waitForSelector('.frame-offer button');
			await status.click('.frame-offer button');
			await t.until('Try it opens the frame on Status', () => location.pathname === '/app/status', null, { timeout: 15000, hard: true });
			await routeShown(t, 'status');
			await t.equal('the setting is on for this device', await t.page.evaluate(() => localStorage.getItem('aac-frame')), 'new');
		});

		await t.step('on: every classic address opens the frame', async () => {
			for (const [from, page] of [['/', 'home'], ['/analytics', 'analytics'], ['/accounts', 'accounts'], ['/?view=scoreboard', 'scoreboard'], ['/status', 'status']]) {
				await t.goto(from, { settleMs: 200 });
				await t.until(`${from} opens /app/${page}`, (p) => location.pathname === `/app/${p}`, page, { timeout: 15000 });
			}
			await t.goto('/?classic=1', { settleMs: 1200 });
			await t.equal('?classic=1 still opens the classic app', await t.page.evaluate(() => location.pathname), '/');
			const manifest = await t.page.evaluate(async () => (await (await fetch('/manifest.webmanifest')).json()).start_url);
			await t.equal('the installed app starts at /, so it follows the setting', manifest, '/');
		});

		await t.step('Classic app in the account menu', async () => {
			await openFrame(t, 'home');
			await t.page.click('.chrome .select-btn[aria-label="Account"]');
			await t.page.locator('.acct-menu .menu-item', { hasText: 'Classic app' }).click();
			await t.until('Classic app opens /', () => location.pathname === '/' && !!document.querySelector('#canvas'), null, { timeout: 15000, hard: true });
			await t.page.waitForTimeout(1500);
			await t.equal('and stays there', await t.page.evaluate(() => location.pathname), '/');
			await t.equal('the setting is off for this device', await t.page.evaluate(() => localStorage.getItem('aac-frame')), 'classic');
		});
	},

	async 'frame: the installed app opens offline from the service worker'(t) {
		await t.newContext({ viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1 });
		await t.app.signIn();
		await t.page.evaluate(() => localStorage.setItem('aac-frame', 'new'));
		await openFrame(t, 'status');
		await t.until('the service worker controls the frame', () => !!navigator.serviceWorker?.controller, null, { timeout: 20000 });
		// Under the worker, as the installed app has used it: the classic page once, and the frame, which stores each
		// tab's page and the start page 4 s after it starts.
		await t.goto('/?classic=1', { settleMs: 2500 });
		await openFrame(t, 'status');
		await t.page.waitForTimeout(6000);
		await t.context.setOffline(true);
		try {
			await t.page.goto(new URL('/', t.run.baseURL).href).catch(() => {});
			await t.until('offline, the start page opens the frame', () => location.pathname === '/app/home' && !!document.querySelector('.shell, .signin-page'), null, { timeout: 30000 });
			const s = await t.page.evaluate(() => ({ text: document.body.innerText.slice(0, 400), frame: !!document.querySelector('.signin-page, .shell') }));
			await t.check('offline: the frame is drawn from the stored copy, not the worker\'s fallback', s.frame && !/has not been downloaded/.test(s.text), s.text.replace(/\s+/g, ' ').slice(0, 160));
		} finally {
			await t.context.setOffline(false);
		}
		// Offline, the sign-in check's two calls fail: that is the offline page's cue, so only those are allowed.
		await t.noConsoleErrors('offline: no console errors but the two offline calls', { allow: [/ERR_INTERNET_DISCONNECTED/] });
	},
};
