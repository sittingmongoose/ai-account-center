// Accounts & Settings in each format and theme, as whole pages: <format>-<theme>.png in `--set out=<folder>`. (Until
// the classic Slint app was retired, its page was shot at the desktop widths too.) Made-up data only
// (../accounts-fixture.mjs, `rich`, with an Update apps run).
import fs from 'node:fs';
import path from 'node:path';
import { fakeServer, serve, updateJob } from '../accounts-fixture.mjs';

const DUO_SCREEN = { width: 466, height: 678 };
const FORMATS = [
	{ name: 'desktop-1024', width: 1024, height: 768, safe: '0,0,0,0', mobile: false },
	{ name: 'desktop-1440', width: 1440, height: 900, safe: '0,0,0,0', mobile: false },
	{ name: 'desktop-2560', width: 2560, height: 1440, safe: '0,0,0,0', mobile: false },
	{ name: 'iphone', width: 402, height: 812, safe: '0,0,34,0', mobile: true },
	{ name: 'duo-folded', width: 466, height: 678, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-open-upright', width: 669, height: 869, safe: '0,0,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-right', width: 678, height: 466, safe: '0,84,34,0', mobile: true, duo: true },
	{ name: 'duo-folded-turned-strip-left', width: 678, height: 466, safe: '0,0,34,84', mobile: true, duo: true },
	{ name: 'duo-landscape', width: 951, height: 669, safe: '0,84,34,0', mobile: true, duo: true },
];
const UNPIN = `html { scrollbar-width: none; }
.rail, .tabbar, .topbar, .chrome, .band-head { position: static !important; }
.rail { height: auto !important; align-self: stretch !important; }`;

async function shoot(t, f, theme) {
	await t.newContext({ viewport: { width: f.width, height: f.height }, ...(f.duo ? { screen: DUO_SCREEN } : {}), isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
	await t.page.addInitScript((mode) => { try { localStorage.setItem('aac-theme', mode); } catch {} }, theme);
	await t.app.signIn();
	const server = fakeServer({ rich: true, update: updateJob() });
	await serve(t, server);
	const out = t.args.out || path.join(process.cwd(), 'accounts-images-out');
	fs.mkdirSync(out, { recursive: true });
	await t.goto(`/app/accounts?e2e&safe=${f.safe}`, { settleMs: 300 });
	await t.until('the native page is drawn', () => document.querySelectorAll('.prov[data-provider]').length >= 9 && !!document.querySelector('.ut-row, .upd-head[aria-expanded="false"]') && document.querySelectorAll('.acc-item').length >= 12, null, { timeout: 60000, hard: true });
	await t.page.waitForTimeout(600);
	await t.page.evaluate(() => document.activeElement?.blur?.());
	await t.page.addStyleTag({ content: UNPIN });
	await t.page.waitForTimeout(250);
	const file = path.join(out, `${f.name}-${theme}.png`);
	await t.page.screenshot({ path: file, fullPage: true });
	const height = await t.page.evaluate(() => document.documentElement.scrollHeight);
	fs.appendFileSync(path.join(out, 'heights.txt'), `${f.name} ${theme} ${height}\n`);
	await t.check(`${f.name} ${theme}: saved`, fs.statSync(file).size > 5000);
	await t.check(`${f.name} ${theme}: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
}

async function step3Shot(t, on) {
	await t.newContext({ viewport: { width: 402, height: 874 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1, serviceWorkers: 'block' });
	await t.app.signIn();
	const server = fakeServer({ rich: true, update: updateJob(), loginsStep3: on });
	await serve(t, server);
	const out = t.args.out || path.join(process.cwd(), 'accounts-images-out');
	fs.mkdirSync(out, { recursive: true });
	await t.goto('/app/accounts?e2e&safe=0,0,0,0', { settleMs: 300 });
	await t.until('the native page is drawn', () => document.querySelectorAll('.prov[data-provider]').length >= 9 && document.querySelectorAll('.acc-item').length >= 12, null, { timeout: 60000, hard: true });
	if (on) {
		await t.until('Codex rows name T3', () => [...document.querySelectorAll('.acc-item[data-row^="codex:"] .acc-acts .quiet')].filter((el) => el.textContent === 'Signs in through T3 on this computer').length >= 2, null, { timeout: 15000, hard: true });
		await t.equal('no AAC Codex sign-in control', await t.page.$$eval('.acc-item[data-row^="codex:"] [data-probe^="signin-again:"]', (els) => els.length), 0);
	} else {
		await t.until('Codex rows offer AAC sign-in', () => document.querySelectorAll('.acc-item[data-row^="codex:"] [data-probe^="signin-again:"]').length >= 2, null, { timeout: 15000, hard: true });
	}
	await t.page.waitForTimeout(400);
	await t.page.evaluate(() => document.activeElement?.blur?.());
	await t.page.addStyleTag({ content: UNPIN });
	await t.page.waitForTimeout(250);
	const file = path.join(out, `accounts-step3-${on ? 'on' : 'off'}-402x874.png`);
	await t.page.screenshot({ path: file, fullPage: true });
	await t.check(`step3 ${on ? 'on' : 'off'} 402x874: saved`, fs.statSync(file).size > 5000);
	await t.check(`step3 ${on ? 'on' : 'off'} 402x874: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
}

export const tests = {
	...Object.fromEntries(
		FORMATS.flatMap((f) => ['light', 'dark'].map((theme) => [`accounts ${f.name} ${theme}`, (t) => shoot(t, f, theme)]))
	),
	'accounts step3 off 402x874': (t) => step3Shot(t, false),
	'accounts step3 on 402x874': (t) => step3Shot(t, true),
};
