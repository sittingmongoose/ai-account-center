// Poll reconciliation may recover DOM-caused focus loss, never re-arm an abandoned action.
import { fixture, open, update } from './duo-poll.check.mjs';

async function setup(t, second = false) {
  const view = fixture(true);
  if (second) {
    const a = view.pinned.updates[0].reported.deliverables[0];
    a.updatedAt = '2026-11-12T07:00:00Z';
    view.pinned.updates[0].reported.deliverables.push({
      id: 'sibling',
      name: 'Sibling evidence',
      kind: 'concept',
      state: 'ready',
      for: 'owner',
      version: 'v1',
      updatedAt: '2026-11-12T06:00:00Z',
    });
  }
  await open(t, view, true);
  t.page.setDefaultTimeout(30000);
  await t.page
    .locator('.c2-sheet-host')
    .getByRole('button', { name: 'Full screen', exact: true })
    .focus();
  return view;
}
async function remember(t) {
  return t.page.evaluate(() => {
    window.__intentFocus = document.activeElement;
    window.__intentEvents = [];
    document.addEventListener('focusin', (e) =>
      window.__intentEvents.push(e.target.outerHTML.slice(0, 120))
    );
    return {
      body: document.activeElement === document.body,
      active: document.activeElement.tagName,
    };
  });
}
async function state(t) {
  return t.page.evaluate(() => ({
    same: document.activeElement === window.__intentFocus,
    body: document.activeElement === document.body,
    active: document.activeElement.outerHTML.slice(0, 160),
    events: window.__intentEvents,
    viewer: !!document.querySelector('.viewer'),
    sheet: !!document.querySelector('.c2-sheet-host'),
  }));
}
export const tests = {};
for (const action of ['reading title', 'sheet background', 'explicit blur']) {
  tests[`duo focus intent: ${action} remains abandoned through a structural poll`] = async (t) => {
    const view = await setup(t);
    if (action === 'reading title') await t.page.locator('.c2-sheet-host .to-name').first().click();
    else if (action === 'sheet background')
      await t.page.locator('.c2-sheet-host .sheet').click({ position: { x: 3, y: 100 } });
    else await t.page.evaluate(() => document.activeElement.blur());
    const before = await remember(t);
    await t.check(
      'the user actually leaves the action on BODY',
      before.body,
      JSON.stringify(before)
    );
    await t.page.keyboard.press('Enter');
    await t.check(
      'Enter before the poll does not activate the abandoned action',
      !(await state(t)).viewer
    );
    view.pinned.updates[0].reported.deliverables[0].note = 'A new unrelated note while reading.';
    await update(t, view, `Structural note update after ${action}`);
    const after = await state(t);
    await t.check(
      'the poll preserves deliberate defocus without any focus event',
      after.same && after.body && !after.events.length,
      JSON.stringify(after)
    );
    await t.page.keyboard.press('Enter');
    await t.check(
      'Enter after the poll still does not re-arm the abandoned action',
      !(await state(t)).viewer
    );
    if (action === 'reading title')
      await t.shot(
        'duo-reading-poll',
        'A real click on reading text leaves the action abandoned through a note insertion and normal status poll.'
      );
  };
}
tests['duo focus intent: Tab selects a different action before an unrelated poll'] = async (t) => {
  const view = await setup(t);
  for (let i = 0; i < 12; i++) {
    await t.page.keyboard.press('Tab');
    if (await t.page.evaluate(() => document.activeElement.matches('.sheet-close'))) break;
  }
  await t.check(
    'trusted Tab selects the sheet Close control',
    await t.page.evaluate(() => document.activeElement.matches('.sheet-close'))
  );
  await remember(t);
  view.pinned.updates[0].reported.deliverables[0].note =
    'An unrelated structural update after Tab.';
  await update(t, view, 'User selected Close before this poll');
  const after = await state(t);
  await t.check(
    'the poll leaves the selected control alone with no focus events',
    after.same && !after.events.length,
    JSON.stringify(after)
  );
  await t.page.keyboard.press('Enter');
  await t.check(
    'Enter activates Close rather than the abandoned Full screen action',
    !(await state(t)).sheet && !(await state(t)).viewer
  );
};
tests[
  'duo focus intent: a focused action survives an unrelated subtree update without refocusing'
] = async (t) => {
  const view = await setup(t);
  await remember(t);
  view.pinned.updates[0].reported.deliverables[0].note = 'Only this note subtree is inserted.';
  await update(t, view, 'Unrelated note, same focused action');
  const after = await state(t);
  await t.check(
    'an untouched focused subtree needs no recovery or focus event',
    after.same && !after.events.length,
    JSON.stringify(after)
  );
};
for (const abandoned of [false, true]) {
  tests[
    `duo focus intent: timestamp reorder ${abandoned ? 'after abandonment stays abandoned' : 'while focused is still repaired'}`
  ] = async (t) => {
    const view = await setup(t, true);
    if (abandoned) await t.page.locator('.c2-sheet-host .to-name').first().click();
    await remember(t);
    view.pinned.updates[0].reported.deliverables[1].updatedAt = '2026-11-12T08:00:00Z';
    await update(t, view, `Reorder with abandoned=${abandoned}`);
    const after = await state(t);
    await t.check(
      'normal timestamp sorting actually moved the sibling first',
      (await t.page.locator('.c2-sheet-host .to-name').first().textContent()) === 'Sibling evidence'
    );
    await t.check(
      'only a control focused before this update can be recovered',
      after.same && (abandoned ? after.body && !after.events.length : !after.body),
      JSON.stringify(after)
    );
    await t.page.keyboard.press('Enter');
    await t.check(
      'Enter respects the current selection after the reorder',
      (await state(t)).viewer === !abandoned
    );
  };
}
tests['duo focus intent: capture occurs after a delayed fetch, just before reconciliation'] =
  async (t) => {
    const view = await setup(t);
    let release;
    const gate = new Promise((resolve) => {
      release = resolve;
    });
    let requested;
    const started = new Promise((resolve) => {
      requested = resolve;
    });
    await t.page.route(
      (u) => u.pathname === '/api/status',
      async (route) => {
        requested();
        await gate;
        await route.fulfill({ json: view });
      }
    );
    await started;
    await t.page.locator('.c2-sheet-host .to-name').first().click();
    const before = await remember(t);
    await t.check('the user abandons focus while the real fetch is outstanding', before.body);
    view.pinned.updates[0].reported.deliverables[0].note =
      'Structural note from the delayed fetch.';
    view.pinned.updates[0].reported.now = 'Delayed response applied';
    release();
    await t.page.waitForFunction(() =>
      document.querySelector('.c2-sheet-host')?.textContent.includes('Delayed response applied')
    );
    const after = await state(t);
    await t.check(
      'fetch-start focus cannot re-arm an action left before reconciliation',
      after.same && after.body && !after.events.length,
      JSON.stringify(after)
    );
    await t.page.keyboard.press('Enter');
    await t.check('the delayed response does not change Enter selection', !(await state(t)).viewer);
  };

// One-shot fixture-only renderer interception places an input after the pre-update capture,
// during a real keyed DOM move. A child consumes bubbling, as real widgets can do.
for (const event of ['pointerdown', 'keydown', 'focusin']) {
  tests[`duo focus intent: consumed ${event} during reconciliation cancels recovery`] = async (
    t
  ) => {
    const view = await setup(t, true);
    await remember(t);
    await t.page.evaluate((event) => {
      const list = document.querySelector('.c2-sheet-host .to-list');
      const sheet = document.querySelector('.c2-sheet-host .sheet');
      const action = document.activeElement;
      for (const method of ['insertBefore', 'appendChild']) {
        const native = list[method];
        list[method] = function (child, ...args) {
          if (!window.__intentRaceFired && child.contains(action)) {
            window.__intentRaceFired = true;
            const target =
              event === 'focusin' ? document.querySelector('.c2-sheet-host .sheet-close') : sheet;
            target.addEventListener(
              event,
              (e) => {
                window.__intentStopped = true;
                e.stopPropagation();
              },
              { once: true }
            );
            if (event === 'focusin') {
              target.focus();
              target.blur();
            } else {
              target.dispatchEvent(
                event === 'pointerdown'
                  ? new PointerEvent(event, { bubbles: true })
                  : new KeyboardEvent(event, { bubbles: true, key: 'ArrowDown' })
              );
              action.blur();
            }
          }
          return native.call(this, child, ...args);
        };
      }
    }, event);
    view.pinned.updates[0].reported.deliverables[1].updatedAt = '2026-11-12T08:00:00Z';
    await update(t, view, `Consumed ${event} during the list move`);
    const after = await state(t);
    await t.check(
      'the input occurs during the actual keyed move and is consumed by a child',
      await t.page.evaluate(() => window.__intentRaceFired && window.__intentStopped)
    );
    await t.check(
      'a post-capture input cannot re-arm the old action',
      after.body,
      JSON.stringify(after)
    );
    await t.page.keyboard.press('Enter');
    await t.check('Enter remains inactive after cancelling the recovery', !(await state(t)).viewer);
  };
}
