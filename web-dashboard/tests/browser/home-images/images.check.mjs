// Home in the app frame, whole page, in each format and theme (`home-<format>-<theme>.png`), and the classic Slint Home
// on the same data (`slint-home-<format>-<theme>.png`, drawn in a window tall enough for the whole page, since the canvas
// scrolls inside itself) for side-by-side review. Made-up data only (home-fixture.mjs).
import fs from 'node:fs';
import path from 'node:path';
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';

const FORMATS = [
	{ name: 'desktop-1024', width: 1024, height: 800, safe: '0,0,0,0', mobile: false },
	{ name: 'desktop-1440', width: 1440, height: 900, safe: '0,0,0,0', mobile: false },
	{ name: 'desktop-2560', width: 2560, height: 1300, safe: '0,0,0,0', mobile: false },
	{ name: 'iphone', width: 402, height: 812, safe: '0,0,34,0', mobile: true },
	{ name: 'duo-folded', width: 466, height: 678, safe: '0,84,34,0', mobile: true },
	{ name: 'duo-folded-rotated', width: 678, height: 466, safe: '0,84,21,0', mobile: true },
	{ name: 'duo-open-upright', width: 669, height: 869, safe: '0,0,34,0', mobile: true },
	{ name: 'duo-folded-turned-strip-right', width: 678, height: 466, safe: '0,84,34,0', mobile: true },
	{ name: 'duo-folded-turned-strip-left', width: 678, height: 466, safe: '0,0,34,84', mobile: true },
	{ name: 'duo-landscape', width: 951, height: 669, safe: '0,84,34,0', mobile: true },
];
const UNPIN = `html { scrollbar-width: none; }
.rail, .tabbar, .topbar, .chrome, .band-head { position: static !important; }
.rail { height: auto !important; align-self: stretch !important; }`;
const only = (t) => (t.args.only ? new RegExp(t.args.only) : null);

async function prepare(t, f, theme) {
	await t.newContext({ viewport: { width: f.width, height: f.height }, isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
	await t.page.addInitScript((mode) => { try { localStorage.setItem('aac-theme', mode); } catch {} }, theme);
	await t.app.signIn();
	await t.app.stub('/api/accounts/dashboard', homeAccounts());
	await t.app.stub('/api/claude/desktop-profiles', homeProfiles());
}

function out(t, file) {
	const dir = t.args.out || path.join(process.cwd(), 'home-images-out');
	fs.mkdirSync(dir, { recursive: true });
	return path.join(dir, file);
}

export const tests = Object.fromEntries(
	FORMATS.flatMap((f) =>
		['light', 'dark'].flatMap((theme) => [
			[
				`home ${f.name} ${theme}`,
				async (t) => {
					if (only(t) && !only(t).test(`home-${f.name}-${theme}`)) return;
					await prepare(t, f, theme);
					await t.goto(`/app/home?safe=${f.safe}`, { settleMs: 300 });
					await t.until('Home drawn', () => document.querySelectorAll('.hrow').length >= 8 && document.querySelectorAll('.hcard').length >= 6, null, { timeout: 60000, hard: true });
					await t.page.waitForTimeout(500);
					await t.page.evaluate(() => document.activeElement?.blur?.());
					await t.page.addStyleTag({ content: UNPIN });
					await t.page.waitForTimeout(250);
					const file = out(t, `home-${f.name}-${theme}.png`);
					await t.page.screenshot({ path: file, fullPage: true });
					await t.check(`home ${f.name} ${theme}: saved`, fs.statSync(file).size > 5000);
					await t.check(`home ${f.name} ${theme}: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
				},
			],
			[
				`slint ${f.name} ${theme}`,
				async (t) => {
					if (t.args.slint === '0' || (only(t) && !only(t).test(`slint-home-${f.name}-${theme}`))) return;
					// The classic app, in a window tall enough to show its whole Home (the canvas scrolls inside itself).
					await prepare(t, { ...f, height: f.mobile ? 2600 : 2000 }, theme);
					await t.goto('/?e2e', { settleMs: 300 });
					await t.until('Slint Home drawn', () => !!window.__aacLastAuth?.signedIn && document.querySelector('#loading')?.hidden, null, { timeout: 60000, hard: true });
					await t.page.waitForTimeout(3500);
					const file = out(t, `slint-home-${f.name}-${theme}.png`);
					await t.page.screenshot({ path: file });
					await t.check(`slint ${f.name} ${theme}: saved`, fs.statSync(file).size > 5000);
				},
			],
		])
	)
);
