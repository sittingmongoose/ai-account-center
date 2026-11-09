// The redesigned pages inside the real Slint shell's iframe at the Duo's window sizes: no rail or band (the shell draws
// the chrome until step 4 replaces it) and no room kept for them. With `--set out=<folder>` it also saves the first
// screen as <page>-embed-<pose>-<theme>.png, what the app shows today. Chrome reports no safe areas, so the shell
// draws its normal top bar and tab bar here. Synthetic data: the design fixtures, answered in the page.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', name), 'utf8'));
const POSES = [
	{ name: 'duo-folded', w: 466, h: 678 },
	{ name: 'duo-portrait', w: 669, h: 951 },
	{ name: 'duo-landscape', w: 951, h: 669 },
];

async function stub(t) {
	const status = { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json'), generatedAt: new Date().toISOString() };
	const now = Date.now();
	status.cards.open = status.cards.open.map((c) => ({ ...c, askedAt: new Date(now - (c.askedAgo ?? 0) * 1000).toISOString(), ageSeconds: c.askedAgo ?? 0 }));
	const board = { ...fixture('scoreboard-view.json'), byModel: fixture('scoreboard-by-model.json') };
	await t.page.route((u) => new URL(u).pathname === '/api/status', (r) => r.fulfill({ json: status }));
	await t.page.route((u) => new URL(u).pathname === '/api/scoreboard', (r) => r.fulfill({ json: board }));
}

// Status in both themes; the Scoreboard in light, to keep the run short.
const PLANS = [
	...POSES.flatMap((pose) => ['light', 'dark'].map((theme) => ({ page: 'status', ready: '.q-card', pose, theme }))),
	...POSES.map((pose) => ({ page: 'scoreboard', ready: '.picks', pose, theme: 'light' })),
];

export const tests = Object.fromEntries(
	PLANS.map(({ page, ready, pose, theme }) => [
		`${page} in the shell at ${pose.name} (${theme})`,
		async (t) => {
			await t.newContext({ viewport: { width: pose.w, height: pose.h }, isMobile: true, hasTouch: true, deviceScaleFactor: 1, serviceWorkers: 'block' });
			await t.page.addInitScript((mode) => { try { localStorage.setItem('aac-theme', mode); } catch {} }, theme);
			await t.app.signIn();
			await stub(t);
			await t.goto(`/${page}?e2e`, { settleMs: 500 });
			await t.until(`${pose.name}: ${page} iframe shown`, (p) => {
				const f = [...document.querySelectorAll('iframe.aac-frame')].find((x) => (x.getAttribute('src') || '').startsWith(`/app/${p}`));
				return !!f && f.style.visibility === 'visible';
			}, page, { timeout: 30000, hard: true });
			const frame = t.page.frames().find((f) => f.url().includes(`/app/${page}`));
			await frame.waitForSelector(ready);
			await t.equal(`${pose.name}: no page chrome inside the iframe`, await frame.evaluate(() => document.documentElement.dataset.chrome), 'none');
			await t.check(`${pose.name}: nothing reserved beside the content`, await frame.evaluate(() => Math.abs(document.querySelector('.content').getBoundingClientRect().width - innerWidth) < 1 && parseFloat(getComputedStyle(document.querySelector('.content')).paddingBottom) === 0));
			await t.check(`${pose.name}: no sideways scroll in the iframe`, await frame.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
			await t.page.waitForTimeout(400);
			if (t.args.out) {
				fs.mkdirSync(t.args.out, { recursive: true });
				await t.page.screenshot({ path: path.join(t.args.out, `${page}-embed-${pose.name}-${theme}.png`) });
			}
		},
	])
);
