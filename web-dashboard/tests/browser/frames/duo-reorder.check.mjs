// Real ten-second polls reorder the keyed rows, without a resize or product-signal mutation.
import { fixture, open, update } from './duo-poll.check.mjs';

function report() {
  const view = fixture();
  view.pinned.updates[0].reported = {
    now: 'Initial reorder report',
    deliverables: ['A', 'B', 'C', 'D'].map((label, i) => ({
      id: `evidence-${label}`,
      name: `Evidence ${label}`,
      kind: 'concept',
      state: 'ready',
      for: 'owner',
      version: 'v1',
      updatedAt: `2026-11-12T0${7 - i}:00:00Z`,
      media: [
        {
          kind: 'video',
          path: '/mnt/Cursor/share/fixture/keyboard-controls.mp4',
          caption: `Clip ${label}`,
        },
      ],
    })),
  };
  return view;
}
const row = (t, label) =>
  t.page
    .locator('.c2-sheet-host .to-row')
    .filter({ has: t.page.getByText(`Evidence ${label}`, { exact: true }) });
async function watch(t, label) {
  const video = row(t, label).locator('video');
  await video.evaluate(async (el) => {
    window.__reorderVideo = el;
    el.muted = true;
    el.loop = true;
    await el.play();
  });
  await row(t, label).getByRole('button', { name: 'Full screen', exact: true }).focus();
  await t.page.evaluate(() => {
    window.__reorderControl = document.activeElement;
    window.__reorderRow = document.activeElement.closest('.to-row');
    window.__reorderSheet = document.querySelector('.c2-sheet-host .sheet');
    window.__reorderMediaEvents = [];
    window.__reorderMoves = 0;
    new MutationObserver((records) => {
      for (const record of records)
        for (const removed of record.removedNodes)
          if (removed.contains(window.__reorderControl)) window.__reorderMoves++;
    }).observe(window.__reorderRow.parentNode, { childList: true });
    for (const name of ['pause', 'emptied', 'loadstart'])
      window.__reorderVideo.addEventListener(name, () => window.__reorderMediaEvents.push(name));
  });
}
async function state(t) {
  return t.page.evaluate(() => ({
    names: [...document.querySelectorAll('.c2-sheet-host .to-name')].map((e) => e.textContent),
    sameSheet: window.__reorderSheet === document.querySelector('.c2-sheet-host .sheet'),
    sameRow: window.__reorderRow?.isConnected,
    samePlayer: window.__reorderVideo?.isConnected,
    sameControl: window.__reorderControl?.isConnected,
    sameFocus: window.__reorderControl === document.activeElement,
    inside: !!document.activeElement?.closest('.c2-sheet-host'),
    focusedRow: document.activeElement?.closest('.to-row')?.querySelector('.to-name')?.textContent,
    focusedRole: document.activeElement?.textContent,
    active: document.activeElement?.tagName,
    playing: window.__reorderVideo && !window.__reorderVideo.paused,
    mediaEvents: window.__reorderMediaEvents,
    moves: window.__reorderMoves,
    viewer: !!document.querySelector('.viewer'),
  }));
}
export const tests = {};
for (const variant of [
  { name: 'metadata control', focus: 'A', change: 'B', stamp: null, order: 'ABCD' },
  { name: 'reviewer sibling timestamp', focus: 'A', change: 'B', stamp: '08:00', order: 'BACD' },
  { name: 'focused row moves up', focus: 'C', change: 'C', stamp: '06:30', order: 'ACBD' },
  { name: 'focused row moves down', focus: 'B', change: 'B', stamp: '04:30', order: 'ACBD' },
  { name: 'focused row moves to first', focus: 'C', change: 'C', stamp: '09:00', order: 'CABD' },
  { name: 'focused row moves to last', focus: 'B', change: 'B', stamp: '03:00', order: 'ACDB' },
]) {
  tests[`duo reorder: ${variant.name} retains playing player and activation`] = async (t) => {
    const view = report();
    await open(t, view, true);
    await watch(t, variant.focus);
    const changed = view.pinned.updates[0].reported.deliverables.find(
      (d) => d.name === `Evidence ${variant.change}`
    );
    changed.note = 'Sibling metadata changed';
    if (variant.stamp) changed.updatedAt = `2026-11-12T${variant.stamp}:00Z`;
    await update(t, view, `Applied ${variant.name}`);
    await t.page.waitForTimeout(150);
    const actual = await state(t);
    await t.check(
      'timestamp sort holds the focused subtree without moving its row',
      actual.names.join(',') === 'Evidence A,Evidence B,Evidence C,Evidence D' && !actual.moves,
      JSON.stringify(actual)
    );
    await t.check(
      'keyed move retains the same sheet, row, player and control',
      actual.sameSheet && actual.sameRow && actual.samePlayer && actual.sameControl,
      JSON.stringify(actual)
    );
    await t.check(
      'surviving control retains focus inside the modal',
      actual.sameFocus && actual.inside,
      JSON.stringify(actual)
    );
    await t.check(
      'playing media survives the move without reload or pause',
      actual.playing && !actual.mediaEvents.length,
      JSON.stringify(actual)
    );
    await t.page.keyboard.press('Enter');
    await t.check('Enter activates the surviving focused control', (await state(t)).viewer);
    await t.page.waitForFunction(
      (expected) =>
        [...document.querySelectorAll('.c2-sheet-host .to-name')]
          .map((e) => e.textContent)
          .join(',') === expected,
      [...variant.order].map((s) => `Evidence ${s}`).join(',')
    );
    await t.check(
      'the latest timestamp order applies after focus leaves for the viewer',
      (await state(t)).names.join(',') === [...variant.order].map((s) => `Evidence ${s}`).join(',')
    );
    if ((await state(t)).viewer) await t.page.keyboard.press('Escape');
    if (variant.name === 'reviewer sibling timestamp')
      await t.shot(
        'duo-reorder-player',
        'After a timestamp-driven sort, the same player and Full screen control retain focus and playback.'
      );
  };
}
for (const [label, neighbour] of [
  ['A', 'B'],
  ['B', 'C'],
  ['D', 'C'],
]) {
  tests[`duo reorder: removing focused ${label} selects neighbour ${neighbour}`] = async (t) => {
    const view = report();
    await open(t, view, true);
    await watch(t, label);
    view.pinned.updates[0].reported.deliverables =
      view.pinned.updates[0].reported.deliverables.filter((d) => d.name !== `Evidence ${label}`);
    await update(t, view, `Removed focused ${label}`);
    await t.page.waitForTimeout(150);
    const actual = await state(t);
    await t.check(
      'focused removed row is actually detached',
      !actual.sameRow && !actual.sameControl,
      JSON.stringify(actual)
    );
    await t.check(
      'focus selects the adjacent surviving row inside the sheet',
      actual.inside && actual.focusedRow === `Evidence ${neighbour}`,
      JSON.stringify(actual)
    );
    await t.check(
      'neighbour keeps the same Full screen control role',
      actual.focusedRole === 'Full screen',
      JSON.stringify(actual)
    );
    await t.page.keyboard.press('Enter');
    await t.check('Enter activates the chosen neighbour', (await state(t)).viewer);
  };
}
tests['duo reorder: a covered pin never steals viewer focus during reorder or removal'] = async (
  t
) => {
  const view = report();
  await open(t, view, true);
  await watch(t, 'A');
  await t.page.keyboard.press('Enter');
  await t.page.waitForSelector('.viewer');
  await t.page.locator('.viewer-btn.close').focus();
  await t.page.evaluate(() => {
    window.__viewerFocus = document.activeElement;
    window.__viewerFocusEvents = [];
    document.addEventListener('focusin', (e) => window.__viewerFocusEvents.push(e.target.tagName));
  });
  view.pinned.updates[0].reported.deliverables[1].updatedAt = '2026-11-12T08:00:00Z';
  await update(t, view, 'Covered pin reordered');
  await t.check(
    'lower reorder cannot steal focus from the top viewer',
    await t.page.evaluate(
      () => document.activeElement === window.__viewerFocus && !window.__viewerFocusEvents.length
    )
  );
  view.pinned.updates[0].reported.deliverables.shift();
  await update(t, view, 'Covered trigger removed');
  await t.check(
    'lower removal cannot steal focus from the top viewer',
    await t.page.evaluate(
      () => document.activeElement === window.__viewerFocus && !window.__viewerFocusEvents.length
    )
  );
  await t.page.keyboard.press('Escape');
  await t.page.waitForSelector('.viewer', { state: 'detached' });
  await t.check(
    'closing the viewer repairs its removed return target inside the sheet',
    (await state(t)).inside
  );
};
tests['duo reorder: a recreated action restores its key and control role'] = async (t) => {
  const view = report();
  view.pinned.updates[0].reported.deliverables[0].path = '/fixture/outside-share.txt';
  await open(t, view, true);
  await t.page.evaluate(() =>
    document.addEventListener(
      'click',
      (e) => {
        const action = e.target.closest('.to-row .to-act a');
        if (action) {
          e.preventDefault();
          window.__replacementActivated = true;
        }
      },
      true
    )
  );
  await row(t, 'A').getByRole('button', { name: 'Copy path', exact: true }).focus();
  await t.page.evaluate(() => {
    window.__oldAction = document.activeElement;
  });
  view.pinned.updates[0].reported.deliverables[0].path =
    '/mnt/Cursor/share/fixture/keyboard-controls.mp4';
  await update(t, view, 'Action emphasis updated');
  await t.page.waitForTimeout(150);
  await t.check(
    'action replacement is a real witness',
    await t.page.evaluate(() => !window.__oldAction.isConnected)
  );
  await t.check(
    'replacement action retains its row key and control role',
    await t.page.evaluate(
      () =>
        document.activeElement?.textContent === 'Open' &&
        document.activeElement.closest('.to-row')?.querySelector('.to-name')?.textContent ===
          'Evidence A'
    )
  );
  await t.page.keyboard.press('Enter');
  await t.check(
    'Enter activates the replacement action',
    await t.page.evaluate(() => window.__replacementActivated === true)
  );
};
