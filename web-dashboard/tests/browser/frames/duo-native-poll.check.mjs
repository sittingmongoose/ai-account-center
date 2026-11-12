// Browser-owned controls must survive a normal poll without a move of their focused subtree.
import { fixture, open, update } from './duo-poll.check.mjs';

function report() {
  const view = fixture();
  view.pinned.updates[0].reported = {
    now: 'Native focus report',
    deliverables: ['A', 'B', 'C'].map((label, i) => ({
      id: `native-${label}`,
      name: `Native ${label}`,
      kind: 'concept',
      state: 'ready',
      for: 'owner',
      version: 'v1',
      updatedAt: `2026-11-12T0${7 - i}:00:00Z`,
      path: '/mnt/Cursor/share/fixture/keyboard-controls.mp4',
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
    .filter({ has: t.page.getByText(`Native ${label}`, { exact: true }) });
const order = (t) => t.page.locator('.c2-sheet-host .to-name').allTextContents();
async function watch(t) {
  await t.page.evaluate(() => {
    window.__nativeRow = document.activeElement.closest('.to-row');
    window.__nativeHost = document.activeElement;
    window.__nativeMoves = 0;
    window.__nativeFocusEvents = [];
    new MutationObserver((records) => {
      for (const record of records)
        for (const removed of record.removedNodes)
          if (removed.contains(window.__nativeRow)) window.__nativeMoves++;
    }).observe(window.__nativeRow.parentNode, { childList: true });
    document.addEventListener('focusin', (e) => window.__nativeFocusEvents.push(e.target.tagName));
  });
}
async function focused(cd) {
  const { nodes } = await cd.send('Accessibility.getFullAXTree');
  const node = nodes.find(
    (n) =>
      n.role?.value !== 'RootWebArea' &&
      n.properties?.some((p) => p.name === 'focused' && p.value?.value)
  );
  return { role: node?.role?.value, name: node?.name?.value, id: node?.backendDOMNodeId };
}
async function mute(t, cd) {
  const video = row(t, 'C').locator('video');
  await video.scrollIntoViewIfNeeded();
  await video.evaluate(async (v) => {
    v.muted = true;
    await v.play();
    v.pause();
    v.muted = false;
  });
  await video.focus();
  for (let i = 0; i < 25; i++) {
    const f = await focused(cd);
    if (
      f.role === 'button' &&
      /^(mute|unmute)$/i.test(f.name) &&
      (await video.evaluate((v) => document.activeElement === v))
    )
      return f;
    await t.page.keyboard.press('Tab');
  }
  throw new Error('Trusted Tab did not reach the selected player native Mute control');
}
export const tests = {
  'duo native poll: focused Mute survives a deferred sort and keeps its Space action': async (
    t
  ) => {
    const view = report();
    await open(t, view, true);
    t.page.setDefaultTimeout(30000);
    const cd = await t.page.context().newCDPSession(t.page);
    try {
      await cd.send('Accessibility.enable');
      const before = await mute(t, cd);
      await t.check(
        'the witness selects the actual native Mute stop',
        before.role === 'button' && /^mute$/i.test(before.name),
        JSON.stringify(before)
      );
      await t.page.keyboard.press('Space');
      await t.check(
        'Space toggles mute without starting playback before polling',
        await row(t, 'C')
          .locator('video')
          .evaluate((v) => v.muted && v.paused)
      );
      await t.page.keyboard.press('Space');
      await watch(t);
      view.pinned.updates[0].reported.deliverables[1].note = 'Matched note-only poll';
      await update(t, view, 'Native metadata control');
      await t.check(
        'a metadata poll preserves the identical native stop',
        (await focused(cd)).id === before.id
      );
      view.pinned.updates[0].reported.deliverables[2].updatedAt = '2026-11-12T09:00:00Z';
      view.pinned.updates[0].reported.deliverables[2].media[0].caption =
        'Caption updated while Mute stays selected';
      await update(t, view, 'Native reorder deferred');
      await t.check(
        'the focused row keeps its current place until focus leaves',
        (await order(t)).join(',') === 'Native A,Native B,Native C'
      );
      await t.check(
        'the focused subtree is never removed or refocused',
        await t.page.evaluate(
          () =>
            !window.__nativeMoves &&
            !window.__nativeFocusEvents.length &&
            document.activeElement === window.__nativeHost
        )
      );
      const after = await focused(cd);
      await t.check(
        'the actual native Mute backend node stays selected',
        after.id === before.id && after.role === 'button' && /^mute$/i.test(after.name),
        JSON.stringify({ before, after })
      );
      await t.check(
        'data in the held row still updates in place',
        await row(t, 'C')
          .getByText('Caption updated while Mute stays selected', { exact: true })
          .isVisible()
      );
      await t.page.keyboard.press('Space');
      await t.check(
        'Space after the poll still toggles mute and never starts playback',
        await row(t, 'C')
          .locator('video')
          .evaluate((v) => v.muted && v.paused)
      );
      await t.shot(
        'duo-native-held',
        'After a real timestamp poll, native Mute remains selected without moving the row; Space still toggles mute while paused.'
      );
      await t.page.keyboard.press('Space');
      view.pinned.updates[0].reported.deliverables =
        view.pinned.updates[0].reported.deliverables.filter((d) => d.name !== 'Native B');
      view.pinned.updates[0].reported.deliverables.push({
        id: 'native-D',
        name: 'Native D',
        kind: 'concept',
        state: 'ready',
        for: 'owner',
        version: 'v1',
        updatedAt: '2026-11-12T10:00:00Z',
      });
      await update(t, view, 'Neighbour removed and new row reported');
      await t.page.waitForFunction(() =>
        [...document.querySelectorAll('.c2-sheet-host .to-name')].some(
          (el) => el.textContent === 'Native D'
        )
      );
      await t.check(
        'a mixed removal/addition never moves or refocuses the surviving native subtree',
        (await focused(cd)).id === before.id &&
          (await t.page.evaluate(() => !window.__nativeMoves && !window.__nativeFocusEvents.length))
      );
      await t.check(
        'the new row appends promptly without changing the focused surviving order',
        (await order(t)).join(',') === 'Native A,Native C,Native D'
      );
      await t.page.keyboard.press('Space');
      await t.check(
        'Space after the mixed update still toggles mute while paused',
        await row(t, 'C')
          .locator('video')
          .evaluate((v) => v.muted && v.paused)
      );
      await t.page.locator('.c2-sheet-host .sheet-close').focus();
      await t.page.waitForFunction(
        () => document.querySelector('.c2-sheet-host .to-name')?.textContent === 'Native D'
      );
      await t.check(
        'the pending timestamp order applies after focus leaves the row',
        (await order(t)).join(',') === 'Native D,Native C,Native A'
      );
      await t.check(
        'flushing the order respects the newly selected Close control',
        await t.page.evaluate(() => document.activeElement.matches('.sheet-close'))
      );
    } finally {
      await cd.send('Accessibility.disable');
      await cd.detach();
    }
  },
  'duo native poll: focused Open stays mounted and flushes the latest order on blur': async (t) => {
    const view = report();
    await open(t, view, true);
    t.page.setDefaultTimeout(30000);
    await row(t, 'A').getByRole('link', { name: 'Open', exact: true }).focus();
    await watch(t);
    for (const [label, stamp] of [
      ['C', '09'],
      ['B', '10'],
    ]) {
      const d = view.pinned.updates[0].reported.deliverables.find(
        (d) => d.name === `Native ${label}`
      );
      d.updatedAt = `2026-11-12T${stamp}:00:00Z`;
      d.note = `Live note for ${label}`;
      await update(t, view, `Deferred ${label} timestamp`);
      await t.check(
        `focused Open defers the ${label} sort without a DOM move or focus event`,
        (await order(t)).join(',') === 'Native A,Native B,Native C' &&
          (await t.page.evaluate(
            () =>
              !window.__nativeMoves &&
              !window.__nativeFocusEvents.length &&
              document.activeElement === window.__nativeHost
          ))
      );
      await t.check(
        `the ${label} text update is still live`,
        await row(t, label).getByText(`Live note for ${label}`, { exact: true }).isVisible()
      );
    }
    await row(t, 'A')
      .getByRole('link', { name: 'Open', exact: true })
      .evaluate((el) => el.blur());
    await t.page.waitForFunction(
      () => document.querySelector('.c2-sheet-host .to-name')?.textContent === 'Native B'
    );
    await t.check(
      'blur flushes the newest complete order rather than an old pending sort',
      (await order(t)).join(',') === 'Native B,Native C,Native A'
    );
    await t.check(
      'the original Open node survives the eventual move',
      await t.page.evaluate(() => window.__nativeHost.isConnected && window.__nativeRow.isConnected)
    );
    await t.check(
      'flushing after explicit blur never re-arms the abandoned Open action',
      await t.page.evaluate(() => document.activeElement === document.body)
    );
    await t.page.keyboard.press('Escape');
    await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
    await open(t, view, true);
    await t.check(
      'reopening starts with the latest order',
      (await order(t)).join(',') === 'Native B,Native C,Native A'
    );
  },
  'duo native poll: removing a native-focused row still recovers inside the sheet': async (t) => {
    const view = report();
    await open(t, view, true);
    t.page.setDefaultTimeout(30000);
    const cd = await t.page.context().newCDPSession(t.page);
    try {
      await cd.send('Accessibility.enable');
      await mute(t, cd);
      await watch(t);
      view.pinned.updates[0].reported.deliverables.pop();
      await update(t, view, 'Native focused row removed');
      await t.check(
        'removal really detaches the focused player',
        await t.page.evaluate(() => !window.__nativeHost.isConnected)
      );
      await t.check(
        'removal retains a usable modal focus destination',
        await t.page.evaluate(
          () =>
            document.activeElement !== document.body &&
            !!document.activeElement.closest('.c2-sheet-host')
        )
      );
      await t.check(
        'removed rows do not remain as deferred entries',
        (await order(t)).join(',') === 'Native A,Native B'
      );
    } finally {
      await cd.send('Accessibility.disable');
      await cd.detach();
    }
  },
};
