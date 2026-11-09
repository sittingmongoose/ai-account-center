// Each page in each format and theme, as a whole-page image. Sticky chrome is unpinned for the shot so the rail and
// tab bar sit once at their place in the page, like the mockups' "Whole page" view.
import fs from 'node:fs';
import path from 'node:path';

const PAGES = ['status', 'scoreboard'];
const FORMATS = [
	{ name: 'desktop', width: 1440, height: 900, safe: '0,0,0,0', mobile: false },
	{ name: 'iphone', width: 402, height: 874, safe: '62,0,34,0', mobile: true },
	{ name: 'duo-folded', width: 466, height: 678, safe: '0,84,34,0', mobile: true },
	{ name: 'duo-portrait', width: 669, height: 951, safe: '82,0,34,0', mobile: true },
	{ name: 'duo-landscape', width: 951, height: 669, safe: '0,84,34,0', mobile: true },
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
					await t.newContext({ viewport: { width: f.width, height: f.height }, isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1 });
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
