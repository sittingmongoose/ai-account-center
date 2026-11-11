// Each page in each format and theme, as a whole-page image. Sticky chrome is unpinned for the shot so the rail and
// tab bar sit once at their place in the page, like the mockups' "Whole page" view.
import fs from 'node:fs';
import path from 'node:path';

const PAGES = ['status', 'scoreboard'];
// The poses an installed app really gets (REAL-DEVICE-INFO.md, the 2026-11-11 review's U7): no top inset on the
// iPhone or the open Duo (iOS keeps its status bar or band outside the page), and the folded Duo turned either way
// with its strip on the right or the left. The Duo poses report the Duo's screen (466x678 in every pose).
const DUO_SCREEN = { width: 466, height: 678 };
const FORMATS = [
	{ name: 'desktop', width: 1440, height: 900, safe: '0,0,0,0', mobile: false },
	{ name: 'iphone', width: 402, height: 812, safe: '0,0,34,0', mobile: true },
	{ name: 'duo-folded', width: 466, height: 678, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-right', width: 678, height: 466, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-left', width: 678, height: 466, safe: '0,0,34,84', mobile: true, duo: true },
	{ name: 'duo-open-upright', width: 669, height: 869, safe: '0,0,34,0', mobile: true, duo: true },
	{ name: 'duo-open-landscape', width: 951, height: 669, safe: '0,84,34,0', mobile: true, duo: true },
];
const THEMES = ['light', 'dark'];
const UNPIN = `html { scrollbar-width: none; }
.rail, .tabbar, .topbar, .chrome, .band-head { position: static !important; }
.rail { height: auto !important; align-self: stretch !important; }`;

async function openQuestions(t) {
	await t.page.waitForSelector('.sec-questions .q-card');
	const list = t.page.locator('.q-list .q-item');
	if (await list.first().isVisible()) await list.nth(1).click();
	else await t.page.locator('button:visible', { hasText: /^Next$/ }).first().click();
	await t.page.waitForSelector('.q-card[data-kind="question"], .q-card');
	await t.page.waitForFunction(() => /start-up order/.test(document.querySelector('.q-title')?.textContent || ''));
	await t.page.locator('.opt .opt-pick').nth(1).click();
	await t.page.evaluate(() => document.activeElement?.blur?.());
}

export const tests = Object.fromEntries(
	PAGES.flatMap((page) =>
		FORMATS.flatMap((f) =>
			THEMES.map((theme) => [
				`${page} ${f.name} ${theme}`,
				async (t) => {
					await t.newContext({ viewport: { width: f.width, height: f.height }, ...(f.duo ? { screen: DUO_SCREEN } : {}), isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1 });
					await t.goto(`/app/${page}?safe=${f.safe}`, { settleMs: 300 });
					if (page === 'status') await openQuestions(t);
					else await t.page.waitForSelector('.picks');
					await t.page.evaluate((mode) => document.documentElement.setAttribute('data-theme', mode), theme);
					await t.page.addStyleTag({ content: UNPIN });
					await t.page.waitForTimeout(250);
					const out = t.args.out || t.outPath?.('frames') || path.join(process.cwd(), 'frames-out');
					fs.mkdirSync(out, { recursive: true });
					const file = path.join(out, `${page}-${f.name}-${theme}.png`);
					await t.page.screenshot({ path: file, fullPage: true });
					await t.check(`${page} ${f.name} ${theme}: saved`, fs.statSync(file).size > 5000);
					// The page itself must not scroll sideways at any format.
					await t.check(`${page} ${f.name} ${theme}: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1));
				},
			])
		)
	)
);

// The review screen at the Duo's landscape size: its Send and Back replace the deck's navigation.
tests['status duo-landscape review light'] = async (t) => {
	await t.newContext({ viewport: { width: 951, height: 669 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1 });
	await t.goto('/app/status?safe=0,84,34,0', { settleMs: 300 });
	await openQuestions(t);
	await t.page.locator('.plate-tools.only-land .primary').click();
	await t.page.waitForSelector('.review .q-send .primary');
	await t.page.evaluate(() => { document.documentElement.setAttribute('data-theme', 'light'); document.activeElement?.blur?.(); });
	await t.page.addStyleTag({ content: UNPIN });
	await t.page.waitForTimeout(250);
	const out = t.args.out || path.join(process.cwd(), 'frames-out');
	fs.mkdirSync(out, { recursive: true });
	const file = path.join(out, 'status-duo-landscape-review-light.png');
	await t.page.screenshot({ path: file, fullPage: true });
	await t.check('review at Duo landscape: Send on screen', await t.page.locator('.review .q-send .primary').isVisible());
	await t.check('review at Duo landscape: saved', fs.statSync(file).size > 5000);
};
