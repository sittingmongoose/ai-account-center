// "New version · Reload" (2026-11-19): after an install the installed app kept offering the new version, because a
// plain reload under the old worker gets the old build back. The fixture server serves a second build of this checkout
// (fixture-server.cjs secondBuild: the same files under another runtime id and shell stamp). Reload must hand over to
// the waiting worker and run the new build once; the notice must not come back, and it must never be offered for a
// build whose worker did not install.
import fs from 'node:fs';
const root = new URL('../../../../', import.meta.url);
const current = JSON.parse(fs.readFileSync(new URL('dist/ui/ui-build-manifest.json', root)));
const flip = id => id.replace(/[0-9a-f]/g, c => ((parseInt(c, 16) + 8) % 16).toString(16));
const second = { pkg: flip(current.buildId), stamp: flip(current.shellBuildId) };

const control = (page, values) => page.evaluate(async values => (await fetch('/__upgrade/control', {
  method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(values),
})).json(), values);
const frame = (page, ms = 15000) => page.waitForFunction(() => window.__aacFrame === true && !!document.querySelector('.shell,.signin-page'), null, { timeout: ms });
const running = page => page.evaluate(() => /\/pkg\/([a-f0-9]{12})\//.exec(document.querySelector('link[rel="modulepreload"]')?.getAttribute('href') || '')?.[1] ?? null);
const workers = page => page.evaluate(async () => {
  const reg = await navigator.serviceWorker.getRegistration('/');
  return { waiting: reg?.waiting?.state ?? null, installing: reg?.installing?.state ?? null, active: reg?.active?.state ?? null, names: await caches.keys() };
});
/** A field with the focus: the frame offers the notice instead of handing over by itself. */
const typeInto = page => page.evaluate(() => {
  let box = document.getElementById('kit-reload-note');
  if (!box) {
    box = document.createElement('textarea');
    box.id = 'kit-reload-note';
    box.style.cssText = 'position:fixed;left:0;bottom:0;width:40px;height:20px;z-index:1';
    document.body.appendChild(box);
  }
  box.focus();
});
const resume = page => page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));

async function start(t) {
  await t.newContext({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'allow' });
  await t.goto('/__upgrade/ready');
  await control(t.page, { uiVersion: 'current', oldWorker: false, delayClassicMs: 0, failPath: null });
  await t.page.evaluate(async () => { await navigator.serviceWorker.register('/sw.js'); await navigator.serviceWorker.ready; });
  await t.page.waitForFunction(() => !!navigator.serviceWorker.controller);
  await t.goto('/app/home');
  await frame(t.page);
  await t.page.waitForTimeout(6000); // the start-up build check (4 s) and the offline warm-up
  await t.equal('this checkout\'s build runs', await running(t.page), current.buildId);
}

export const tests = {
  async 'Reload hands over to the waiting worker once; the notice does not come back'(t) {
    await start(t);
    await control(t.page, { uiVersion: 'second' });
    await typeInto(t.page);
    await resume(t.page);
    await t.until('the frame offers the new version once its worker installed', () => !!document.querySelector('.new-build'), null, { timeout: 45000 });
    const offered = await workers(t.page);
    await t.equal('a complete new worker waits behind the notice', offered.waiting, 'installed', JSON.stringify(offered));
    // The cause: a plain reload is answered by the worker still in charge, with its own build.
    await t.page.reload();
    await frame(t.page);
    await t.equal('a plain reload keeps the old build (the worker in charge answers it)', await running(t.page), current.buildId);
    await typeInto(t.page);
    await resume(t.page);
    await t.until('the notice is offered again: the worker still waits', () => !!document.querySelector('.new-build'), null, { timeout: 15000 });
    await t.shot('notice', 'New version notice with a fully installed worker waiting. Synthetic data only.');
    await Promise.all([
      t.page.waitForEvent('load', { timeout: 30000 }),
      t.page.click('.new-build button.btn.primary'),
    ]);
    await frame(t.page);
    await t.equal('Reload runs the new build', await running(t.page), second.pkg);
    const after = await workers(t.page);
    await t.check('the new worker is in charge with nothing waiting', after.active === 'activated' && !after.waiting && !after.installing, JSON.stringify(after));
    await t.equal('only the new build\'s copy remains', JSON.stringify(after.names), JSON.stringify(['aac-shell-' + second.stamp]));
    await t.page.waitForTimeout(5000);
    await typeInto(t.page);
    await resume(t.page);
    await t.page.waitForTimeout(8000);
    await t.check('the notice does not come back', !(await t.page.$('.new-build')));
    await t.equal('still the new build', await running(t.page), second.pkg);
    await t.noConsoleErrors('no errors across the hand-over');
  },

  async 'resumed with nothing typed: the frame hands over by itself and runs the new build'(t) {
    await start(t);
    await control(t.page, { uiVersion: 'second' });
    const loaded = t.page.waitForEvent('load', { timeout: 60000 });
    await resume(t.page);
    await loaded;
    await frame(t.page);
    await t.equal('the resumed app runs the new build', await running(t.page), second.pkg);
    const after = await workers(t.page);
    await t.equal('only the new build\'s copy remains', JSON.stringify(after.names), JSON.stringify(['aac-shell-' + second.stamp]));
    await t.noConsoleErrors('no errors across the hand-over');
  },

  async 'a build whose worker cannot install is never offered'(t) {
    await start(t);
    // the second build's runtime fails to download: its worker's install fails
    await control(t.page, { uiVersion: 'second', failPath: `/pkg/${second.pkg}/aac_web_bg.wasm` });
    await typeInto(t.page);
    await resume(t.page);
    await t.page.waitForTimeout(36000); // the frame waits up to 30 s for an install
    await t.check('no notice for a build that did not install', !(await t.page.$('.new-build')));
    const state = await workers(t.page);
    await t.check('nothing waits; the installed copy stays in charge', !state.waiting && state.active === 'activated' && JSON.stringify(state.names) === JSON.stringify(['aac-shell-' + current.shellBuildId]), JSON.stringify(state));
    await t.equal('the installed build keeps running', await running(t.page), current.buildId);
    await t.noConsoleErrors('only the refused runtime download may fail', { allow: [/status of 503/] });
  },
};
