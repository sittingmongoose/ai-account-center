// Body overlays follow the owning route and current Duo geometry. Synthetic fixtures only.
import { readFileSync } from 'node:fs';
import { fixture, open as pin } from './duo-poll.check.mjs';

const read = (name) =>
  JSON.parse(readFileSync(new URL(`../../../leptos/fixtures/${name}`, import.meta.url)));
async function bridge(t, key, view) {
  await t.page.locator('iframe.slint-frame').evaluate(
    (el, { key, view }) => {
      el.contentWindow.eval(
        `parent.postMessage(${JSON.stringify({ type: 'aac-content', kind: 'view', key, json: view ? JSON.stringify(view) : '' })}, location.origin)`
      );
    },
    { key, view }
  );
}
const frames = (t) =>
  t.page.evaluate(
    () => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))
  );
async function analytics(t, width = 669, height = 951, safe = '82,0,34,0', home = false) {
  await t.newContext({
    viewport: { width, height },
    screen: { width: 466, height: 678 },
    deviceScaleFactor: 1,
    hasTouch: true,
    serviceWorkers: 'block',
  });
  await t.goto(`/app/${home ? 'home' : 'analytics'}?safe=${safe}`);
  if (home)
    await t.page
      .getByRole('link', { name: 'Analytics', exact: true })
      .filter({ visible: true })
      .click();
  await t.page.waitForSelector('iframe.slint-frame', { state: 'attached' });
  await bridge(t, 'analytics', read('analytics-view.json'));
  await t.page.waitForSelector('.prov-btn');
}
async function pose(t, width, height, padding) {
  await t.page.locator('[data-safe-probe]').evaluate((el, padding) => {
    el.style.padding = padding;
  }, padding);
  await t.page.setViewportSize({ width, height });
  await frames(t);
}
async function popupState(t) {
  return t.page.evaluate(() => {
    const pop = document.querySelector('.pop');
    const r = pop?.getBoundingClientRect(),
      root = document.documentElement,
      css = getComputedStyle(root);
    const rail = parseFloat(css.getPropertyValue('--rail-w')) || 0;
    return {
      x: r?.left,
      right: r?.right,
      top: r?.top,
      bottom: r?.bottom,
      width: innerWidth,
      height: innerHeight,
      lo: root.dataset.railSide === 'left' ? rail : 0,
      hi: innerWidth - (root.dataset.railSide === 'right' ? rail : 0),
      locked: document.body.style.position === 'fixed',
      inside: !!document.activeElement?.closest('.pop'),
      guards: pop?.closest('.overlay-root').querySelectorAll('[data-overlay-guard]').length,
      documentFocused: document.hasFocus(),
      active: document.activeElement?.outerHTML.slice(0, 180),
    };
  });
}
async function retired(t, label) {
  await t.page.goBack();
  await t.page.waitForFunction(() => location.pathname === '/app/home');
  await frames(t);
  const state = await t.page.evaluate(() => ({
    roots: document.querySelectorAll('.overlay-root').length,
    locked: document.body.style.position === 'fixed',
    visibleFocus: document.activeElement?.getBoundingClientRect().width > 0,
    nav: document.activeElement?.matches('a[aria-current="page"]'),
  }));
  await t.check(
    `${label}: Back retires all old-route portals and locks`,
    !state.roots && !state.locked,
    JSON.stringify(state)
  );
  await t.page.keyboard.press('Tab');
  await t.check(
    `${label}: Tab cannot return to hidden overlay`,
    await t.page.evaluate(() => !document.activeElement?.closest('.overlay-root'))
  );
  if (label === 'Provider')
    await t.shot(
      'duo-popover-back',
      'Back returns to Home with no hidden Analytics picker, retained focus trap or scroll lock.'
    );
  await t.page.goForward();
  await frames(t);
  await t.check(
    `${label}: Forward does not resurrect the old opening`,
    (await t.page.locator('.overlay-root').count()) === 0
  );
}

export const tests = {
  'duo portals: open Provider follows fold, unfold and late opposite-edge insets': async (t) => {
    await analytics(t);
    await t.page.locator('.prov-btn').click();
    await t.page.waitForSelector('.pop .pick-row');
    await t.page.locator('.pop').evaluate((el) => {
      window.__posePopover = el;
    });
    await pose(t, 466, 678, '0px 84px 34px 0px');
    await t.page.waitForFunction(() => document.documentElement.dataset.chrome === 'rail');
    await t.page.locator('.pop .pick-row').last().focus();
    await t.page.keyboard.press('Tab');
    let state = await popupState(t);
    await t.check(
      'Sol M1: folding keeps the whole picker beside the rail',
      state.x >= state.lo &&
        state.right <= state.hi + 1 &&
        state.top >= 0 &&
        state.bottom <= state.height + 1,
      JSON.stringify(state)
    );
    await t.check(
      'Sol M1: mobile Tab wraps and background locks',
      state.inside && state.locked && state.guards === 2 && state.documentFocused,
      JSON.stringify(state)
    );
    await t.shot(
      'duo-popover-fold',
      'Provider stays visible and modal after unfolding-to-folded transition; its right edge clears the rail.'
    );
    await pose(t, 669, 951, '82px 0px 34px 0px');
    await t.page.waitForFunction(() => document.documentElement.dataset.chrome === 'band');
    state = await popupState(t);
    await t.check(
      'unfold restores anchored layout and releases phone-only modality',
      !state.locked &&
        state.guards === 0 &&
        state.x >= 0 &&
        state.right <= 669 &&
        state.bottom <= 951,
      JSON.stringify(state)
    );
    await t.check(
      'geometry changes update in place without resetting the popup',
      await t.page.locator('.pop').evaluate((el) => el === window.__posePopover)
    );
    await pose(t, 678, 466, '0px 84px 34px 0px');
    await t.page.waitForTimeout(1750);
    await t.page.locator('[data-safe-probe]').evaluate((el) => {
      el.style.padding = '0px 0px 34px 84px';
    });
    await t.page.waitForFunction(() => document.documentElement.dataset.railSide === 'left');
    await frames(t);
    state = await popupState(t);
    await t.check(
      'late inset-only swap re-clamps the anchored popup',
      state.x >= state.lo && state.right <= state.hi + 1,
      JSON.stringify(state)
    );
    await t.page.keyboard.press('Escape');
    await t.check(
      'closing leaves no lock or focus guards',
      await t.page.evaluate(
        () => !document.querySelector('.overlay-root') && document.body.style.position !== 'fixed'
      )
    );
  },
  'duo portals: all Analytics popovers retire on Back and stay closed on Forward': async (t) => {
    for (const [selector, label] of [
      ['.prov-btn', 'Provider'],
      ['.date-btn', 'Custom range'],
      ['.cbm-row', 'Model detail'],
    ]) {
      await analytics(t, 466, 678, '0,84,34,0', true);
      await t.page.locator(selector).first().click();
      await t.page.waitForSelector('.pop');
      await t.check(
        `${label}: folded opening owns the modal lock`,
        await t.page.evaluate(() => document.body.style.position === 'fixed')
      );
      await retired(t, label);
    }
  },
  'duo portals: nonmodal keyboard route change closes the owning popover': async (t) => {
    await analytics(t, 1440, 1000, '0,0,0,0');
    await t.page.locator('.prov-btn').click();
    await t.page.waitForSelector('.pop');
    // Wide popovers allow ordinary page focus; keyboard activation uses the actual navigation link.
    await t.page.getByRole('link', { name: 'Home', exact: true }).filter({ visible: true }).focus();
    await t.page.keyboard.press('Enter');
    await t.page.waitForFunction(() => location.pathname === '/app/home');
    await frames(t);
    await t.check(
      'keyboard route change retires its body popup',
      (await t.page.locator('.pop').count()) === 0
    );
    await t.check(
      'keyboard destination retains visible focus and no lock',
      await t.page.evaluate(
        () =>
          document.activeElement?.matches('a[aria-current="page"]') &&
          document.body.style.position !== 'fixed'
      )
    );
  },
  'duo portals: exact model tokens have an overlay-local hover and touch surface': async (t) => {
    await analytics(t, 1440, 1000, '0,0,0,0');
    const tokens = t.page.locator('.cbm-row .cbm-tok').first();
    await tokens.hover();
    await t.page.waitForSelector('.tip-card:not([hidden])');
    const expected = read('analytics-view.json').usage.cbm.rows[0].tokTip;
    await t.check(
      'matched parent-row tooltip remains readable',
      (await t.page.locator('.tip-card:not([hidden])').textContent()) === expected
    );
    await t.page.locator('.cbm-row').first().click();
    await t.page.waitForSelector('.pop .mdetail');
    const exact = t.page.locator('.pop .md-type [data-tip]').first();
    await exact.hover();
    await frames(t);
    let tip = await exact.getAttribute('data-tip');
    await t.equal('the witness uses the exact input count', tip, '32,022,027 tokens');
    const visibleTip = () =>
      t.page.evaluate(
        (tip) =>
          [...document.querySelectorAll('.overlay-root .tip-card')].some(
            (e) => !e.hidden && e.textContent === tip && e.getBoundingClientRect().width > 0
          ),
        tip
      );
    await t.check(
      'Astra L2: hover opens the exact count above the portaled detail',
      await visibleTip()
    );
    await t.shot(
      'duo-model-token-tip',
      'Exact Input tokens remain readable inside the portaled ModelDetail tooltip scope.'
    );
    await t.page.mouse.move(0, 0);
    await exact.tap();
    await frames(t);
    await t.check('touch also opens the overlay-local token explanation', await visibleTip());
    await t.page.keyboard.press('Escape');
    await t.check(
      'closing removes the overlay and its tooltip surface',
      (await t.page.locator('.overlay-root').count()) === 0
    );
  },
};

for (const kind of ['pin', 'viewer', 'details', 'activation']) {
  tests[`duo portals: ${kind} reflows on pose change and retires on route departure`] = async (
    t
  ) => {
    await pin(t, fixture(true), true, true);
    if (kind === 'viewer')
      await t.page.locator('.c2-sheet-host [data-focus-role="media-enlarge"]').first().click();
    if (kind === 'details' || kind === 'activation')
      await bridge(
        t,
        kind,
        kind === 'details'
          ? { id: 'fixture', title: 'Synthetic account', meters: [], amounts: [], facts: [] }
          : { product: 'Codex', targetProfile: 'fixture', canConfirm: false, processes: [] }
      );
    const selector = {
      pin: '.c2-sheet-host .sheet',
      viewer: '.viewer',
      details: '.details',
      activation: '.adialog',
    }[kind];
    await t.page.waitForSelector(selector);
    await pose(t, 669, 951, '82px 0px 34px 0px');
    await t.page.waitForFunction(() => document.documentElement.dataset.chrome === 'band');
    await t.check(
      `${kind}: responsive surface stays inside the viewport`,
      await t.page.locator(selector).evaluate((el) => {
        const r = el.getBoundingClientRect();
        return (
          r.left >= -1 && r.right <= innerWidth + 1 && r.top >= 0 && r.bottom <= innerHeight + 1
        );
      })
    );
    await t.check(
      `${kind}: responsive surface keeps its lock`,
      await t.page.evaluate(() => document.body.style.position === 'fixed')
    );
    await retired(t, kind);
  };
}

tests['duo portals: Scoreboard model sheet reflows and retires on Back'] = async (t) => {
  await analytics(t, 466, 678, '0,84,34,0');
  await t.page.getByRole('link', { name: 'Home', exact: true }).filter({ visible: true }).click();
  await t.page
    .getByRole('link', { name: 'Scoreboard', exact: true })
    .filter({ visible: true })
    .click();
  await t.page.waitForSelector('.sbm');
  await t.page.locator('.sbm').first().click();
  await t.page.waitForSelector('.sheet-host .sheet');
  await pose(t, 951, 669, '0px 84px 34px 0px');
  await frames(t);
  await t.check(
    'model sheet reflows beside the current landscape rail',
    await t.page.locator('.sheet-host .sheet').evaluate((el) => {
      const r = el.getBoundingClientRect();
      return r.left >= 0 && r.right <= innerWidth - 84 + 1 && r.top >= 0 && r.bottom <= innerHeight;
    })
  );
  await retired(t, 'Scoreboard');
};

// Embedded documents have no FrameCtx; they share the same logical owner retirement rule.
tests['duo portals: embedded shell hides retire pin and global viewer owners'] = async (t) => {
  const view = fixture(true);
  await t.newContext({
    viewport: { width: 466, height: 678 },
    screen: { width: 466, height: 678 },
    serviceWorkers: 'block',
  });
  await t.page.route(
    (u) => u.pathname === '/api/status',
    (r) => r.fulfill({ json: view })
  );
  await t.page.route(
    (u) => u.pathname === '/api/status/deliverable',
    (r) =>
      r.continue({
        url: new URL('/api/status/files/keyboard-controls.mp4', r.request().url()).href,
      })
  );
  await t.goto('/app/status?embed=1');
  await t.page.locator('.c2-row').first().click();
  await t.page.waitForSelector('.c2-sheet-host');
  await t.page.locator('.c2-sheet-host [data-focus-role="media-enlarge"]').first().click();
  await t.page.waitForSelector('.viewer');
  await t.check(
    'embedded nested overlays own their locks before hiding',
    await t.page.evaluate(
      () =>
        document.querySelectorAll('.overlay-root').length === 2 &&
        document.body.style.position === 'fixed'
    )
  );
  await t.page.evaluate(() =>
    postMessage({ type: 'aac-visible', visible: false }, location.origin)
  );
  await frames(t);
  await t.check(
    'logical shell hide retires every embedded body portal',
    await t.page.evaluate(
      () => !document.querySelector('.overlay-root') && document.body.style.position !== 'fixed'
    )
  );
  await t.page.evaluate(() => postMessage({ type: 'aac-visible', visible: true }, location.origin));
  await frames(t);
  await t.check(
    'showing the embedded owner again cannot revive its old overlays',
    (await t.page.locator('.overlay-root').count()) === 0
  );
};

// Use the dashboard's authenticated source/origin seam to exercise actual signed-in-owner teardown.
tests['duo portals: expired-session owner unmount retires the viewer and clears its selection'] =
  async (t) => {
    await pin(t, fixture(true), true, true);
    await t.page.locator('.c2-sheet-host [data-focus-role="media-enlarge"]').first().click();
    await t.page.waitForSelector('.viewer');
    await t.page
      .locator('iframe.slint-frame')
      .evaluate((el) =>
        el.contentWindow.eval(
          `parent.postMessage({type:'aac-content',kind:'auth',signedIn:false,state:'expired',reason:'revoked'},location.origin)`
        )
      );
    await t.page.waitForSelector('.shell', { state: 'detached' });
    await frames(t);
    await t.check(
      'expired-session message unmounts the signed-in page tree',
      (await t.page.locator('.shell').count()) === 0
    );
    await t.check(
      'page owner unmount removes the viewer and restores scrolling',
      await t.page.evaluate(
        () => !document.querySelector('.overlay-root') && document.body.style.position !== 'fixed'
      )
    );
    // The fixture auth endpoint remains synthetic authenticated=true: a normal controller recheck starts a fresh owner.
    await t.page.evaluate(() => window.aacFrameSession.recheck());
    await t.page.waitForSelector('.shell');
    await frames(t);
    await t.check(
      'a fresh signed-in owner cannot reopen the previous viewer selection',
      (await t.page.locator('.overlay-root').count()) === 0
    );
  };
