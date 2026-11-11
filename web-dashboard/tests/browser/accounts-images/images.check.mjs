// Accounts & Settings in each format and theme, as whole pages: <format>-<theme>.png in `--set out=<folder>`, and the
// Slint page it replaces (classic `/accounts`) at the desktop widths, with a window tall enough to hold its whole
// page, as slint-<format>-<theme>.png. Made-up data only (../accounts-fixture.mjs, `rich`, with an Update apps run).
import fs from 'node:fs';
import path from 'node:path';
import { fakeServer, serve, updateJob } from '../accounts-fixture.mjs';

const FORMATS = [
	{ name: 'desktop-1024', width: 1024, height: 768, safe: '0,0,0,0', mobile: false, slint: true },
	{ name: 'desktop-1440', width: 1440, height: 900, safe: '0,0,0,0', mobile: false, slint: true },
	{ name: 'desktop-2560', width: 2560, height: 1440, safe: '0,0,0,0', mobile: false, slint: true },
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

async function shoot(t, f, theme, { slint }) {
	await t.newContext({ viewport: { width: f.width, height: slint ? 3400 : f.height }, isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1, serviceWorkers: 'block' });
	await t.page.addInitScript((mode) => { try { localStorage.setItem('aac-theme', mode); } catch {} }, theme);
	await t.app.signIn();
	const server = fakeServer({ rich: true, update: updateJob() });
	await serve(t, server);
	const out = t.args.out || path.join(process.cwd(), 'accounts-images-out');
	fs.mkdirSync(out, { recursive: true });
	if (slint) {
		await t.goto('/accounts?e2e', { settleMs: 300 });
		await t.until('the Slint page has its accounts', () => (globalThis.__aacLastAccounts?.colA?.length ?? 0) > 0 && document.querySelector('#loading')?.hidden, null, { timeout: 60000, hard: true });
		await t.page.waitForTimeout(3000);
		const file = path.join(out, `slint-${f.name}-${theme}.png`);
		await t.page.screenshot({ path: file });
		await t.check(`slint ${f.name} ${theme}: saved`, fs.statSync(file).size > 5000);
		return;
	}
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

export const tests = Object.fromEntries(
	FORMATS.flatMap((f) => ['light', 'dark'].flatMap((theme) => [
		[`accounts ${f.name} ${theme}`, (t) => shoot(t, f, theme, { slint: false })],
		...(f.slint ? [[`slint ${f.name} ${theme}`, (t) => shoot(t, f, theme, { slint: true })]] : []),
	]))
);
