// Upgrading an installed app from the previous release (review M1, Astra and Sol, 2026-11-14) against the native Rust
// dashboard server (ai-account-center / aac-http). One rule: the service worker never evicts the last usable app copy
// until a complete replacement is cached, and a cached classic page always finds its way to the app frame.
// The previous release is real (fixtures/previous); the app is this build served by Rust StaticUi.
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const repo = new URL('../../../../', import.meta.url);
const { stampedPrecache } = require(new URL('scripts/ui-precache.js', repo).pathname);
const CLASSIC = fs.readFileSync(new URL('./fixtures/previous/classic-index.html', import.meta.url), 'utf8');
const OLD_CACHE = 'aac-shell-f52e1a7c6000';
/** Longer than the old worker's 3 s navigation timeout and than the checks below wait: only a cached page can win. */
const SLOW_MS = 12000;

/** A device on the previous release: signed in on temporary CCS_HOME, its old worker installed and controlling. */
async function previousRelease(t) {
  await t.newContext({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'allow' });
  await t.goto('/__upgrade/ready');
  await t.app.signIn();
  const { builds } = await t.app.control({ uiVersion: 'current', oldWorker: true, delayClassicMs: 0, failPath: null });
  await t.page.evaluate(async () => {
    await navigator.serviceWorker.register('/sw.js');
    await navigator.serviceWorker.ready;
  });
  await t.page.waitForFunction(() => !!navigator.serviceWorker.controller, null, { timeout: 15000 });
  return builds;
}

/** The previous release's worker also kept the classic start page this device chose, and the retired bridge. */
async function cachedClassicPage(t) {
  await t.page.evaluate(
    async ({ html, name }) => {
      localStorage.setItem('aac-frame', 'classic');
      const cache = await caches.open(name);
      await cache.put('/', new Response(html, { headers: { 'Content-Type': 'text/html' } }));
      // Stand-in for the retired bridge that never leaves the loading screen: only the network's /bridge.js moves on.
      await cache.put('/bridge.js', new Response('// retired bridge\n', { headers: { 'Content-Type': 'text/javascript' } }));
    },
    { html: CLASSIC, name: OLD_CACHE }
  );
}

/** The app frame started signed in (real session on temporary CCS_HOME) within `ms`. */
const frameSignedIn = (t, ms) =>
  t.page.waitForFunction(
    () => window.__aacFrame === true && !!document.querySelector('.shell') && !document.querySelector('.signin-page') && !document.querySelector('.boot-splash'),
    null,
    { timeout: ms }
  );

/** The app frame started (shell or offline sign-in card) within `ms`. */
const frameStarted = (t, ms) =>
  t.page.waitForFunction(() => window.__aacFrame === true && !!document.querySelector('.shell,.signin-page'), null, {
    timeout: ms,
  });

const runningBuild = (page) =>
  page.evaluate(
    () =>
      /\/pkg\/([a-f0-9]{12})\//.exec(document.querySelector('link[rel="modulepreload"]')?.getAttribute('href') || '')?.[1] ??
      null
  );

/** Asks the registration for this build's worker; resolves with what the install came to ('installed' or 'redundant'). */
const installOutcome = (t) =>
  t.page.evaluate(async () => {
    const reg = await navigator.serviceWorker.getRegistration('/');
    const found = new Promise((resolve) => reg.addEventListener('updatefound', () => resolve(reg.installing), { once: true }));
    await reg.update().catch(() => {});
    const worker = await Promise.race([found, new Promise((resolve) => setTimeout(() => resolve(null), 20000))]);
    if (!worker) return 'no update';
    const settled = (s) => s === 'installed' || s === 'redundant' || s === 'activated';
    if (settled(worker.state)) return worker.state;
    return new Promise((resolve) => {
      worker.addEventListener('statechange', () => settled(worker.state) && resolve(worker.state));
      setTimeout(() => resolve(`still ${worker.state}`), 45000);
    });
  });

/** Closes the last client, so the waiting worker activates as on the next launch; returns its caches once active. */
async function nextLaunch(t, build) {
  await t.page.goto('about:blank');
  for (let i = 0; i < 150; i++) {
    for (const worker of t.context.serviceWorkers()) {
      try {
        const state = await worker.evaluate(async () => ({
          build: BUILD_ID,
          state: self.registration.active?.state,
          names: await caches.keys(),
        }));
        if (state.build === build && state.state === 'activated') return state;
      } catch {
        // a worker that went away meanwhile
      }
    }
    await t.page.waitForTimeout(100);
  }
  return null;
}

async function offlineLaunch(t, origin, address, label, expectedBuildId = null) {
  await t.context.setOffline(true);
  try {
    await t.page.goto(new URL(address, origin).href, { waitUntil: 'commit', timeout: 20000 });
    await frameStarted(t, 20000).catch(() => {});
    const result = await t.page.evaluate(() => ({
      frame: window.__aacFrame === true,
      path: location.pathname,
      buildId:
        /\/pkg\/([a-f0-9]{12})\//.exec(document.querySelector('link[rel="modulepreload"]')?.getAttribute('href') || '')?.[1] ??
        null,
      text: document.body.innerText.slice(0, 160),
    }));
    await t.check(`${label}: ${address} opens the app offline`, result.frame, JSON.stringify(result));
    if (expectedBuildId) {
      await t.equal(`${label}: ${address} runs the expected build (no stale shell)`, result.buildId, expectedBuildId);
    }
    return result;
  } finally {
    await t.context.setOffline(false);
  }
}

export const tests = {
  async 'a cached classic page on a slow first launch moves to the frame by itself (Astra M1)'(t) {
    const builds = await previousRelease(t);
    const origin = new URL(t.page.url()).origin;
    await cachedClassicPage(t);
    // The update is on the native server; the first launch after it is slow.
    await t.app.control({ oldWorker: false, delayClassicMs: SLOW_MS });
    await t.page.goto(new URL('/', origin).href, { waitUntil: 'commit', timeout: 20000 });
    await frameSignedIn(t, 15000);
    const state = await t.page.evaluate(() => ({
      path: location.pathname,
      search: location.search,
      classic: localStorage.getItem('aac-frame'),
    }));
    await t.equal('it opens Home in the frame', `${state.path}${state.search}`, '/app/home');
    await t.equal('the classic choice is forgotten', state.classic, null);
    await t.equal('the frame runs the current build', await runningBuild(t.page), builds.current.buildId);
    await t.shot('upgrade-slow-classic-home', 'After the slow first launch: the stored classic page moved itself to the frame (Home, empty test CCS_HOME).');
  },

  async 'the cached classic page keeps its destination (Astra M1)'(t) {
    const builds = await previousRelease(t);
    const origin = new URL(t.page.url()).origin;
    await cachedClassicPage(t);
    await t.app.control({ oldWorker: false, delayClassicMs: SLOW_MS });
    // The old worker answers this address with its stored start page after 3 s.
    await t.page.goto(new URL('/accounts/claude?e2e&view=accounts', origin).href, { waitUntil: 'commit', timeout: 20000 });
    await frameSignedIn(t, 15000);
    const state = await t.page.evaluate(() => `${location.pathname}${location.search}`);
    await t.equal('the classic address opens its tab with the rest of its query', state, '/app/accounts?e2e');
    await t.equal('the frame runs the current build', await runningBuild(t.page), builds.current.buildId);
  },

  async 'update, close the last client, then an offline launch opens the new copy (Sol M1)'(t) {
    const builds = await previousRelease(t);
    const origin = new URL(t.page.url()).origin;
    const newCache = `aac-shell-${builds.current.shellBuildId}`;
    const precache = stampedPrecache(fs.readFileSync(path.join(builds.current.dir, 'sw.js'), 'utf8')) ?? [];
    // The previous release runs this app for a while and keeps its copy as it goes (warm-up after 4 s).
    await t.goto('/');
    await frameSignedIn(t, 20000);
    await t.page.waitForTimeout(6000);
    await t.goto('/__upgrade/ready');
    await t.app.control({ oldWorker: false });
    await t.equal('the new worker downloads the whole app and waits', await installOutcome(t), 'installed');
    const stored = await t.page.evaluate(async (name) =>
      (await (await caches.open(name)).keys()).map((r) => {
        const u = new URL(r.url);
        return `${u.pathname}${u.search}`;
      }), newCache);
    await t.check(
      `its copy holds all ${precache.length} addresses before it may replace anything`,
      precache.length > 0 && precache.every((a) => stored.includes(a)),
      JSON.stringify(precache.filter((a) => !stored.includes(a)))
    );
    const active = await nextLaunch(t, builds.current.shellBuildId);
    await t.check('the next launch activates it', !!active, JSON.stringify(active));
    await t.equal('with its own copy whole, the older copy goes', JSON.stringify(active?.names ?? []), JSON.stringify([newCache]));
    for (const address of ['/', '/app/accounts', '/analytics']) {
      await offlineLaunch(t, origin, address, 'next launch, offline', builds.current.buildId);
    }
    await t.shot('upgrade-offline-next-launch', 'The first launch after the update, offline: the frame from the new copy (offline sign-in card).');
  },

  async 'a failed download leaves the installed copy in charge (Sol M1)'(t) {
    await previousRelease(t);
    const origin = new URL(t.page.url()).origin;
    await t.goto('/');
    await frameSignedIn(t, 20000);
    await t.page.waitForTimeout(6000);
    await t.goto('/__upgrade/ready');
    await t.app.control({ oldWorker: false, failPath: '/bridge.js' });
    await t.equal('one address fails: the install fails', await installOutcome(t), 'redundant');
    const names = await t.page.evaluate(() => caches.keys());
    await t.equal('the installed copy stays and no partial copy is kept', JSON.stringify(names), JSON.stringify([OLD_CACHE]));
    await offlineLaunch(t, origin, '/', 'after the failed update, offline');
    // The next try, with the native server whole again, installs.
    await t.goto('/__upgrade/ready');
    await t.app.control({ failPath: null });
    await t.equal('the next try installs', await installOutcome(t), 'installed');
    await t.noConsoleErrors('only the refused asset and offline probes may fail', { allow: [/status of 503/] });
  },
};
