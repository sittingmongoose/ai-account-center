// Real, different packaged builds served through native Rust StaticUi (aac-http).
// Opt in with --set previousUi=<c281 UI> --set fix1Ui=<5c206f UI>.
// No worker bytes, response bytes or UI routes are rewritten. Failure is only a 503 at the replacement wasm.
import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { homeAccounts, homeProfiles } from '../home-fixture.mjs';

const readBuild = (dir) => {
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'ui-build-manifest.json'), 'utf8'));
  const sha = createHash('sha256').update(fs.readFileSync(path.join(dir, manifest.wasm.path))).digest('hex');
  if (sha !== manifest.wasm.sha256) throw new Error('Retained wasm does not match its build record.');
  return manifest;
};

const frame = (page, ms = 15000) =>
  page.waitForFunction(() => window.__aacFrame === true && !!document.querySelector('.shell,.signin-page'), null, {
    timeout: ms,
  });

const state = (page) =>
  page.evaluate(async () => ({
    frame: window.__aacFrame === true,
    path: location.pathname + location.search + location.hash,
    text: document.body.innerText.slice(0, 220),
    usableHome:
      !!document.querySelector('.home') &&
      /Claude/.test(document.querySelector('.home').innerText) &&
      !/view version/.test(document.body.innerText),
    splash: !!document.getElementById('aac-boot') || !!document.querySelector('.boot-splash'),
    names: await caches.keys(),
  }));

async function start(t, version) {
  await t.newContext({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'allow' });
  await t.goto('/__upgrade/ready');
  await t.app.signIn();
  const { builds } = await t.app.control({ uiVersion: version, oldWorker: false, delayClassicMs: 0, failPath: null });
  await t.page.route((u) => new URL(u).pathname === '/api/accounts/dashboard', (r) => r.fulfill({ json: homeAccounts() }));
  await t.page.route((u) => new URL(u).pathname === '/api/claude/desktop-profiles', (r) => r.fulfill({ json: homeProfiles() }));
  await t.page.evaluate(async () => {
    await navigator.serviceWorker.register('/sw.js');
    await navigator.serviceWorker.ready;
  });
  await t.page.waitForFunction(() => !!navigator.serviceWorker.controller);
  await t.goto('/');
  await frame(t.page);
  await t.page.waitForTimeout(6000); // historical frame warms its cache four seconds after startup
  return { origin: new URL(t.page.url()).origin, builds };
}

async function outcome(page) {
  return page.evaluate(async () => {
    const reg = await navigator.serviceWorker.getRegistration('/');
    const found = new Promise((resolve) => reg.addEventListener('updatefound', () => resolve(reg.installing), { once: true }));
    await reg.update().catch(() => {});
    const worker = await Promise.race([found, new Promise((resolve) => setTimeout(() => resolve(null), 12000))]);
    if (!worker) return 'no update';
    if (['installed', 'redundant', 'activated'].includes(worker.state)) return worker.state;
    return new Promise((resolve) => {
      worker.addEventListener('statechange', () => ['installed', 'redundant', 'activated'].includes(worker.state) && resolve(worker.state));
      setTimeout(() => resolve(worker.state), 45000);
    });
  });
}

async function offline(t, origin, address) {
  await t.context.setOffline(true);
  try {
    await t.page.goto(new URL(address, origin).href, { waitUntil: 'commit' });
    await frame(t.page, 10000).catch(() => {});
    return await state(t.page);
  } finally {
    await t.context.setOffline(false);
  }
}

async function active(t, id) {
  for (let i = 0; i < 600; i++) {
    for (const worker of t.context.serviceWorkers()) {
      try {
        const value = await worker.evaluate(async () => ({
          id: BUILD_ID,
          state: self.registration.active?.state,
          waiting: !!self.registration.waiting,
          installing: !!self.registration.installing,
          names: await caches.keys(),
        }));
        if (value.id === id && value.state === 'activated' && !value.waiting && !value.installing) return value;
      } catch {
        /* a retired worker */
      }
    }
    await t.page.waitForTimeout(100);
  }
  return null;
}

export const tests = {};
for (const [version, setting] of [
  ['previous', 'previousUi'],
  ['fix1', 'fix1Ui'],
]) {
  tests[`cross-version ${version}: failed update, navigation, close last client, offline launch`] = async (t) => {
    if (!t.args[setting]) {
      await t.check('cross-version fixture is opt in', true);
      return;
    }
    const previous = readBuild(String(t.args[setting]));
    const { origin, builds } = await start(t, version);
    await t.check('two genuinely different packaged runtimes', previous.buildId !== builds.current.buildId);
    await t.check('old app starts offline before update', (await offline(t, origin, '/')).frame);
    await t.goto('/__upgrade/ready');
    await t.app.control({ uiVersion: 'current', failPath: '/' + builds.current.wasmPath });
    await t.equal('interrupted replacement cannot install', await outcome(t.page), 'redundant');
    await t.page.goto(origin + '/?e2e#kept', { waitUntil: 'commit' });
    await frame(t.page, 10000).catch(() => {});
    await t.page
      .waitForFunction(() => document.querySelector('.home')?.innerText.includes('Claude'), null, { timeout: 10000 })
      .catch(() => {});
    const online = await state(t.page);
    await t.check('entry page recovers usable Home with matching retained controllers online', online.frame && online.usableHome, JSON.stringify(online));
    await t.equal('recovery preserves destination, query and fragment', online.path, '/app/home?e2e#kept');
    await t.equal('the recovered app is not left under the loading splash', online.splash, false);
    await t.page.goto('about:blank');
    const after = await offline(t, origin, '/?e2e#kept');
    await t.check('next offline launch starts the retained app', after.frame, JSON.stringify(after));
    await t.equal('the offline retained app is not left under the loading splash', after.splash, false);
    await t.equal('only the retained cache survives the failed install', JSON.stringify(after.names), JSON.stringify(['aac-shell-' + previous.shellBuildId]));
    await t.shot('retained-' + version, 'Offline retained app after interrupted cross-version navigation. Synthetic data only.');
    await t.noConsoleErrors('only the deliberately interrupted wasm request may fail', {
      allow: [/Failed to load resource:.*(?:status of 503|ERR_FAILED).*\/aac_web_bg\.wasm/],
    });
  };
}

tests['a fixed worker preserves its complete version across a different real build'] = async (t) => {
  if (!t.args.fix1Ui) {
    await t.check('cross-version fixture is opt in', true);
    return;
  }
  const replacement = readBuild(String(t.args.fix1Ui));
  const { origin, builds } = await start(t, 'current');
  const cacheName = 'aac-shell-' + builds.current.shellBuildId;
  const entry = await t.page.evaluate(async (name) => (await (await caches.open(name)).match('/')).text(), cacheName);
  await t.goto('/__upgrade/ready');
  await t.app.control({ uiVersion: 'fix1', failPath: '/' + replacement.wasm.path });
  await t.equal('different runtime fails installation', await outcome(t.page), 'redundant');
  await t.page.goto(origin + '/', { waitUntil: 'commit' });
  await frame(t.page, 10000).catch(() => {});
  const saved = await t.page.evaluate(async (name) => (await (await caches.open(name)).match('/')).text(), cacheName);
  await t.equal('ordinary navigation never overwrites the installed entry', saved, entry);
  await t.page.goto('about:blank');
  await t.check('fixed worker still opens its original app offline', (await offline(t, origin, '/')).frame);
  await t.noConsoleErrors('only the deliberately interrupted wasm request may fail', {
    allow: [/Failed to load resource:.*(?:status of 503|ERR_FAILED).*\/aac_web_bg\.wasm/],
  });
};

tests['two tabs keep replacement waiting; canonical trailing slash aliases work offline'] = async (t) => {
  if (!t.args.fix1Ui) {
    await t.check('cross-version fixture is opt in', true);
    return;
  }
  const { origin, builds } = await start(t, 'fix1');
  await t.goto('/__upgrade/ready');
  const second = await t.context.newPage();
  try {
    await second.goto(origin + '/__upgrade/ready');
    await t.app.control({ uiVersion: 'current', failPath: null });
    await t.equal('complete replacement waits', await outcome(t.page), 'installed');
    await t.page.goto('about:blank');
    const waiting = await second.evaluate(async () => (await navigator.serviceWorker.getRegistration('/'))?.waiting?.state);
    await t.equal('remaining tab keeps replacement waiting', waiting, 'installed');
  } finally {
    await second.close();
  }
  const next = await active(t, builds.current.shellBuildId);
  await t.check('last client closing activates replacement', !!next, JSON.stringify(next));
  await t.equal('only the complete new cache remains', JSON.stringify(next?.names), JSON.stringify(['aac-shell-' + builds.current.shellBuildId]));
  for (const tab of ['home', 'analytics', 'accounts', 'status', 'scoreboard']) {
    const address = '/app/' + tab + '/?e2e';
    await t.check('offline alias ' + address, (await offline(t, origin, address)).frame);
  }
  const unknown = await offline(t, origin, '/app/x');
  await t.check('unknown app path still refuses shell fallback', !unknown.frame && unknown.text.includes('has not been downloaded'), JSON.stringify(unknown));
  await t.noConsoleErrors('only the explicitly refused unknown route may return 503', {
    allow: [/status of 503.*\/app\/x(?:\s|$)/],
  });
};
