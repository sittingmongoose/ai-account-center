// Regressions from the installed Duo app. Synthetic design fixtures; no provider refresh or real accounts.
import { readFileSync } from 'node:fs';
const SCREEN = { width: 466, height: 678 };
async function open(t, width, height, safe, longPins = false) {
  await t.newContext({
    viewport: { width, height },
    screen: SCREEN,
    isMobile: true,
    hasTouch: true,
    deviceScaleFactor: 1,
    serviceWorkers: 'block',
  });
  if (longPins) {
    const view = JSON.parse(
      readFileSync(new URL('../../../leptos/fixtures/status-design-view.json', import.meta.url))
    );
    // A real report shape with enough content to fill a sheet. Keep the last row visible while its plate ends
    // near the top of the viewport; on the base, a transformed plate then puts the tall sheet above the viewport.
    const rich = view.pinned.updates[0];
    view.pinned.updates = view.pinned.updates.map((p) => ({ ...rich, id: p.id, title: p.title }));
    await t.page.route(
      (u) => new URL(u).pathname === '/api/status',
      (r) => r.fulfill({ json: view })
    );
  }
  await navigate(t, safe);
  await t.page.waitForSelector('.c2-rows button');
}
// The base's safe= override was immutable. For a faithful baseline, --set rawProbe=1 instead overrides only
// computed env() padding, before WASM starts, with no resize or lifecycle event. Candidate runs use safe= itself.
async function navigate(t, safe, options) {
  if (t.args.rawProbe) {
    await t.page.addInitScript(
      (padding) => {
        const install = () => {
          const s = document.createElement('style');
          s.id = 'test-insets';
          s.textContent = `body > div[style*="safe-area-inset"] { padding: ${padding} !important; }`;
          document.head.append(s);
        };
        if (document.head) install();
        else document.addEventListener('DOMContentLoaded', install, { once: true });
      },
      safe
        .split(',')
        .map((n) => `${n}px`)
        .join(' ')
    );
  }
  await t.goto(`/app/status?e2e${t.args.rawProbe ? '' : `&safe=${safe}`}`, options);
}
// The safe= override uses the same padding probe as env(). The fallback identifies it on the unmodified base.
async function safe(t, padding) {
  await t.page.evaluate((padding) => {
    const probe =
      document.querySelector('[data-safe-probe]') ||
      [...document.body.children].find((e) => e.style.padding.includes('safe-area-inset'));
    if (!probe) throw new Error('safe-area probe missing');
    const override = document.querySelector('#test-insets');
    if (override)
      override.textContent = `body > div[style*="safe-area-inset"] { padding: ${padding} !important; }`;
    else probe.style.padding = padding;
  }, padding);
}
const sheetBox = () => {
  const host = document.querySelector('.c2-sheet-host');
  const sheet = host?.querySelector('.sheet');
  const r = sheet?.getBoundingClientRect();
  return (
    r && {
      top: r.top,
      bottom: r.bottom,
      left: r.left,
      right: r.right,
      h: innerHeight,
      w: innerWidth,
      scroll: sheet.scrollTop,
      inContent: !!host.closest('main'),
      y: scrollY,
    }
  );
};
export const tests = {
  async 'duo regression: horizontal inset redistribution after all settles'(t) {
    await open(t, 678, 466, '0,84,34,0');
    await t.page.evaluate(() => dispatchEvent(new Event('orientationchange')));
    await t.page.waitForTimeout(1800);
    const before = await t.page.evaluate(() => {
      const probe =
        document.querySelector('[data-safe-probe]') ||
        [...document.body.children].find((e) => e.style.padding.includes('safe-area-inset'));
      const r = probe.getBoundingClientRect();
      return { width: r.width, height: r.height };
    });
    for (const side of ['left', 'right']) {
      await safe(t, side === 'left' ? '0px 0px 34px 84px' : '0px 84px 34px 0px');
      await t.page.waitForTimeout(250);
      const actual = await t.page.evaluate(() => {
        const root = document.documentElement,
          css = getComputedStyle(root);
        const probe =
          document.querySelector('[data-safe-probe]') ||
          [...document.body.children].find((e) => e.style.padding.includes('safe-area-inset'));
        const r = probe.getBoundingClientRect(),
          p = getComputedStyle(probe),
          c = document.querySelector('main.content').getBoundingClientRect();
        return {
          side: root.dataset.railSide,
          left: css.getPropertyValue('--safe-left'),
          right: css.getPropertyValue('--safe-right'),
          probeLeft: p.paddingLeft,
          probeRight: p.paddingRight,
          width: r.width,
          height: r.height,
          contentLeft: c.left,
          contentRight: c.right,
          controls: [...document.querySelectorAll('.content button')]
            .filter((e) => e.getBoundingClientRect().width)
            .map((e) => {
              const b = e.getBoundingClientRect();
              return [b.left, b.right];
            }),
        };
      });
      await t.check(
        `${side}: witness keeps the original combined probe dimensions`,
        actual.width === before.width && actual.height === before.height,
        JSON.stringify(actual)
      );
      await t.equal(
        `${side}: rail follows the new edge without a viewport event`,
        actual.side,
        side
      );
      await t.check(
        `${side}: safe properties follow individual edges`,
        actual.left === (side === 'left' ? '84px' : '0px') &&
          actual.right === (side === 'right' ? '84px' : '0px'),
        JSON.stringify(actual)
      );
      await t.check(
        `${side}: content clears the camera-side strip`,
        Math.abs(actual.contentLeft - (side === 'left' ? 84 : 0)) <= 1 &&
          Math.abs(actual.contentRight - (side === 'right' ? 594 : 678)) <= 1 &&
          actual.controls.every(
            ([l, r]) => l >= (side === 'left' ? 84 : 0) && r <= (side === 'right' ? 594 : 678)
          ),
        JSON.stringify(actual)
      );
      if (side === 'left')
        await t.shot(
          'duo-inset-side-swap',
          'The late opposite-edge inset moves the rail and content clear of the left camera strip without a resize.'
        );
    }
  },
  async 'duo regression: vertical inset redistribution after all settles'(t) {
    await open(t, 402, 874, '62,0,34,0');
    await t.page.waitForTimeout(1800);
    await safe(t, '34px 0px 62px 0px');
    await t.page.waitForTimeout(250);
    const actual = await t.page.evaluate(() => {
      const s = getComputedStyle(document.documentElement);
      return [s.getPropertyValue('--safe-top'), s.getPropertyValue('--safe-bottom')];
    });
    await t.check(
      'top and bottom update even when their sum stays 96px',
      actual[0] === '34px' && actual[1] === '62px',
      JSON.stringify(actual)
    );
  },
  async 'duo regression: late portrait insets without resize'(t) {
    await open(t, 669, 869, '0,0,34,0');
    await t.equal(
      'initial zero top gets phone chrome',
      await t.page.getAttribute('html', 'data-chrome'),
      'phone'
    );
    // Beyond the old 350 ms settle window, with no resize/orientation event.
    await t.page.waitForTimeout(1700);
    await safe(t, '82px 0px 34px 0px');
    await t.page.waitForTimeout(250);
    await t.equal(
      'late env padding switches to band',
      await t.page.getAttribute('html', 'data-chrome'),
      'band'
    );
    await t.shot(
      'duo-late-portrait',
      'Portrait after the 82px inset arrives: page/status line belongs in the top band; tabs directly below.'
    );
  },
  async 'duo regression: landscape reserve survives zero insets'(t) {
    await open(t, 951, 669, '0,0,0,0');
    const check = async (label) => {
      const p = await t.page.evaluate(() => ({
        reserve: parseFloat(
          getComputedStyle(document.documentElement).getPropertyValue('--rail-reserve')
        ),
        controls: [...document.querySelectorAll('.rail a, .rail button')]
          .filter((e) => e.getBoundingClientRect().height)
          .map((e) => e.getBoundingClientRect().top),
      }));
      await t.check(
        `${label}: rail clears the measured 116px cluster plus margin`,
        p.reserve >= 136 && p.controls.every((y) => y >= 136),
        JSON.stringify(p)
      );
    };
    await check('startup zero');
    await safe(t, '0px 84px 34px 0px');
    await t.page.waitForTimeout(100);
    await check('strip arrives');
    await safe(t, '0px');
    // A real viewport event while env temporarily drops to zero must also retain clearance.
    await t.page.evaluate(() => dispatchEvent(new Event('resize')));
    await t.page.waitForTimeout(100);
    await check('strip briefly missing');
    await t.shot(
      'duo-landscape-reserve',
      'Landscape with zero reported inset: rail controls stay below the reserved clock/signal area.'
    );
  },
  ...Object.fromEntries(
    ['container', 'transform', 'animation'].map((mode) => [
      `duo regression: scrolled pin sheet ${mode}`,
      async (t) => {
        await open(t, 669, 869, '82,0,34,0', true);
        await t.page.evaluate((mode) => {
          const p = document.querySelector('.sec-pinned');
          p.style.marginTop = '1800px';
          if (mode === 'transform') p.style.transform = 'translateY(3px)';
          if (mode === 'animation') {
            const s = document.createElement('style');
            s.textContent =
              '@keyframes duo-test { from { transform:translateY(2px) } to { transform:translateY(4px) } } .sec-pinned { animation:duo-test 10s linear infinite alternate !important }';
            document.head.append(s);
          }
          [...document.querySelectorAll('.c2-rows button')]
            .at(-1)
            .scrollIntoView({ block: 'start' });
        }, mode);
        const trigger = t.page.locator('.c2-rows button').last();
        await trigger.focus();
        const y = await t.page.evaluate(() => scrollY);
        await t.check('pin opens far down the page', y > 1000, String(y));
        // Record scroll at activation: Enter can finish scrolling a focused, animated row before its click.
        await t.page.evaluate(() =>
          document.addEventListener(
            'click',
            () => {
              window.__duoOpenScroll = scrollY;
            },
            { capture: true, once: true }
          )
        );
        // Keyboard activation remains deterministic while its ancestor moves; it is a real UI event.
        await t.page.keyboard.press('Enter');
        await t.page.waitForSelector('.c2-sheet-host');
        await t.page.waitForTimeout(100);
        const b = await t.page.evaluate(sheetBox);
        await t.check(
          'entire sheet is inside viewport',
          b && b.top >= 82 && b.bottom <= b.h + 1 && b.left >= 0 && b.right <= b.w,
          JSON.stringify(b)
        );
        await t.check(
          'sheet is outside the content containing block',
          b && !b.inContent,
          JSON.stringify(b)
        );
        await t.equal('sheet content starts at top', b?.scroll, 0);
        await t.shot(
          `duo-sheet-${mode}`,
          `Pinned thread after scrolling >1000px, ${mode} ancestor: close/grabber and sheet top stay within the viewport.`
        );
        await t.page.mouse.wheel(0, 600);
        const locked = await t.page.evaluate(
          () =>
            document.body.style.position === 'fixed' &&
            document.documentElement.style.overflow === 'hidden'
        );
        await t.check('background scroll is locked', locked);
        // Use DOM activation on the base too: an offscreen X must still close, as in the owner report.
        await t.page.locator('.sheet-close').evaluate((e) => e.click());
        await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
        const restored = await t.page.evaluate(() => scrollY);
        const openedAt = await t.page.evaluate(() => window.__duoOpenScroll);
        await t.check(
          'closing restores original scroll',
          Math.abs(restored - openedAt) <= 2,
          `${openedAt} -> ${restored}`
        );
        await t.focused(trigger);
      },
    ])
  ),
};

// Disable only the inset probe's observer to exercise the launch/lifecycle fallback separately.
async function noInsetObserver(t) {
  await t.page.addInitScript(() => {
    const Native = window.ResizeObserver;
    window.ResizeObserver = class extends Native {
      observe(el, options) {
        if (
          el.hasAttribute('data-safe-probe') ||
          el.hasAttribute('data-safe-edge') ||
          el.style.padding.includes('safe-area-inset')
        )
          return;
        super.observe(el, options);
      }
    };
  });
}
// Like their neighbours, these block service workers: the fixture server has no /sw.js (duo-Astra-6-L1).
tests['duo regression: startup settle catches delayed insets'] = async (t) => {
  await t.newContext({
    viewport: { width: 669, height: 869 },
    screen: SCREEN,
    isMobile: true,
    hasTouch: true,
    deviceScaleFactor: 1,
    serviceWorkers: 'block',
  });
  await noInsetObserver(t);
  await navigate(t, '0,0,34,0', { settleMs: 0 });
  await t.page.waitForSelector('.c2-row');
  await safe(t, '82px 0px 34px 0px');
  await t.page.waitForTimeout(1700);
  await t.equal(
    'startup settle recovers without ResizeObserver or resize',
    await t.page.getAttribute('html', 'data-chrome'),
    'band'
  );
};
for (const event of ['pageshow', 'focus', 'visibilitychange']) {
  tests[`duo regression: ${event} rereads settled insets`] = async (t) => {
    await t.newContext({
      viewport: { width: 669, height: 869 },
      screen: SCREEN,
      isMobile: true,
      hasTouch: true,
      deviceScaleFactor: 1,
      serviceWorkers: 'block',
    });
    await noInsetObserver(t);
    await navigate(t, '0,0,34,0');
    await t.page.waitForSelector('.c2-row');
    await t.page.waitForTimeout(1700);
    await safe(t, '82px 0px 34px 0px');
    await t.page.evaluate(
      (event) => (event === 'visibilitychange' ? document : window).dispatchEvent(new Event(event)),
      event
    );
    await t.page.waitForTimeout(100);
    await t.equal(
      `${event} recovers the portrait band`,
      await t.page.getAttribute('html', 'data-chrome'),
      'band'
    );
  };
}

// Interaction checks after viewport placement: close methods, scroll restoration, focus and nested scroll locks.
tests['duo interactions: pin Escape scrim fling and reopen'] = async (t) => {
  await open(t, 669, 869, '82,0,34,0');
  const trigger = t.page.locator('.c2-row').first();
  await trigger.scrollIntoViewIfNeeded();
  await trigger.click();
  await t.page.waitForSelector('.sheet-close');
  await t.focused('.sheet-close');
  await t.page.keyboard.press('Tab');
  await t.check(
    'Tab stays in the overlay',
    await t.page.evaluate(() => !!document.activeElement.closest('.c2-sheet-host'))
  );
  await t.page.keyboard.press('Escape');
  await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
  await t.focused(trigger);
  await trigger.click();
  await t.page.waitForSelector('.c2-sheet-host');
  await t.page.locator('.sheet-scrim').click({ position: { x: 10, y: 85 } });
  await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
  await trigger.click();
  await t.page.waitForSelector('.c2-sheet-host .grabber-zone');
  const g = await t.page.locator('.c2-sheet-host .grabber-zone').boundingBox();
  await t.page.mouse.move(g.x + g.width / 2, g.y + g.height / 2);
  await t.page.mouse.down();
  await t.page.mouse.move(g.x + g.width / 2, g.y + g.height / 2 + 110, { steps: 5 });
  await t.page.mouse.up();
  await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
  await trigger.click();
  await t.page.waitForSelector('.c2-sheet-host');
  await t.check(
    'reopening resets the drag offset and sheet scroll',
    await t.page.evaluate(() => {
      const s = document.querySelector('.c2-sheet-host .sheet');
      return s.scrollTop === 0 && (!s.style.transform || s.style.transform.includes('(0'));
    })
  );
  await t.page.keyboard.press('Escape');
  await t.check(
    'last close releases scroll lock',
    await t.page.evaluate(
      () => !document.body.style.position && !document.documentElement.style.overflow
    )
  );
};
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
tests['duo interactions: Details and approval use body with nested locks'] = async (t) => {
  await open(t, 951, 669, '0,84,34,0');
  // The frame loads its bridge on the first account-backed tab, then retains it when returning to Status.
  await t.page.locator('.rail a[href="/app/home"]').click();
  await t.page.waitForSelector('iframe.slint-frame', { state: 'attached' });
  await t.page.locator('.rail a[href="/app/status"]').click();
  await t.page.waitForSelector('.c2-row');
  await t.page.evaluate(() => {
    document.querySelector('main.content').style.transform = 'translateY(3px)';
    document.querySelector('.sec-pinned').style.marginTop = '1800px';
    scrollTo(0, 1500);
  });
  const y = await t.page.evaluate(() => scrollY);
  await bridge(t, 'details', {
    id: 'fixture',
    title: 'fixture@example.invalid',
    meters: [],
    amounts: [],
    facts: [],
  });
  await t.page.waitForSelector('.details');
  await t.settle();
  await t.check(
    'Details lives outside the shell',
    await t.page.evaluate(() => !document.querySelector('.details').closest('.shell'))
  );
  await t.check(
    'Details clears the right rail',
    await t.page.evaluate(
      () => document.querySelector('.details').getBoundingClientRect().right <= innerWidth - 84 + 1
    )
  );
  await t.focused('[data-probe="details-close"]');
  await bridge(t, 'activation', {
    product: 'Codex',
    targetProfile: 'fixture',
    canConfirm: false,
    processes: [],
  });
  await t.page.waitForSelector('.adialog');
  await t.check(
    'approval lives outside the shell',
    await t.page.evaluate(() => !document.querySelector('.adialog').closest('.shell'))
  );
  await bridge(t, 'activation', null);
  await t.page.waitForSelector('.adialog', { state: 'detached' });
  await t.check(
    'closing approval retains Details scroll lock',
    await t.page.evaluate(() => document.body.style.position === 'fixed')
  );
  await t.page.keyboard.press('Escape');
  await t.page.waitForSelector('.details', { state: 'detached' });
  await t.check(
    'final close restores scroll and inline styles',
    await t.page.evaluate(
      (y) =>
        Math.abs(scrollY - y) <= 2 &&
        !document.body.style.position &&
        !document.documentElement.style.overflow,
      y
    )
  );
};
tests['duo diagnostics: decisions identify the inset trigger'] = async (t) => {
  await open(t, 669, 869, '0,0,34,0');
  await t.page.waitForTimeout(1700);
  await safe(t, '82px 0px 34px 0px');
  await t.page.waitForTimeout(100);
  await t.page.locator('button[aria-label="Account"]:visible').click();
  await t.expectDialog(() => t.page.getByRole('menuitem', { name: /Device info/ }).click(), {
    accept: true,
    text: /reserve 0 \[insets\]/,
  });
};
