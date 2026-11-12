// Every mutable sheet collection shares focus-safe ordering. Text-only task/stage rows use an
// explicitly fixture-added tabindex to exercise the same contract without adding product controls.
import { fixture, open, update } from './duo-poll.check.mjs';

function report(labels = ['Neighbour', 'Target']) {
  const view = fixture();
  view.pinned.updates[0].reported = {
    now: 'Shared lists fixture',
    deliverables: [
      {
        id: 'media-pair',
        name: 'Two synthetic clips',
        kind: 'concept',
        state: 'ready',
        for: 'owner',
        version: 'v1',
        updatedAt: '2026-11-12T07:00:00Z',
        path: '/mnt/Cursor/share/fixture/keyboard-controls.mp4',
        media: labels.map((label) => ({
          kind: 'video',
          path: `/mnt/Cursor/share/fixture/${label.toLowerCase()}-keyboard-controls.mp4`,
          caption: `${label} clip`,
        })),
      },
    ],
  };
  return view;
}
async function focused(cd) {
  const { nodes } = await cd.send('Accessibility.getFullAXTree');
  const n = nodes.find(
    (n) =>
      n.role?.value !== 'RootWebArea' &&
      n.properties?.some((p) => p.name === 'focused' && p.value?.value)
  );
  return { role: n?.role?.value, name: n?.name?.value, id: n?.backendDOMNodeId };
}
async function selectMute(t, cd) {
  const video = t.page
    .locator('.c2-sheet-host figure.media')
    .filter({ hasText: 'Target clip' })
    .locator('video');
  await video.scrollIntoViewIfNeeded();
  await video.evaluate(async (v) => {
    v.muted = true;
    await v.play();
    v.pause();
    v.muted = false;
  });
  await video.focus();
  for (let i = 0; i < 35; i++) {
    const ax = await focused(cd);
    if (
      ax.role === 'button' &&
      /^mute$/i.test(ax.name) &&
      (await video.evaluate((v) => document.activeElement === v))
    )
      return ax;
    await t.page.keyboard.press('Tab');
  }
  throw new Error('Trusted Tab did not select Target native Mute');
}
async function watch(t, root) {
  await t.page.evaluate((root) => {
    window.__listActive = document.activeElement;
    window.__listMoves = [];
    window.__listFocus = [];
    new MutationObserver((records) => {
      for (const record of records)
        for (const removed of record.removedNodes)
          if (removed.contains(window.__listActive)) window.__listMoves.push(removed.nodeName);
    }).observe(document.querySelector(root), { childList: true, subtree: true });
    document.addEventListener('focusin', (e) => window.__listFocus.push(e.target.tagName));
  }, root);
}
const stable = (t) =>
  t.page.evaluate(
    () =>
      window.__listActive.isConnected &&
      document.activeElement === window.__listActive &&
      !window.__listMoves.length &&
      !window.__listFocus.length
  );
const captions = (t) =>
  t.page.locator('.c2-sheet-host .media-list figcaption > span').allTextContents();
const mediaState = (t) =>
  t.page.evaluate(() => ({
    connected: window.__listActive.isConnected,
    muted: window.__listActive.muted,
    paused: window.__listActive.paused,
  }));
async function setup(t, view) {
  await open(t, view, true);
  t.page.setDefaultTimeout(30000);
}
async function blurToClose(t) {
  await t.page.locator('.c2-sheet-host .sheet-close').focus();
  await t.page.waitForTimeout(100);
}

export const tests = {
  'duo lists: Sol exact inner-media reversal preserves native Mute and nested boundaries': async (
    t
  ) => {
    const view = report();
    await setup(t, view);
    const cd = await t.page.context().newCDPSession(t.page);
    try {
      await cd.send('Accessibility.enable');
      const before = await selectMute(t, cd);
      await t.check(
        'trusted Tab selects actual Target Mute',
        before.role === 'button' && /^mute$/i.test(before.name)
      );
      await t.page.keyboard.press('Space');
      await t.check(
        'initial Space toggles mute without playing',
        (await mediaStateFromTarget(t)).muted && (await mediaStateFromTarget(t)).paused
      );
      await t.page.keyboard.press('Space');
      await watch(t, '.c2-sheet-host .to-row');
      const d = view.pinned.updates[0].reported.deliverables[0];
      d.note = 'Matched metadata without a reorder';
      await update(t, view, 'Media metadata control');
      await t.check(
        'matched metadata preserves exact native AX stop and action',
        (await focused(cd)).id === before.id && (await stable(t))
      );
      d.media.reverse();
      await update(t, view, 'Exact Sol media reversal');
      const after = await focused(cd);
      await t.check(
        'same-row media reversal retains actual Mute rather than VIDEO',
        after.id === before.id && after.role === 'button' && /^mute$/i.test(after.name),
        JSON.stringify({ before, after })
      );
      await t.check('no ancestor of the focused video is removed or refocused', await stable(t));
      await t.check(
        'inner media keeps its previous order while it owns focus',
        (await captions(t)).join(',') === 'Neighbour clip,Target clip'
      );
      await t.page.keyboard.press('Space');
      const action = await mediaState(t);
      await t.check(
        'Space after the exact poll still mutes without playback',
        action.muted && action.paused,
        JSON.stringify(action)
      );
      await t.shot(
        'duo-inner-media-held',
        'Two media in one unchanged deliverable: native Mute remains selected through an ordinary media-order reversal.'
      );
      d.media.find((m) => m.caption === 'Target clip').caption = 'Target clip updated live';
      view.pinned.updates[0].reported.deliverables.push({
        id: 'newer',
        name: 'Newer evidence',
        kind: 'concept',
        state: 'ready',
        for: 'owner',
        version: 'v1',
        updatedAt: '2026-11-12T09:00:00Z',
      });
      await update(t, view, 'Nested outer and inner orders pending');
      await t.check(
        'data stays live while both containing orders are held',
        (await t.page
          .locator('.c2-sheet-host')
          .getByText('Target clip updated live', { exact: true })
          .isVisible()) && (await stable(t))
      );
      await t.page
        .locator('.c2-sheet-host .to-row')
        .first()
        .getByRole('link', { name: 'Open', exact: true })
        .focus();
      await t.page.waitForFunction(
        () =>
          document.querySelector('.c2-sheet-host figcaption > span')?.textContent ===
          'Target clip updated live'
      );
      await t.check(
        'leaving inner media releases it while outer deliverables still hold',
        (await captions(t)).join(',') === 'Target clip updated live,Neighbour clip' &&
          (await t.page.locator('.c2-sheet-host .to-name').first().textContent()) ===
            'Two synthetic clips'
      );
      await blurToClose(t);
      await t.check(
        'leaving the outer list applies its latest complete order',
        (await t.page.locator('.c2-sheet-host .to-name').first().textContent()) === 'Newer evidence'
      );
    } finally {
      await cd.send('Accessibility.disable');
      await cd.detach();
    }
  },
  'duo lists: display limit never evicts a still-reported focused media item': async (t) => {
    const view = report(['Target', 'B', 'C', 'D']);
    await setup(t, view);
    const cd = await t.page.context().newCDPSession(t.page);
    try {
      await cd.send('Accessibility.enable');
      const before = await selectMute(t, cd);
      await watch(t, '.c2-sheet-host .to-row');
      view.pinned.updates[0].reported.deliverables[0].media.reverse();
      await update(t, view, 'Media display boundary reversed');
      await t.check(
        'top-three clipping cannot remove the focused surviving video',
        (await stable(t)) && (await focused(cd)).id === before.id
      );
      await t.check(
        'the displayed media order stays held at its existing limit',
        (await captions(t)).join(',') === 'Target clip,B clip,C clip'
      );
      await t.page.keyboard.press('Space');
      const action = await mediaState(t);
      await t.check(
        'the retained native target still mutes and stays paused',
        action.muted && action.paused
      );
      await blurToClose(t);
      await t.check(
        'after blur the latest top-three selection applies',
        (await captions(t)).join(',') === 'D clip,C clip,B clip'
      );
      await t.check(
        'the old target may leave the limited view only after focus leaves',
        !(await mediaState(t)).connected
      );
    } finally {
      await cd.send('Accessibility.disable');
      await cd.detach();
    }
  },
  'duo lists: older versions hold focused Open and keep version data live': async (t) => {
    const view = report([]);
    const d = view.pinned.updates[0].reported.deliverables[0];
    d.versions = ['old-a', 'old-b'].map((version) => ({
      version,
      path: '/mnt/Cursor/share/fixture/keyboard-controls.mp4',
      updatedAt: '2026-11-12T05:00:00Z',
    }));
    await setup(t, view);
    await t.page.locator('.c2-sheet-host .to-row [data-focus-role="versions-toggle"]').click();
    await t.page
      .locator('.c2-sheet-host .to-older li')
      .first()
      .getByRole('link', { name: 'Open', exact: true })
      .focus();
    await t.check(
      'the witness selects the sheet version Open control',
      await t.page.evaluate(() => !!document.activeElement.closest('.c2-sheet-host .to-older'))
    );
    const oldText = await t.page.locator('.c2-sheet-host .to-older li').first().textContent();
    await watch(t, '.c2-sheet-host .to-older');
    d.versions[0].updatedAt = '2026-11-12T10:00:00Z';
    d.versions.reverse();
    await update(t, view, 'Versions reordered');
    await t.check(
      'version Open stays selected without a moved or replaced subtree',
      await stable(t)
    );
    await t.check(
      'version order holds while its time updates live',
      (await t.page.locator('.c2-sheet-host .to-older li b').allTextContents()).join(',') ===
        'old-a,old-b' &&
        (await t.page.locator('.c2-sheet-host .to-older li').first().textContent()) !== oldText
    );
    await blurToClose(t);
    await t.check(
      'version order flushes after focus leaves',
      (await t.page.locator('.c2-sheet-host .to-older li b').allTextContents()).join(',') ===
        'old-b,old-a'
    );
  },
  'duo lists: task rows retain their id through reordering and title changes': async (t) => {
    const view = report([]);
    const pin = view.pinned.updates[0];
    pin.done = [];
    pin.left = ['A', 'B', 'C'].map((label) => ({
      id: `task-${label}`,
      title: `Task ${label}`,
      state: 'working',
    }));
    await setup(t, view);
    // Task rows are text-only today. This fixture-only focusability seam exercises future controls too.
    await t.page
      .locator('.c2-sheet-host .c2-tg-rows > li')
      .first()
      .evaluate((el) => (el.tabIndex = 0));
    await t.page.locator('.c2-sheet-host .c2-tg-rows > li').first().focus();
    await t.check(
      'the task witness focuses its fixture-enabled row',
      await t.page.evaluate(() => document.activeElement.matches('.c2-sheet-host .c2-tg-rows > li'))
    );
    await watch(t, '.c2-sheet-host .c2-tg-rows');
    pin.left[0].title = 'Task A updated';
    pin.left = [pin.left[2], pin.left[0], pin.left[1]];
    await update(t, view, 'Task rows reordered');
    await t.check('task identity survives a title update and pending sort', await stable(t));
    await t.check(
      'task text updates without publishing a focused-list reorder',
      (await t.page.locator('.c2-sheet-host .c2-tg-t').allTextContents()).join(',') ===
        'Task A updated,Task B,Task C'
    );
    await blurToClose(t);
    await t.check(
      'task order applies after leaving the task list',
      (await t.page.locator('.c2-sheet-host .c2-tg-t').allTextContents()).join(',') ===
        'Task C,Task A updated,Task B'
    );
  },
  'duo lists: stages retain focus and live state through a report reorder': async (t) => {
    const view = report([]);
    const pin = view.pinned.updates[0];
    pin.left = [];
    pin.done = [];
    pin.reported.stages = ['A', 'B', 'C'].map((label) => ({
      label: `Stage ${label}`,
      state: 'working',
    }));
    await setup(t, view);
    // Stages have no intrinsic Tab stops; only the fixture adds a focusable row for this contract test.
    await t.page
      .locator('.c2-sheet-host .c2-stages li')
      .first()
      .evaluate((el) => (el.tabIndex = 0));
    await t.page.locator('.c2-sheet-host .c2-stages li').first().focus();
    await t.check(
      'the stage witness focuses its fixture-enabled row',
      await t.page.evaluate(() => document.activeElement.matches('.c2-sheet-host .c2-stages li'))
    );
    await watch(t, '.c2-sheet-host .c2-stages');
    pin.reported.stages[0].note = 'Updated stage data';
    pin.reported.stages.reverse();
    await update(t, view, 'Stages reordered');
    await t.check('stage row stays mounted and selected without a focus repair', await stable(t));
    await t.check(
      'stage order holds while its note stays live',
      (await t.page.locator('.c2-sheet-host .c2-stages .lab').allTextContents()).join(',') ===
        'Stage A Updated stage data,Stage B,Stage C'
    );
    await blurToClose(t);
    await t.check(
      'stage order flushes after focus leaves',
      (await t.page.locator('.c2-sheet-host .c2-stages .lab').allTextContents()).join(',') ===
        'Stage C,Stage B,Stage A Updated stage data'
    );
  },
  'duo lists: task-group membership changes preserve a focused group control': async (t) => {
    const view = report([]);
    const pin = view.pinned.updates[0];
    for (const item of pin.left) item.state = 'working';
    for (const item of pin.done) item.state = 'done';
    await setup(t, view);
    const done = t.page
      .locator('.c2-sheet-host .c2-tg')
      .filter({ has: t.page.locator('h5').filter({ hasText: /^Done/ }) });
    await done.getByRole('button', { name: /^Show all/ }).click();
    await done.getByRole('button', { name: 'Fewer', exact: true }).focus();
    await watch(t, '.c2-sheet-host .c2-col:last-child');
    pin.left = [{ id: 'new-todo', title: 'New todo task', state: 'todo' }];
    pin.done[0].note = 'Done task updated live';
    await update(t, view, 'Task groups changed');
    await t.page.waitForFunction(
      () => document.querySelectorAll('.c2-sheet-host .c2-tg').length === 2
    );
    await t.check('surviving group Fewer keeps focus and its subtree', await stable(t));
    await t.check(
      'group data stays live while a mixed membership change is held',
      (await done.getByText('Done task updated live', { exact: true }).isVisible()) &&
        (await t.page.locator('.c2-sheet-host .c2-tg').first().textContent()).includes('Done')
    );
    await blurToClose(t);
    await t.check(
      'latest task groups appear after focus leaves',
      (await t.page.locator('.c2-sheet-host .c2-tg').count()) === 2 &&
        (await t.page
          .locator('.c2-sheet-host .c2-tg')
          .first()
          .textContent()
          .then((s) => s.includes('To do')))
    );
  },
};
async function mediaStateFromTarget(t) {
  return t.page
    .locator('.c2-sheet-host figure.media')
    .filter({ hasText: 'Target clip' })
    .locator('video')
    .evaluate((v) => ({ muted: v.muted, paused: v.paused }));
}
