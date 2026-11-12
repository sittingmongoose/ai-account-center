// An ordinary status poll must update a report without reopening its sheet or changing modal ownership.
import { readFileSync } from 'node:fs';

const ROOT = new URL('../../../leptos/', import.meta.url);
function fixture(media = false) {
  const view = JSON.parse(readFileSync(new URL('fixtures/status-design-view.json', ROOT)));
  view.refresh.pollSeconds = 10;
  if (media) {
    view.pinned.updates[0].reported = {
      now: 'Viewing synthetic evidence.',
      deliverables: [
        {
          id: 'poll-evidence',
          name: 'Synthetic evidence',
          kind: 'concept',
          state: 'ready',
          for: 'owner',
          version: 'v1',
          media: [
            {
              kind: 'video',
              path: '/mnt/Cursor/share/fixture/keyboard-controls.mp4',
              caption: 'Polling fixture clip',
            },
          ],
        },
      ],
    };
  }
  return view;
}

async function open(t, view, folded = false, bridge = false) {
  await t.newContext({
    viewport: folded ? { width: 466, height: 678 } : { width: 669, height: 951 },
    screen: { width: 466, height: 678 },
    deviceScaleFactor: 1,
    serviceWorkers: 'block',
  });
  await t.page.route(
    (u) => u.pathname === '/api/status',
    (r) => r.fulfill({ json: view })
  );
  await t.page.route(
    (u) => u.pathname === '/api/status/deliverable',
    // Preserve real HTTP Range responses so seeking is a meaningful playback-state witness.
    (r) =>
      r.continue({
        url: new URL('/api/status/files/keyboard-controls.mp4', r.request().url()).href,
      })
  );
  await t.goto(folded ? '/app/status?safe=0,84,34,0' : '/app/status?safe=82,0,34,0');
  await t.page.waitForSelector('.c2-row');
  if (bridge) {
    await t.page.locator('a[href="/app/home"]:visible').click();
    await t.page.waitForSelector('iframe.slint-frame', { state: 'attached' });
    await t.page.locator('a[href="/app/status"]:visible').click();
    await t.page.waitForSelector('.c2-row');
  }
  await t.page.locator('.c2-row').first().click();
  await t.page.waitForSelector('.c2-sheet-host');
}

async function bridge(t, key, view) {
  await t.page.locator('iframe.slint-frame').evaluate(
    (el, { key, view }) => {
      el.contentWindow.eval(
        `parent.postMessage(${JSON.stringify({
          type: 'aac-content',
          kind: 'view',
          key,
          json: view ? JSON.stringify(view) : '',
        })}, location.origin)`
      );
    },
    { key, view }
  );
}

async function remember(t) {
  await t.page.evaluate(() => {
    window.__pollSheet = document.querySelector('.c2-sheet-host .sheet');
    window.__pollHost = document.querySelector('.c2-sheet-host');
    window.__pollFocus = document.activeElement;
    window.__pollFocusEvents = [];
    document.addEventListener('focusin', (e) =>
      window.__pollFocusEvents.push(e.target.outerHTML.slice(0, 150))
    );
  });
}

async function update(t, view, text) {
  const pin = view.pinned.updates[0];
  if (pin.reported) pin.reported.now = text;
  else pin.now = text;
  // Wait for the real ten-second safety poll. No synthetic resize, reload, hint or product signal mutation.
  await t.page.waitForFunction(
    (text) => document.querySelector('.c2-sheet-host')?.textContent.includes(text),
    text
  );
}

async function state(t) {
  return t.page.evaluate(() => ({
    sameSheet: window.__pollSheet === document.querySelector('.c2-sheet-host .sheet'),
    sameHost: window.__pollHost === document.querySelector('.c2-sheet-host'),
    sameFocus: window.__pollFocus === document.activeElement,
    focusEvents: window.__pollFocusEvents,
    focus: document.activeElement?.outerHTML.slice(0, 200),
    scroll: document.querySelector('.c2-sheet-host .sheet')?.scrollTop,
    sameVideo: window.__pollVideo === document.querySelector('.c2-sheet-host video'),
    videoTime: document.querySelector('.c2-sheet-host video')?.currentTime,
    videoSeeking: document.querySelector('.c2-sheet-host video')?.seeking,
    videoDuration: document.querySelector('.c2-sheet-host video')?.duration,
    viewer: !!document.querySelector('.viewer'),
    pin: !!document.querySelector('.c2-sheet-host'),
    locked: document.body.style.position === 'fixed',
  }));
}

export const tests = {
  async 'duo polling: expanded tasks retain their state and focused control'(t) {
    const view = fixture();
    for (const item of view.pinned.updates[0].left) item.state = 'working';
    for (const item of view.pinned.updates[0].done) item.state = 'done';
    await open(t, view, true);
    await t.page.locator('.c2-sheet-host .c2-tg-fold button').first().click();
    const control = t.page.locator('.c2-sheet-host .c2-tg-less button').first();
    await control.focus();
    await remember(t);
    const count = await t.page.locator('.c2-sheet-host .c2-tg-rows li').count();
    view.pinned.updates[0].left[0].note = 'Task wording updated by the poll';
    view.pinned.updates[0].updated.at = '2026-11-12T05:00:00.000Z';
    await update(t, view, 'A live report update while expanded tasks are being read.');
    const after = await state(t);
    await t.check(
      'expanded task rows survive a report poll',
      (await t.page.locator('.c2-sheet-host .c2-tg-rows li').count()) === count,
      JSON.stringify(after)
    );
    await t.check(
      'expanded task content updates in place',
      await t.page.evaluate(() =>
        [...document.querySelectorAll('.c2-sheet-host .c2-tg-rows')].some((el) =>
          el.textContent.includes('Task wording updated by the poll')
        )
      )
    );
    await t.check(
      'expanded task control retains keyboard focus',
      after.sameFocus && !after.focusEvents.length,
      JSON.stringify(after)
    );
  },
  async 'duo polling: same pin keeps its sheet and reading position'(t) {
    const view = fixture();
    await open(t, view, true);
    await t.page.locator('.sheet-close').focus();
    await t.page.locator('.c2-sheet-host .sheet').evaluate((e) => {
      e.scrollTop = 250;
    });
    const before = await state(t);
    await t.check(
      'witness has genuine sheet overflow',
      before.scroll >= 100,
      JSON.stringify(before)
    );
    await remember(t);
    await update(t, view, 'A new status update while this sheet stays open.');
    const after = await state(t);
    await t.check(
      'poll retains sheet and host DOM identity',
      after.sameSheet && after.sameHost,
      JSON.stringify(after)
    );
    await t.check(
      'poll preserves internal reading position',
      Math.abs(after.scroll - before.scroll) <= 2,
      JSON.stringify({ before, after })
    );
    await t.check(
      'poll never refocuses the open sheet',
      after.sameFocus && !after.focusEvents.length,
      JSON.stringify(after)
    );
    await t.page.keyboard.press('Escape');
    await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
    await t.check('final close releases the background lock', !(await state(t)).locked);
  },

  async 'duo polling: inline media keeps its focused control'(t) {
    const view = fixture(true);
    await open(t, view);
    const control = t.page
      .locator('.c2-sheet-host')
      .getByRole('button', { name: 'Full screen', exact: true });
    await control.focus();
    await remember(t);
    await t.page.waitForFunction(
      () => document.querySelector('.c2-sheet-host video')?.readyState >= 2
    );
    await t.page.locator('.c2-sheet-host video').evaluate((e) => {
      window.__pollVideo = e;
      e.pause();
      e.currentTime = 1;
    });
    await t.page.waitForFunction(
      () => Math.abs(window.__pollVideo.currentTime - 1) < 0.1 && !window.__pollVideo.seeking
    );
    await update(t, view, 'Inline media report updated during keyboard use.');
    const after = await state(t);
    await t.check(
      'poll keeps inline media DOM and focused control',
      after.sameFocus &&
        !after.focusEvents.length &&
        (await t.page.evaluate(
          () => window.__pollVideo === document.querySelector('.c2-sheet-host video')
        )),
      JSON.stringify(after)
    );
    view.pinned.updates[0].reported.deliverables[0].note =
      'A refreshed description for the same evidence.';
    view.pinned.updates[0].reported.deliverables[0].media[0].caption = 'Updated clip caption';
    await update(t, view, 'The same deliverable description changed during keyboard use.');
    const description = await state(t);
    await t.check(
      'deliverable text changes retain the same inline media and focus',
      description.sameFocus &&
        !description.focusEvents.length &&
        (await t.page.evaluate(
          () =>
            window.__pollVideo === document.querySelector('.c2-sheet-host video') &&
            Math.abs(window.__pollVideo.currentTime - 1) < 0.1
        )),
      JSON.stringify(description)
    );
    await t.check(
      'deliverable description updates in place',
      await t.page
        .locator('.c2-sheet-host')
        .textContent()
        .then(
          (text) =>
            text.includes('A refreshed description for the same evidence.') &&
            text.includes('Updated clip caption')
        )
    );
    await control.press('Enter');
    await t.page.waitForSelector('.viewer');
    await t.check('retained control still opens the viewer', (await state(t)).viewer);
  },

  async 'duo polling: visible viewer owns focus and Escape after a lower report update'(t) {
    const view = fixture(true);
    await open(t, view);
    await t.page
      .locator('.c2-sheet-host')
      .getByRole('button', { name: 'Full screen', exact: true })
      .click();
    await t.page.waitForSelector('.viewer');
    await t.page.locator('.viewer-btn.close').focus();
    await remember(t);
    await update(t, view, 'Evidence report updated while the viewer stays open.');
    const after = await state(t);
    await t.check(
      'poll never steals focus from the visible viewer',
      after.sameFocus && !after.focusEvents.length,
      JSON.stringify(after)
    );
    await t.page.keyboard.press('Tab');
    await t.check(
      'Tab remains in the visible viewer',
      await t.page.evaluate(() => !!document.activeElement?.closest('.viewer'))
    );
    await t.shot(
      'duo-poll-viewer',
      'After the normal status poll, the visible viewer remains the keyboard owner above the same pin sheet.'
    );
    await t.page.keyboard.press('Escape');
    const closed = await state(t);
    await t.check(
      'Escape closes only the newest viewer',
      !closed.viewer && closed.pin,
      JSON.stringify(closed)
    );
    if (closed.viewer) await t.page.locator('.viewer-btn.close').click();
    await t.check(
      'underlying sheet remains locked and keeps identity',
      (await state(t)).locked && (await state(t)).sameSheet
    );
    await t.page.keyboard.press('Escape');
    await t.check(
      'second Escape closes the sheet and unlocks',
      !(await state(t)).pin && !(await state(t)).locked
    );
  },
};

for (const kind of ['details', 'activation']) {
  tests[`duo polling: poll never steals focus from open ${kind}`] = async (t) => {
    const view = fixture();
    await open(t, view, false, true);
    const selector = kind === 'details' ? '.details' : '.adialog';
    await bridge(
      t,
      kind,
      kind === 'details'
        ? { id: 'fixture', title: 'Synthetic account', meters: [], amounts: [], facts: [] }
        : { product: 'Codex', targetProfile: 'fixture', canConfirm: false, processes: [] }
    );
    await t.page.waitForSelector(selector);
    const control = t.page
      .locator(kind === 'details' ? '[data-probe="details-close"]' : '.adialog .btn')
      .first();
    await control.focus();
    await remember(t);
    await update(t, view, `Pin update underneath the open ${kind}.`);
    const after = await state(t);
    await t.check(
      `poll never steals focus from ${kind}`,
      after.sameFocus && !after.focusEvents.length,
      JSON.stringify(after)
    );
    await t.check(
      'newest overlay is painted above the pin',
      await control.evaluate((el) => {
        const r = el.getBoundingClientRect();
        const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
        return el === hit || el.contains(hit);
      })
    );
    await t.page.keyboard.press('Escape');
    // Approval close is acknowledged by the bridge, just as in the other isolated frame checks.
    if (kind === 'activation') await bridge(t, kind, null);
    await t.page.waitForSelector(selector, { state: 'detached' });
    await t.check(
      'closing the newest overlay keeps the pin open',
      (await state(t)).pin && (await state(t)).sameSheet
    );
  };
}

tests['duo polling: removing a lower pin never refocuses over the viewer'] = async (t) => {
  const view = fixture(true);
  await open(t, view);
  await t.page
    .locator('.c2-sheet-host')
    .getByRole('button', { name: 'Full screen', exact: true })
    .click();
  await t.page.waitForSelector('.viewer');
  await t.page.locator('.viewer-btn.close').focus();
  await remember(t);
  view.pinned.updates.shift();
  await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
  const after = await state(t);
  await t.check(
    'lower-overlay removal keeps focus in the viewer',
    after.sameFocus && !after.focusEvents.length,
    JSON.stringify(after)
  );
  await t.check('remaining viewer keeps the scroll lock', after.viewer && after.locked);
  await t.page.keyboard.press('Escape');
  await t.check(
    'Escape closes the remaining viewer and unlocks',
    !(await state(t)).viewer && !(await state(t)).locked
  );
};
