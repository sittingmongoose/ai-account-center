// Service-worker build upgrade against the native Rust dashboard server (aac-http / ai-account-center):
// an older built Leptos app is installed with its service worker registered and controlling the signed-in page,
// then the native server switches to serving the newer built Leptos app (dist/ui). Verifies:
//   1. "New version · Reload" hands over to the waiting worker once, runs the newer build with no stale shell
//      (including offline) and no reload loop, and preserves the signed-in session.
//   2. Resuming with nothing typed hands over automatically once with no reload loop.
//   3. A newer build whose worker cannot install (missing/503 wasm) is never offered and leaves the older build running.
const frameSignedIn = (page, ms = 20000) =>
  page.waitForFunction(
    () => window.__aacFrame === true && !!document.querySelector('.shell') && !document.querySelector('.signin-page') && !document.querySelector('.boot-splash'),
    null,
    { timeout: ms }
  );

const frameAny = (page, ms = 20000) =>
  page.waitForFunction(() => window.__aacFrame === true && !!document.querySelector('.shell,.signin-page'), null, {
    timeout: ms,
  });

const runningBuild = (page) =>
  page.evaluate(
    () =>
      /\/pkg\/([a-f0-9]{12})\//.exec(document.querySelector('link[rel="modulepreload"]')?.getAttribute('href') || '')?.[1] ??
      null
  );

const workerState = (page) =>
  page.evaluate(async () => {
    const reg = await navigator.serviceWorker.getRegistration('/');
    return {
      controlled: !!navigator.serviceWorker.controller,
      waiting: reg?.waiting?.state ?? null,
      installing: reg?.installing?.state ?? null,
      active: reg?.active?.state ?? null,
      names: await caches.keys(),
    };
  });

/** A field with focus: the frame offers the notice instead of handing over automatically over unsaved typing. */
const typeInto = (page) =>
  page.evaluate(() => {
    let box = document.getElementById('kit-reload-note');
    if (!box) {
      box = document.createElement('textarea');
      box.id = 'kit-reload-note';
      box.style.cssText = 'position:fixed;left:0;bottom:0;width:40px;height:20px;z-index:1';
      document.body.appendChild(box);
    }
    box.focus();
  });

const resume = (page) => page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));

/**
 * Installs and opens the older built Leptos app signed in (real loopback test login on temporary CCS_HOME), letting
 * the app itself register /sw.js and take control of the page.
 */
async function startOnOlderBuild(t) {
  await t.newContext({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'allow' });
  await t.goto('/__upgrade/ready');
  await t.app.signIn();
  const { builds } = await t.app.control({ uiVersion: 'older', oldWorker: false, delayClassicMs: 0, failPath: null });
  await t.goto('/app/home');
  await frameSignedIn(t.page, 30000);
  // The app registers /sw.js itself (frame.rs warm_offline_copies); wait until that worker installs, activates and claims.
  await t.until(
    'older build service worker is registered and controlling the page',
    async () => {
      const reg = await navigator.serviceWorker.getRegistration('/');
      const names = await caches.keys();
      return !!navigator.serviceWorker.controller && reg?.active?.state === 'activated' && !reg?.installing && !reg?.waiting && names.length > 0;
    },
    null,
    { timeout: 30000, hard: true }
  );
  // Let the 4 s start-up build check and offline warm-up settle before switching the server to the newer build.
  await t.page.waitForTimeout(5500);
  await t.equal('the older build is running', await runningBuild(t.page), builds.older.buildId);
  const state = await workerState(t.page);
  await t.equal('only the older build cache is installed', JSON.stringify(state.names), JSON.stringify([`aac-shell-${builds.older.shellBuildId}`]));
  return builds;
}

export const tests = {
  async 'Reload hands over from older build to newer build once; no stale shell and no reload loop'(t) {
    const builds = await startOnOlderBuild(t);
    const origin = new URL(t.page.url()).origin;
    await t.app.control({ uiVersion: 'current' });
    await typeInto(t.page);
    await resume(t.page);
    await t.until('the frame offers the newer version once its worker installed', () => !!document.querySelector('.new-build'), null, { timeout: 45000 });
    const offered = await workerState(t.page);
    await t.equal('a complete newer worker waits behind the notice', offered.waiting, 'installed', JSON.stringify(offered));

    // A plain reload stays on the installed older worker (never mixes old shell with new runtime).
    await t.page.reload();
    await frameSignedIn(t.page, 20000);
    await t.equal('a plain reload keeps the installed older build until hand-over', await runningBuild(t.page), builds.older.buildId);

    await typeInto(t.page);
    await resume(t.page);
    await t.until('the notice is offered again while the newer worker waits', () => !!document.querySelector('.new-build'), null, { timeout: 15000 });
    await t.shot('notice', 'New version notice with a complete newer worker waiting on the native server.');

    let loads = 0;
    const onLoad = () => { loads += 1; };
    t.page.on('load', onLoad);
    try {
      await Promise.all([
        t.page.waitForEvent('load', { timeout: 30000 }),
        t.page.click('.new-build button.btn.primary'),
      ]);
      await frameSignedIn(t.page, 20000);
      await t.equal('Reload runs the newer build', await runningBuild(t.page), builds.current.buildId);
      const after = await workerState(t.page);
      await t.check(
        'the newer worker is in charge with nothing waiting or installing',
        after.controlled && after.active === 'activated' && !after.waiting && !after.installing,
        JSON.stringify(after)
      );
      await t.equal(
        'older shell cache is gone and only the newer copy remains',
        JSON.stringify(after.names),
        JSON.stringify([`aac-shell-${builds.current.shellBuildId}`])
      );
      await t.equal('Reload triggered exactly one navigation', loads, 1);

      // Past the 4 s startup build check and another resume: no reload loop and the notice stays gone.
      await t.page.waitForTimeout(5500);
      await typeInto(t.page);
      await resume(t.page);
      await t.page.waitForTimeout(6000);
      await t.equal('no reload loop after hand-over', loads, 1);
      await t.check('the new-version notice does not come back', !(await t.page.$('.new-build')));
      await t.equal('still running the newer build', await runningBuild(t.page), builds.current.buildId);
      await t.noConsoleErrors('no console errors across the hand-over');
    } finally {
      t.page.off('load', onLoad);
    }

    // Offline launch must serve the newer shell from cache, never a stale older shell.
    await t.context.setOffline(true);
    try {
      for (const address of ['/', '/app/home']) {
        await t.page.goto(new URL(address, origin).href, { waitUntil: 'commit', timeout: 20000 });
        await frameAny(t.page, 20000);
        await t.equal(`offline ${address} serves the newer build (no stale shell)`, await runningBuild(t.page), builds.current.buildId);
      }
    } finally {
      await t.context.setOffline(false);
    }
  },

  async 'resumed with nothing typed: the frame hands over once by itself with no reload loop'(t) {
    const builds = await startOnOlderBuild(t);
    await t.app.control({ uiVersion: 'current' });
    let loads = 0;
    const onLoad = () => { loads += 1; };
    t.page.on('load', onLoad);
    try {
      const loaded = t.page.waitForEvent('load', { timeout: 60000 });
      await resume(t.page);
      await loaded;
      await frameSignedIn(t.page, 20000);
      await t.equal('the resumed app runs the newer build', await runningBuild(t.page), builds.current.buildId);
      const after = await workerState(t.page);
      await t.equal(
        'only the newer build cache remains',
        JSON.stringify(after.names),
        JSON.stringify([`aac-shell-${builds.current.shellBuildId}`])
      );
      await t.equal('automatic hand-over reloaded once', loads, 1);

      // Past the 4 s startup check and another resume with nothing typed: must not reload again.
      await t.page.waitForTimeout(5500);
      await resume(t.page);
      await t.page.waitForTimeout(5000);
      await t.equal('no reload loop on subsequent resumes', loads, 1);
      await t.check('the notice does not appear on the newer build', !(await t.page.$('.new-build')));
      await t.equal('still running the newer build', await runningBuild(t.page), builds.current.buildId);
      await t.noConsoleErrors('no console errors across automatic hand-over');
    } finally {
      t.page.off('load', onLoad);
    }
  },

  async 'a newer build whose worker cannot install is never offered and keeps the older copy'(t) {
    const builds = await startOnOlderBuild(t);
    await t.app.control({ uiVersion: 'current', failPath: `/${builds.current.wasmPath}` });
    let loads = 0;
    const onLoad = () => { loads += 1; };
    t.page.on('load', onLoad);
    try {
      await typeInto(t.page);
      await resume(t.page);
      await t.page.waitForTimeout(36000); // frame.rs INSTALL_WAIT_MS is 30 s
      await t.check('no notice for a build whose worker did not install', !(await t.page.$('.new-build')));
      await t.equal('no unexpected reload when install fails', loads, 0);
      const state = await workerState(t.page);
      await t.check(
        'nothing waits and the installed older copy stays in charge',
        !state.waiting &&
          state.active === 'activated' &&
          JSON.stringify(state.names) === JSON.stringify([`aac-shell-${builds.older.shellBuildId}`]),
        JSON.stringify(state)
      );
      await t.equal('the installed older build keeps running', await runningBuild(t.page), builds.older.buildId);
      await t.noConsoleErrors('only the refused runtime download may fail', { allow: [/status of 503/] });
    } finally {
      t.page.off('load', onLoad);
    }
  },
};
