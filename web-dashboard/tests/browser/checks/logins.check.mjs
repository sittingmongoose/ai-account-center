// Logins (Central Logins step 1) on Accounts & Settings: the real GET /api/accounts/logins answers a read-only view
// with no secret field, and the plate draws every row of the device fixture (web-dashboard/leptos/fixtures/
// logins-view.json, invented names only) as one aligned line on a desktop and as three short lines on a phone, with
// no sideways scroll. LOOK: the two screenshots.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const FIXTURE = JSON.parse(fs.readFileSync(path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos/fixtures/logins-view.json'), 'utf8'));
const ROWS = '.route:not([hidden]) .lg-plate .lg-row:not(.lg-headrow)';
const ALLOWED = ['id', 'provider', 'providerLabel', 'account', 'computer', 'tool', 'kind', 'kindLabel', 'endsAt', 'endsText', 'renewer', 'health', 'healthText', 'lastRenewal'];

const noSideways = async (t, name) => t.check(name, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));

export const tests = {
	async 'logins: the real route reads only and carries no secret field'(t) {
		await t.app.open({ page: 'accounts' });
		const answer = await t.page.evaluate(async () => {
			const r = await fetch('/api/accounts/logins', { credentials: 'same-origin', cache: 'no-store' });
			return { status: r.status, store: r.headers.get('cache-control'), text: await r.text() };
		});
		await t.check('GET answers 200', answer.status === 200, answer);
		await t.check('no-store', /no-store/.test(answer.store || ''), answer.store);
		const view = JSON.parse(answer.text);
		await t.check('view version 1', view.version === 1, view.version);
		const keys = [...new Set((view.rows || []).flatMap((row) => Object.keys(row)))];
		await t.check('rows carry only the allowlisted fields', keys.every((k) => ALLOWED.includes(k)), keys);
		const leak = answer.text.match(/sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{8}|access_?token|refresh_?token|cookie|Bearer |\/home\/|\/Users\/|\.claude\/|\.codex\/|auth\.json/i);
		await t.check('no credential-shaped text', leak === null, leak && leak[0]);
	},

	async 'logins: every row drawn on a desktop and a phone'(t) {
		await t.newContext({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1, serviceWorkers: 'block' });
		await t.app.stub('/api/accounts/logins', { ...FIXTURE, generatedAt: new Date().toISOString() });
		// the registry answered empty, so no account but the fixture's can reach a screenshot
		await t.app.stub('/api/accounts/registry', { providers: [], accounts: [], jobs: [], trash: [] });
		await t.app.noResets();
		await t.app.open({ page: 'accounts' });
		await t.until('every login row is drawn', (n) => document.querySelectorAll(n).length > 0, ROWS, { timeout: 30000 });
		await t.check('one line per fixture row', await t.page.locator(ROWS).count() === FIXTURE.rows.length);
		await t.check('the head counts', /15 logins · 3 need you · 1 ends soon/.test(await t.page.locator('.lg-plate .plate-meta').innerText()));
		await t.check('a CAPTCHA wait reads in the critical tone', await t.page.locator(`${ROWS} .lg-h.crit`).count() === 3);
		await t.page.locator('.lg-plate').scrollIntoViewIfNeeded();
		await t.crop('logins-desktop', '.lg-plate', 'Logins plate at 1920: header row, then 15 one-line rows, the three that need you first in red, then the one ending soon in amber; every health word on one line; long account ends with an ellipsis');
		await noSideways(t, 'no sideways scroll at 1920');
		await t.viewport(390, 844);
		await t.page.waitForTimeout(600);
		await t.check('the header row is hidden on a phone', await t.page.locator('.lg-headrow').isHidden());
		const tall = await t.page.locator(ROWS).first().evaluate((el) => el.getBoundingClientRect().height);
		await t.check('a phone row is three short lines', tall > 30 && tall < 70, tall);
		await noSideways(t, 'no sideways scroll at 390');
		await t.page.locator('.lg-plate').scrollIntoViewIfNeeded();
		await t.crop('logins-phone', '.lg-plate', 'Logins plate at 390: each login three short lines (account | health; computer, tool, end; kind, renewer), small text, no orphaned word');
		await t.audit('accounts with logins at 390');
	},
};
