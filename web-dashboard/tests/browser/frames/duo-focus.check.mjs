// Native keyboard navigation in the actual body overlay, including the browser's media-control shadow tree.
import { readFileSync } from 'node:fs';

async function status(t, video = false) {
  await t.newContext({
    viewport: { width: 951, height: 669 },
    screen: { width: 466, height: 678 },
    deviceScaleFactor: 1,
    serviceWorkers: 'block',
  });
  if (video) {
    const view = JSON.parse(
      readFileSync(new URL('../../../leptos/fixtures/status-design-view.json', import.meta.url))
    );
    const card = view.cards.open[1];
    card.options[0].media = [
      {
        kind: 'video',
        url: '/api/status/files/keyboard-controls.mp4',
        label: 'built',
        caption: 'Keyboard fixture video',
      },
    ];
    view.cards.open = [card];
    view.cards.answered = [];
    await t.page.route(
      (u) => new URL(u).pathname === '/api/status',
      (r) => r.fulfill({ json: view })
    );
  }
  await t.goto('/app/status?safe=0,84,34,0');
  await t.page.waitForSelector('.c2-row');
}

export const tests = {
  async 'duo focus: native video playback seek mute and boundary wrapping'(t) {
    await status(t, true);
    const trigger = t.page.getByRole('button', { name: 'Full screen', exact: true }).first();
    await trigger.click();
    await t.page.waitForSelector('.viewer video');
    await t.until('fixture video has metadata', () => {
      const v = document.querySelector('.viewer video');
      return v.readyState >= 2 && v.duration > 0;
    });
    await t.page.locator('.viewer video').evaluate((v) => {
      v.pause();
      v.currentTime = 0;
    });
    await t.page.locator('.viewer-btn.close').focus();
    await t.page.keyboard.press('Tab');
    const reached = await t.page.evaluate(
      () => document.activeElement === document.querySelector('.viewer video')
    );
    await t.check('Tab after Close enters the inline video controls', reached);
    // On the old trap this is false: Tab goes to Rotate and cannot reach the player at all.
    if (!reached) {
      await t.page.keyboard.press('Escape');
      return;
    }
    const cd = await t.page.context().newCDPSession(t.page);
    let playback = false,
      seek = false,
      mute = false,
      internal = 0,
      wrapped = false;
    const seen = [];
    try {
      await cd.send('Accessibility.enable');
      for (let i = 0; i < 28; i++) {
        const tree = await cd.send('Accessibility.getFullAXTree');
        const focused = tree.nodes.filter((n) =>
          n.properties?.some((p) => p.name === 'focused' && p.value?.value)
        );
        const name = focused
          .map((n) => `${n.role?.value || ''}:${n.name?.value || ''}`)
          .join(' | ');
        seen.push(name);
        const inVideo = await t.page.evaluate(
          () => document.activeElement === document.querySelector('.viewer video')
        );
        if (inVideo) internal++;
        if (
          !playback &&
          focused.some(
            (n) => n.role?.value === 'button' && /^(play|pause)$/i.test(n.name?.value || '')
          )
        ) {
          await t.page.keyboard.press('Space');
          await t.until(
            'native Play button starts playback',
            () => !document.querySelector('.viewer video').paused
          );
          await t.page.locator('.viewer video').evaluate((v) => v.pause());
          playback = true;
        }
        if (
          !seek &&
          focused.some(
            (n) =>
              n.role?.value === 'slider' &&
              /progress|seek|timeline|scrub/i.test(n.name?.value || '')
          )
        ) {
          await t.page.locator('.viewer video').evaluate((v) => {
            v.currentTime = 0;
          });
          await t.page.keyboard.press('ArrowRight');
          await t.until(
            'native seek slider changes playback position',
            () => document.querySelector('.viewer video').currentTime > 0
          );
          seek = true;
        }
        if (
          !mute &&
          focused.some(
            (n) => n.role?.value === 'button' && /^(mute|unmute)$/i.test(n.name?.value || '')
          )
        ) {
          const before = await t.page.locator('.viewer video').evaluate((v) => v.muted);
          await t.page.keyboard.press('Space');
          await t.check(
            'native mute button toggles mute',
            (await t.page.locator('.viewer video').evaluate((v) => v.muted)) !== before
          );
          await t.page.keyboard.press('Shift+Tab');
          await t.check(
            'reverse Tab stays within native video controls',
            await t.page.evaluate(
              () => document.activeElement === document.querySelector('.viewer video')
            )
          );
          await t.page.keyboard.press('Tab');
          mute = true;
        }
        if (
          await t.page
            .locator('.viewer-btn')
            .first()
            .evaluate((e) => e === document.activeElement)
        ) {
          wrapped = true;
          break;
        }
        await t.page.keyboard.press('Tab');
      }
      await t.check(
        'playback, seek and mute are reachable through native sequential focus',
        playback && seek && mute,
        JSON.stringify(seen)
      );
      await t.check(
        'Tab traverses several native controls before wrapping',
        internal >= 3 && wrapped,
        JSON.stringify(seen)
      );
      await t.page.locator('.viewer-btn').first().focus();
      await t.page.keyboard.press('Shift+Tab');
      await t.check(
        'Shift+Tab from the first control wraps into the video',
        await t.page.evaluate(
          () => document.activeElement === document.querySelector('.viewer video')
        )
      );
      await t.shot(
        'duo-video-keyboard',
        'Viewer with the native player reached by keyboard: playback, timeline and mute remain usable.'
      );
    } finally {
      await cd.send('Accessibility.disable');
      await cd.detach();
    }
    await t.page.keyboard.press('Escape');
    await t.page.waitForSelector('.viewer', { state: 'detached' });
    await t.focused(trigger);
  },
  async 'duo focus: audio summary iframe editable and positive tabindex use native order'(t) {
    await status(t);
    await t.page.locator('.c2-row').first().click();
    await t.page.waitForSelector('.sheet-close');
    await t.page.evaluate(() => {
      const box = document.createElement('section');
      box.innerHTML = `<div tabindex="5" data-focus-key="positive5">Positive 5</div>
        <div tabindex="2" data-focus-key="positive2">Positive 2</div>
        <details id="focus-details"><summary data-focus-key="summary">Keyboard details</summary><button data-focus-key="detail-button">Detail action</button></details>
        <iframe data-focus-key="iframe" title="Keyboard fixture frame" srcdoc="<button id='frame-control'>Frame action</button>" style="height:60px;width:100%"></iframe>
        <div contenteditable="true" data-focus-key="editable">Editable fixture</div>
        <div tabindex="0" data-focus-key="zero">Zero tabindex</div>
        <button tabindex="-1" data-focus-key="negative">Programmatic only</button>
        <button disabled data-focus-key="disabled">Disabled</button>
        <div inert><button data-focus-key="inert">Inert</button></div>
        <div tabindex="0" style="visibility:hidden" data-focus-key="hidden">Hidden</div>
        <div aria-hidden="true" style="height:900px"></div>
        <audio controls muted preload="metadata" src="/api/status/files/keyboard-controls.mp4" data-focus-key="audio"></audio>`;
      document.querySelector('.c2-sheet-host .sheet').append(box);
    });
    await t.page.locator('[data-focus-key="positive2"]').focus();
    await t.page.keyboard.press('Shift+Tab');
    await t.focused('[data-focus-key="audio"]');
    await t.visible(
      '[data-focus-key="audio"]',
      'reverse wrapping scrolls the last native control into view'
    );
    await t.page.locator('[data-focus-key="positive2"]').focus();
    await t.page.keyboard.press('Tab');
    await t.focused('[data-focus-key="positive5"]');
    const seen = new Set();
    let summary = false,
      frame = false;
    for (let i = 0; i < 55; i++) {
      const state = await t.page.evaluate(() => ({
        key: document.activeElement?.dataset.focusKey || '',
        inside: !!document.activeElement?.closest('.c2-sheet-host'),
      }));
      if (state.key) seen.add(state.key);
      await t.check(`Tab ${i}: focus stays in the sheet`, state.inside);
      if (state.key === 'summary' && !summary) {
        await t.page.keyboard.press('Space');
        await t.check(
          'native summary opens details',
          await t.page.locator('#focus-details').evaluate((e) => e.open)
        );
        summary = true;
      }
      if (state.key === 'iframe' && !frame) {
        await t.page.keyboard.press('Space');
        await t.check(
          'keyboard enters the iframe button',
          await t.page
            .locator('[data-focus-key="iframe"]')
            .evaluate((e) => e.contentDocument.activeElement?.id === 'frame-control')
        );
        frame = true;
      }
      if (
        [
          'summary',
          'detail-button',
          'iframe',
          'editable',
          'zero',
          'audio',
          'positive2',
          'positive5',
        ].every((k) => seen.has(k))
      )
        break;
      await t.page.keyboard.press('Tab');
    }
    await t.check(
      'every native focusable kind and nonnegative tabindex is reachable',
      [
        'summary',
        'detail-button',
        'iframe',
        'editable',
        'zero',
        'audio',
        'positive2',
        'positive5',
      ].every((k) => seen.has(k)),
      JSON.stringify([...seen])
    );
    await t.check(
      'negative, disabled, inert and hidden controls are skipped',
      !['negative', 'disabled', 'inert', 'hidden'].some((k) => seen.has(k)),
      JSON.stringify([...seen])
    );
    if (seen.has('positive2'))
      await t.visible(
        '[data-focus-key="positive2"]',
        'forward wrapping scrolls the first positive stop into view'
      );
    await t.page.keyboard.press('Escape');
    await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
  },
};
