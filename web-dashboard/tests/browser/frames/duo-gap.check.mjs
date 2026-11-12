// Nonblocking native follow-up: capture layout/dynamic-viewport metrics after an upright-to-landscape turn.
import { writeFileSync } from 'node:fs';
import path from 'node:path';
import { fixture, open } from './duo-poll.check.mjs';

export const tests = {};
for (const side of ['right', 'left']) {
  tests[`duo gap: landscape rail ${side} keeps the pin sheet at the viewport bottom`] = async (
    t
  ) => {
    await open(t, fixture());
    await t.page.locator('[data-safe-probe]').evaluate((el, side) => {
      el.style.padding = side === 'right' ? '0px 84px 34px 0px' : '0px 0px 34px 84px';
    }, side);
    await t.page.setViewportSize({ width: 951, height: 669 });
    await t.page.waitForFunction(
      (side) => document.documentElement.dataset.railSide === side,
      side
    );
    const metrics = await t.page.evaluate(() => {
      const probe = document.createElement('i');
      probe.style.cssText =
        'position:fixed;height:100dvh;width:0;pointer-events:none;visibility:hidden';
      document.body.append(probe);
      const dvh = probe.getBoundingClientRect().height;
      probe.remove();
      return {
        inner: innerHeight,
        visual: visualViewport.height,
        client: document.documentElement.clientHeight,
        dvh,
        host: document.querySelector('.c2-sheet-host').getBoundingClientRect().toJSON(),
        sheet: document.querySelector('.c2-sheet-host .sheet').getBoundingClientRect().toJSON(),
      };
    });
    if (t.args.gapOut)
      writeFileSync(path.join(t.args.gapOut, `${side}.json`), JSON.stringify(metrics, null, 2));
    await t.check(
      'landscape sheet reaches the bottom with no empty band',
      Math.abs(metrics.sheet.bottom - metrics.inner) <= 1,
      JSON.stringify(metrics)
    );
    await t.check(
      'layout and dynamic viewport agree in this rendered control',
      metrics.client === metrics.inner &&
        metrics.dvh === metrics.inner &&
        Math.abs(metrics.host.bottom - metrics.inner) <= 1,
      JSON.stringify(metrics)
    );
    await t.shot(
      `duo-landscape-gap-${side}`,
      'Landscape pin-sheet bottom alignment; Chromium control for the separately reported native-only gap.'
    );
  };
}
