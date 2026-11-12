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
  if (t.args.packaged) {
    const ui = new URL('../../../../dist/ui/', import.meta.url);
    const manifest = JSON.parse(readFileSync(new URL('ui-build-manifest.json', ui)));
    await t.page.route('**/app/pkg/*', (r) =>
      r.fulfill({
        path: new URL(
          `pkg/${manifest.leptos.buildId}/${new URL(r.request().url()).pathname.split('/').pop()}`,
          ui
        ).pathname,
      })
    );
  }
  await t.goto('/app/status?safe=0,84,34,0');
  await t.page.waitForSelector('.c2-row');
}

async function videoTour(t, cd, reverse) {
  const direction = reverse ? 'reverse' : 'forward';
  const first = t.page.locator('.viewer-btn').first();
  await first.focus();
  const stops = [];
  let playback = false,
    seek = false,
    mute = false,
    wrapped = false;
  for (let i = 0; i < 36; i++) {
    const tree = await cd.send('Accessibility.getFullAXTree');
    const node = tree.nodes.find(
      (n) =>
        n.role?.value !== 'RootWebArea' &&
        n.properties?.some((p) => p.name === 'focused' && p.value?.value)
    );
    const role = node?.role?.value || '';
    const name = node?.name?.value || '';
    const key = `${role}:${name.replace(/^(play|pause)$/i, 'playback').replace(/^(unmute|mute)$/i, 'mute')}`;
    if (i && (await first.evaluate((e) => e === document.activeElement))) {
      wrapped = true;
      break;
    }
    stops.push(key);
    await t.check(
      `${direction} video ${i}: focus stays in viewer`,
      await t.page.evaluate(() => !!document.activeElement?.closest('.viewer'))
    );
    if (role === 'button' && /^(play|pause)$/i.test(name) && !playback) {
      await t.page.keyboard.press('Space');
      await t.until(
        `${direction}: native Play starts playback`,
        () => !document.querySelector('.viewer video').paused
      );
      await t.page.locator('.viewer video').evaluate((v) => v.pause());
      playback = true;
    }
    if (role === 'slider' && /progress|seek|timeline|scrub/i.test(name) && !seek) {
      await t.page.locator('.viewer video').evaluate((v) => {
        v.currentTime = 0;
      });
      await t.page.keyboard.press('ArrowRight');
      await t.until(
        `${direction}: native seek changes position`,
        () => document.querySelector('.viewer video').currentTime > 0
      );
      seek = true;
    }
    if (role === 'button' && /^(mute|unmute)$/i.test(name) && !mute) {
      const before = await t.page.locator('.viewer video').evaluate((v) => v.muted);
      await t.page.keyboard.press('Space');
      await t.check(
        `${direction}: native mute toggles`,
        (await t.page.locator('.viewer video').evaluate((v) => v.muted)) !== before
      );
      mute = true;
    }
    await t.page.keyboard.press(reverse ? 'Shift+Tab' : 'Tab');
  }
  await t.check(
    `${direction}: native playback seek and mute all work`,
    playback && seek && mute,
    JSON.stringify(stops)
  );
  await t.check(
    `${direction}: full tour wraps to first viewer control`,
    wrapped,
    JSON.stringify(stops)
  );
  return stops;
}

export const tests = {
  async 'duo focus: complete forward and reverse native video traversal'(t) {
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
    const cd = await t.page.context().newCDPSession(t.page);
    try {
      await cd.send('Accessibility.enable');
      const forward = await videoTour(t, cd, false);
      const reverse = await videoTour(t, cd, true);
      await t.check(
        'Shift+Tab walks every native focus stop in the exact reverse order',
        JSON.stringify(reverse) === JSON.stringify([forward[0], ...forward.slice(1).reverse()]),
        JSON.stringify({ forward, reverse })
      );
      await t.shot(
        'duo-video-reverse',
        'Native video controls remain reachable across the reverse modal boundary.'
      );
    } finally {
      await cd.send('Accessibility.disable');
      await cd.detach();
    }
    await t.page.keyboard.press('Escape');
    await t.page.waitForSelector('.viewer', { state: 'detached' });
    await t.focused(trigger);
  },
  async 'duo focus: every sheet stop in both directions with audio and frame scopes'(t) {
    await status(t);
    await t.page.locator('.c2-row').first().click();
    await t.page.waitForSelector('.sheet-close');
    await t.page.evaluate(() => {
      const box = document.createElement('section');
      box.innerHTML = `<div tabindex="5" data-focus-key="positive5">Positive 5</div>
        <div tabindex="2" data-focus-key="positive2">Positive 2</div>
        <details id="focus-details"><summary data-focus-key="summary">Keyboard details</summary><button data-focus-key="detail-button">Detail action</button></details>
        <iframe data-focus-key="frame" title="Keyboard frame" srcdoc="<button data-focus-key='frame-first'>First</button><input data-focus-key='frame-middle' aria-label='Middle'><button data-focus-key='frame-last'>Last</button>" style="height:60px;width:100%"></iframe>
        <iframe sandbox="allow-scripts" data-focus-key="opaque" title="Opaque keyboard frame" srcdoc="<button data-focus-key='opaque-first'>First</button><button data-focus-key='opaque-last'>Last</button>" style="height:60px;width:100%"></iframe>
        <div contenteditable="true" data-focus-key="editable">Editable fixture</div>
        <div tabindex="0" data-focus-key="zero">Zero tabindex</div>
        <button tabindex="-1" data-focus-key="negative">Programmatic only</button>
        <button disabled data-focus-key="disabled">Disabled</button>
        <div inert><button data-focus-key="inert">Inert</button></div>
        <div tabindex="0" style="visibility:hidden" data-focus-key="hidden">Hidden</div>
        <div aria-hidden="true" style="height:900px"></div>
        <audio controls muted preload="metadata" src="/api/status/files/keyboard-controls.mp4" data-focus-key="audio"></audio>`;
      document.querySelector('.c2-sheet-host .sheet').append(box);
      // Give existing controls distinct identities too, so two links cannot mask a skipped stop.
      document.querySelectorAll('.c2-sheet-host *').forEach((el, i) => {
        if (!el.dataset.focusKey) el.dataset.focusKey = `element-${i}`;
      });
      const bg = document.createElement('div');
      bg.innerHTML =
        '<button tabindex="3">Background 3</button><button tabindex="6">Background 6</button>';
      document.body.prepend(bg);
    });
    await t.page.locator('[data-focus-key="summary"]').focus();
    await t.page.keyboard.press('Space');
    await t.check(
      'native summary opens details',
      await t.page.locator('#focus-details').evaluate((e) => e.open)
    );
    await t.until(
      'audio controls have metadata',
      () => document.querySelector('audio').readyState >= 2
    );
    const positive = t.page.locator('[data-focus-key="positive2"]');
    await positive.focus();
    await positive.evaluate((el) => {
      const cancel = (e) => {
        if (e.key === 'Tab') {
          e.preventDefault();
          el.removeEventListener('keydown', cancel);
        }
      };
      el.addEventListener('keydown', cancel);
    });
    await t.page.keyboard.press('Shift+Tab');
    await t.focused(positive);
    const cd = await t.page.context().newCDPSession(t.page);
    try {
      await cd.send('Accessibility.enable');
      const forward = await sheetTour(t, cd, false);
      const reverse = await sheetTour(t, cd, true);
      const expected = [
        'positive2',
        'positive5',
        'summary',
        'detail-button',
        'frame-first',
        'frame-middle',
        'frame-last',
        'opaque-first',
        'opaque-last',
        'editable',
        'zero',
      ];
      for (const [direction, stops] of [
        ['forward', forward],
        ['reverse', reverse],
      ]) {
        await t.check(
          `${direction}: every author focus stop is reachable`,
          expected.every((k) => stops.includes(k)),
          JSON.stringify(stops)
        );
        await t.check(
          `${direction}: hidden disabled inert and negative stops are skipped`,
          !['negative', 'disabled', 'inert', 'hidden'].some((k) => stops.includes(k)),
          JSON.stringify(stops)
        );
        await t.check(
          `${direction}: native audio buttons and sliders are reachable`,
          stops.some((k) => k.startsWith('audio:button:')) &&
            stops.some((k) => k.startsWith('audio:slider:')),
          JSON.stringify(stops)
        );
      }
      await t.check(
        'sheet Shift+Tab visits every stop in exact reverse order',
        JSON.stringify(reverse) === JSON.stringify([forward[0], ...forward.slice(1).reverse()]),
        JSON.stringify({ forward, reverse })
      );
    } finally {
      await cd.send('Accessibility.disable');
      await cd.detach();
    }
    await t.page.keyboard.press('Escape');
    await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
  },
};

async function sheetTour(t, cd, reverse) {
  const first = t.page.locator('[data-focus-key="positive2"]');
  await first.focus();
  const stops = [];
  let wrapped = false;
  for (let i = 0; i < 50; i++) {
    const state = await t.page.evaluate(() => ({
      key: document.activeElement?.dataset.focusKey,
      tag: document.activeElement?.tagName,
      inside: !!document.activeElement?.closest('.c2-sheet-host'),
    }));
    if (i && (await first.evaluate((e) => e === document.activeElement))) {
      wrapped = true;
      break;
    }
    let key = state.key;
    if (state.tag === 'IFRAME')
      key = await t.page
        .frameLocator(`[data-focus-key="${key}"]`)
        .locator('body')
        .evaluate(() => document.activeElement?.dataset.focusKey || 'frame-body');
    if (key === 'audio') {
      const tree = await cd.send('Accessibility.getFullAXTree');
      const n = tree.nodes.find(
        (n) =>
          n.role?.value !== 'RootWebArea' &&
          n.properties?.some((p) => p.name === 'focused' && p.value?.value)
      );
      key = `audio:${n?.role?.value}:${n?.name?.value}`;
    }
    if (!key)
      key = await t.page.evaluate(
        () => document.activeElement?.className || document.activeElement?.tagName
      );
    stops.push(key);
    await t.check(`${reverse ? 'reverse' : 'forward'} sheet ${i}: stays in sheet`, state.inside);
    await t.page.keyboard.press(reverse ? 'Shift+Tab' : 'Tab');
  }
  await t.check(
    `${reverse ? 'reverse' : 'forward'} sheet tour wraps`,
    wrapped,
    JSON.stringify(stops)
  );
  await t.visible(first, 'wrapped first stop is visible');
  return stops;
}

for (const opaque of [false, true]) {
  tests[
    `duo focus: reverse boundary enters ${opaque ? 'opaque' : 'same-origin'} frame at its last stop`
  ] = async (t) => {
    await status(t);
    await t.page.locator('.c2-row').first().click();
    await t.page.waitForSelector('.sheet-close');
    await t.page.evaluate((opaque) => {
      const frame = document.createElement('iframe');
      frame.id = 'boundary-frame';
      if (opaque) frame.setAttribute('sandbox', 'allow-scripts');
      frame.srcdoc = `<button id="frame-first">First</button><input id="frame-middle" aria-label="Middle"><button id="frame-last">Last</button>`;
      frame.style.cssText = 'width:100%;height:60px';
      document.querySelector('.c2-sheet-host .sheet').append(frame);
    }, opaque);
    const frame = t.page.frameLocator('#boundary-frame');
    await frame.locator('#frame-first').focus();
    // Leave the frame's remembered focus at its first control. Reverse entry must use native navigation,
    // rather than focusing the iframe host and restoring that remembered control.
    await t.page.locator('.sheet-scrim').focus();
    await t.page.keyboard.press('Shift+Tab');
    await t.focused(frame.locator('#frame-last'));
    await t.page.keyboard.press('Shift+Tab');
    await t.focused(frame.locator('#frame-middle'));
    await t.page.keyboard.press('Shift+Tab');
    await t.focused(frame.locator('#frame-first'));
    await t.page.keyboard.press('Tab');
    await t.focused(frame.locator('#frame-middle'));
    await t.page.keyboard.press('Tab');
    await t.focused(frame.locator('#frame-last'));
    await t.page.keyboard.press('Tab');
    await t.focused('.sheet-scrim');
    await t.page.locator('.sheet-close').focus();
    await t.page.keyboard.press('Escape');
    await t.page.waitForSelector('.c2-sheet-host', { state: 'detached' });
  };
}
