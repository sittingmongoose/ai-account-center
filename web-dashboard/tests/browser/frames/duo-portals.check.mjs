// Body overlays follow the owning route and current Duo geometry. Synthetic fixtures only.
import { readFileSync } from 'node:fs';
import { fixture, open as pin } from './duo-poll.check.mjs';

const read = (name) =>
  JSON.parse(readFileSync(new URL(`../../../leptos/fixtures/${name}`, import.meta.url)));
async function bridge(t, key, view) {
  // the view as the frame's dashboard controller sends it (public/frame-dashboard.mjs, an `aac-dash` event)
  await t.page.evaluate(
    ({ key, view }) =>
      window.dispatchEvent(
        new CustomEvent('aac-dash', { detail: { key, json: view ? JSON.stringify(view) : '' } })
      ),
    { key, view }
  );
}
const frames = (t) =>
  t.page.evaluate(
    () => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))
  );
/** Analytics reads its own data (analytics/feed.rs): its read is held open here and the fixture view handed to the
 * page as its controller would (an `aac-analytics` event), so the shots stay the same fixture. */
async function analytics(t, width = 669, height = 951, safe = '82,0,34,0', home = false) {
  await t.newContext({
    viewport: { width, height },
    screen: { width: 466, height: 678 },
    deviceScaleFactor: 1,
    hasTouch: true,
    serviceWorkers: 'block',
  });
  await t.page.route((u) => u.pathname === '/api/accounts/analytics', () => {});
  // its side reads (the fixture server has no account routes): synthetic answers, so nothing 404s
  const side = {
    '/api/accounts/dashboard': { schemaVersion: 1, updatedAt: new Date().toISOString(), accounts: [], settings: {} },
    '/api/accounts/preferences': { timeZone: 'UTC' },
    '/api/antigravity/auto-switch': { enabled: false },
  };
  await t.page.route((u) => u.pathname in side, (r) => r.fulfill({ json: side[new URL(r.request().url()).pathname] }));
  await t.goto(`/app/${home ? 'home' : 'analytics'}?safe=${safe}`);
  if (home)
    await t.page
      .getByRole('link', { name: 'Analytics', exact: true })
      .filter({ visible: true })
      .click();
  await t.page.waitForSelector('.ax-page', { state: 'attached' });
  await t.page.evaluate(
    (json) => dispatchEvent(new CustomEvent('aac-analytics', { detail: { key: 'analytics', json } })),
    JSON.stringify(read('analytics-view.json'))
  );
  await t.page.waitForSelector('.prov-btn');
}
async function pose(t, width, height, padding) {
  await t.page.locator('[data-safe-probe]').evaluate((el, padding) => {
    el.style.padding = padding;
  }, padding);
  await t.page.setViewportSize({ width, height });
  await frames(t);
}
// Finite running animations (the popup's 260 ms pop-in) end before a box is measured (duo-Astra-6-L2).
const settled = (t) =>
  t.page.evaluate(async () => {
    await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
    await Promise.all(
      document
        .getAnimations()
        .filter((a) => a.playState === 'running' && a.effect?.getTiming().iterations !== Infinity)
        .map((a) => a.finished.catch(() => {}))
    );
  });
async function popupState(t) {
  await settled(t);
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
    // The Details panel slides in (dt-up, then dt-in at this width): measure it settled, like Provider (L2).
    await settled(t);
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

// Use the dashboard controller's own end-of-session note to exercise actual signed-in-owner teardown.
tests['duo portals: expired-session owner unmount retires the viewer and clears its selection'] =
  async (t) => {
    await pin(t, fixture(true), true, true);
    await t.page.locator('.c2-sheet-host [data-focus-role="media-enlarge"]').first().click();
    await t.page.waitForSelector('.viewer');
    // the controller found the session revoked (its `signed-out` note): the frame's sign-in takes over
    await t.page.evaluate(() =>
      window.dispatchEvent(
        new CustomEvent('aac-dash', {
          detail: { key: 'signed-out', json: JSON.stringify({ state: 'expired', reason: 'revoked' }) },
        })
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

// ---- Max round 6 follow-ups (Astra M1 / Sol M2, Sol M1, Sol L1). Synthetic controller responses and views only.
const overlayState = (t) =>
  t.page.evaluate(() => ({
    route: location.pathname,
    roots: document.querySelectorAll('.overlay-root').length,
    locked: document.body.style.position === 'fixed',
    guards: document.querySelectorAll('[data-overlay-guard]').length,
    inside: !!document.activeElement?.closest('.pop'),
    inOverlay: !!document.activeElement?.closest('.overlay-root'),
    visible: (document.activeElement?.getBoundingClientRect().width || 0) > 0,
    body: document.activeElement === document.body,
    active: document.activeElement?.outerHTML.slice(0, 160),
  }));

// The real activation-confirmation controller in the dashboard controller's place, with synthetic activate responses:
// odd calls answer a 409 review, even calls (the confirmation) wait until the test settles them. It takes the page's
// approval actions from the frame's dashboard controller (public/frame-dashboard.mjs, whose other actions still run)
// and hands its views to the page as that controller does (`aac-dash` events).
async function approvalController(t) {
  await t.page.waitForFunction(() => window.aacFrameDashboard?.state?.().hasData === true);
  await t.page.evaluate(async () => {
    const { createActivationConfirmation } = await import('/activation-confirmation.mjs');
    window.__events = [];
    const send = (v) =>
      dispatchEvent(new CustomEvent('aac-dash', { detail: { key: 'activation', json: v ? JSON.stringify(v) : '' } }));
    let calls = 0;
    window.__controller = createActivationConfirmation({
      activate: async (target) => {
        window.__events.push({ kind: 'activate', call: ++calls });
        if (calls % 2) throw { status: 409, payload: { confirmation: { token: 'a'.repeat(43), targetProfile: target,
          expiresAt: new Date(Date.now() + 60000).toISOString(), processes: [{ label: 'Synthetic busy process', pid: 1234, role: 'fixture' }] } } };
        return new Promise((resolve, reject) => { window.__settle = { resolve, reject }; });
      },
      prompt: (v) => { window.__events.push({ kind: 'prompt', busy: v.inProgress, error: v.error }); send(v); },
      close: () => { window.__events.push({ kind: 'close' }); send(null); },
      busy: () => {}, success: () => {}, error: () => {},
    });
    window.__acts = [];
    const dashboard = window.aacFrameDashboard;
    const act = dashboard.act.bind(dashboard);
    dashboard.act = (kind, value) => {
      window.__acts.push(kind);
      if (kind === 'activation-confirm') { void window.__controller.confirm(); return true; }
      if (kind === 'activation-cancel') { window.__events.push({ kind: 'cancel', accepted: window.__controller.cancel() }); return true; }
      return act(kind, value);
    };
  });
}
const inFrame = (t, code) => t.page.evaluate(code);

tests['duo portals: a late approval result after Back cannot revive the retired modal'] = async (
  t
) => {
  await t.newContext({
    viewport: { width: 466, height: 678 },
    screen: { width: 466, height: 678 },
    deviceScaleFactor: 1,
    serviceWorkers: 'block',
  });
  await t.page.route(
    (u) => u.pathname === '/' && !u.search,
    (r) => r.fulfill({ status: 204 })
  );
  await t.goto('/app/home?safe=0,84,34,0');
  await t.page.waitForFunction(() => window.aacFrameDashboard?.state?.().hasData === true);
  await t.page.getByRole('link', { name: 'Accounts', exact: true }).filter({ visible: true }).click();
  await approvalController(t);
  await inFrame(t, "window.__controller.begin('synthetic-review-profile')");
  await t.page.waitForSelector('.adialog .btn.primary[aria-disabled="false"]');
  await t.page.locator('.adialog').evaluate((el) => {
    window.__approval = el;
  });
  await t.page.locator('.adialog .btn.primary').click();
  await t.page.waitForFunction(() =>
    document.querySelector('.adialog .a-status')?.textContent.includes('Stopping')
  );
  const busy = await overlayState(t);
  await t.check(
    'a busy update changes the same approval in place: no new portal, focus kept inside',
    busy.roots === 1 &&
      busy.inOverlay &&
      (await t.page.evaluate(() => document.querySelector('.adialog') === window.__approval)),
    JSON.stringify(busy)
  );
  await t.page.goBack();
  await t.page.waitForFunction(
    () => location.pathname === '/app/home' && !document.querySelector('.overlay-root')
  );
  await settled(t);
  const retiredState = await overlayState(t);
  await t.check(
    'Back retires the busy approval and releases its lock',
    !retiredState.roots && !retiredState.locked,
    JSON.stringify(retiredState)
  );
  // The same request now fails: the controller publishes its error and then its settled view.
  await inFrame(t, "window.__settle.reject({ status: 500, message: 'Synthetic stop timeout' })");
  await settled(t);
  await inFrame(t, 'Promise.resolve()');
  await settled(t);
  const after = await overlayState(t);
  const events = await inFrame(t, 'window.__events');
  await t.check(
    'Astra/Sol: the late failure stays retired on Home: no overlay, no lock, focus not taken',
    !after.roots && !after.locked && !after.inOverlay,
    JSON.stringify(after)
  );
  await t.check(
    'the settled approval is cancelled and closed, never while its request ran',
    events.some((e) => e.kind === 'cancel' && e.accepted) &&
      events.slice(events.findLastIndex((e) => e.kind === 'prompt')).some((e) => e.kind === 'close') &&
      !events.some((e) => e.kind === 'cancel' && !e.accepted),
    JSON.stringify(events)
  );
  await t.shot(
    'late-approval-after-back',
    'After Back while busy, the switch request fails: Home stays clear, with no approval over it.'
  );
  // A new Activate on Accounts opens a fresh approval, focused, with its own lock.
  await t.page.getByRole('link', { name: 'Accounts', exact: true }).filter({ visible: true }).click();
  await inFrame(t, "window.__controller.begin('synthetic-review-profile')");
  await t.page.waitForSelector('.adialog .btn.primary[aria-disabled="false"]');
  await frames(t);
  const fresh = await overlayState(t);
  await t.check(
    'the next Activate opens a fresh, modal approval',
    fresh.roots === 1 && fresh.locked && fresh.inOverlay,
    JSON.stringify(fresh)
  );
  await t.page.locator('.adialog .btn').first().click();
  await t.page.waitForSelector('.adialog', { state: 'detached' });
  const cancelled = await overlayState(t);
  await t.check(
    'Cancel closes it and releases the lock',
    !cancelled.roots && !cancelled.locked,
    JSON.stringify(cancelled)
  );
};

for (const route of ['Tab', 'focus'])
  tests[`duo portals: folding a nonmodal Provider with page focus (${route}) moves focus into the sheet`] =
    async (t) => {
      await analytics(t, 669, 951, '82,0,34,0', true);
      await t.page.locator('.prov-btn').click();
      await t.page.waitForSelector('.pop .pick-row');
      if (route === 'Tab') {
        // Sol's trace: ordinary forward Tab from the last option, out of the open nonmodal picker, to Date.
        await t.page.locator('.pop .pick-row').last().focus();
        for (let i = 0; i < 45; i++) {
          await t.page.keyboard.press('Tab');
          if (await t.page.evaluate(() => document.activeElement?.matches('.date-btn'))) break;
        }
      } else await t.page.locator('.date-btn').focus();
      const before = await overlayState(t);
      await t.check(
        'precondition: Provider open and nonmodal, Date focused on the page',
        before.roots === 1 &&
          !before.locked &&
          !before.inside &&
          (await t.page.evaluate(() => document.activeElement?.matches('.date-btn'))),
        JSON.stringify(before)
      );
      await pose(t, 466, 678, '0px 84px 34px 0px');
      await t.page.waitForFunction(() => document.documentElement.dataset.chrome === 'rail');
      await frames(t);
      const folded = await overlayState(t);
      await t.check(
        'Sol M1: the now-modal sheet takes the focus inside, locked and guarded',
        folded.inside && folded.visible && folded.locked && folded.guards === 2,
        JSON.stringify(folded)
      );
      if (route === 'Tab') {
        // Unfolding makes it nonmodal again: focus stays on a visible control, then the fold takes it back in.
        await pose(t, 669, 951, '82px 0px 34px 0px');
        await t.page.waitForFunction(() => document.documentElement.dataset.chrome === 'band');
        await frames(t);
        const unfolded = await overlayState(t);
        await t.check(
          'unfolding back to nonmodal leaves focus on a visible control, not stranded',
          unfolded.roots === 1 && !unfolded.locked && unfolded.guards === 0 && unfolded.visible && !unfolded.body,
          JSON.stringify(unfolded)
        );
        await pose(t, 466, 678, '0px 84px 34px 0px');
        await t.page.waitForFunction(() => document.documentElement.dataset.chrome === 'rail');
        await frames(t);
        const refolded = await overlayState(t);
        await t.check(
          'folding again keeps the focus inside the modal sheet',
          refolded.inside && refolded.locked && refolded.guards === 2,
          JSON.stringify(refolded)
        );
      }
      await t.page.keyboard.press('Enter');
      await frames(t);
      const entered = await overlayState(t);
      await t.check(
        'Enter after the fold stays in the sheet: no Custom range opens behind it',
        entered.roots <= 1,
        JSON.stringify(entered)
      );
      if (route === 'focus')
        await t.shot(
          'duo-fold-focus-inside',
          'Provider was nonmodal with Date focused; after the fold the phone sheet holds the focus.'
        );
      await t.page.keyboard.press('Escape');
      await frames(t);
      await t.page.keyboard.press('Escape');
      await frames(t);
      const closed = await overlayState(t);
      await t.check(
        'Escape closes the picker and releases its lock',
        !closed.roots && !closed.locked,
        JSON.stringify(closed)
      );
    };

tests['duo portals: an open exact-token explanation follows a live model update'] = async (t) => {
  await analytics(t, 669, 951, '82,0,34,0', true);
  await t.page.locator('.cbm-row').first().click();
  await t.page.waitForSelector('.pop .mdetail');
  await t.page.locator('.pop .md-type [data-tip]').first().tap();
  await frames(t);
  const view = read('analytics-view.json'),
    model = view.usage.cbm.rows[0];
  model.types[0].tip = '32,022,028 tokens';
  model.tipIn = model.tipIn.replaceAll('32,022,027', '32,022,028');
  model.tokTip = model.tokTip.replaceAll('333,816,956', '333,816,957');
  // One more input token: every formatted total, cost and share stays the same; only the exact count changes. The
  // view arrives as the Analytics controller sends it (an `aac-analytics` event), as in analytics() above.
  await t.page.evaluate(
    (json) => dispatchEvent(new CustomEvent('aac-analytics', { detail: { key: 'analytics', json } })),
    JSON.stringify(view)
  );
  await frames(t);
  const current = await t.page.evaluate(() => ({
    input: document.querySelector('.pop .md-type [data-tip]')?.getAttribute('data-tip'),
    tips: [...document.querySelectorAll('.overlay-root .tip-card')].map((el) => ({
      hidden: el.hidden,
      text: el.textContent,
    })),
  }));
  await t.check(
    'the live model carries the new exact count',
    current.input === '32,022,028 tokens',
    JSON.stringify(current)
  );
  await t.check(
    'Sol L1: the open explanation shows the current count or is dismissed, never the old one',
    !current.tips.some((tip) => !tip.hidden && tip.text === '32,022,027 tokens'),
    JSON.stringify(current)
  );
};

tests['duo portals: Details under an approval closes after Back and opens again on Accounts'] = async (
  t
) => {
  await t.newContext({
    viewport: { width: 466, height: 678 },
    screen: { width: 466, height: 678 },
    deviceScaleFactor: 1,
    serviceWorkers: 'block',
  });
  await t.goto('/app/home?safe=0,84,34,0');
  await t.page.waitForFunction(() => window.aacFrameDashboard?.state?.().hasData === true);
  await t.page.getByRole('link', { name: 'Accounts', exact: true }).filter({ visible: true }).click();
  await approvalController(t);
  const details = (id) => ({ id, title: `Synthetic account ${id}`, meters: [], amounts: [], facts: [] });
  await bridge(t, 'details', details('one'));
  await t.page.waitForSelector('.details');
  // A switch from the panel asks for approval over it; Details refuses to close while it waits.
  await inFrame(t, "window.__controller.begin('synthetic-review-profile')");
  await t.page.waitForSelector('.adialog .btn.primary[aria-disabled="false"]');
  await t.page.goBack();
  await t.page.waitForFunction(
    () => location.pathname === '/app/home' && !document.querySelector('.overlay-root')
  );
  await settled(t);
  await inFrame(t, 'Promise.resolve()');
  await settled(t);
  const home = await overlayState(t);
  const acts = await inFrame(t, 'window.__acts');
  await t.check(
    'Back retires both: no overlay or lock over Home',
    !home.roots && !home.locked,
    JSON.stringify(home)
  );
  await t.check(
    'once the approval is cancelled, the retired Details closes and tells the dashboard',
    acts.includes('activation-cancel') && acts.includes('details-closed'),
    JSON.stringify(acts)
  );
  // Forward to Accounts (no press, which would close a stale panel by itself): the next account's Details shows,
  // so the retired panel wasn't left open and hidden.
  await t.page.goForward();
  await t.page.waitForFunction(() => location.pathname === '/app/accounts');
  await frames(t);
  await bridge(t, 'details', details('two'));
  await frames(t);
  const shown = await t.page.evaluate(() => {
    const d = document.querySelector('.details');
    const r = d?.getBoundingClientRect();
    return { present: !!d, w: r?.width || 0, h: r?.height || 0, title: d?.textContent.includes('two') };
  });
  await t.check(
    'the next Details request on Accounts is visible',
    shown.present && shown.w > 0 && shown.h > 0 && shown.title,
    JSON.stringify(shown)
  );
};
