// The same synthetic Analytics data and Safari-width poses as max round 2, with actual text ranges,
// served through the native Rust dashboard server with real test sign-in and synthetic Analytics payload.
import { analyticsFixture } from '../device/analytics-fixture.mjs';
import { homeAccounts } from '../home-fixture.mjs';
import { usageView } from '../../../public/analytics-usage.mjs';

// The captions are what this build reports for the synthetic Analytics payload (one window ending 2026-11-16T02:00Z),
// read here through the page's own model and default page state: expected text follows the fixture, never frozen text.
const PAYLOAD = analyticsFixture({ range: '7d', now: Date.parse('2026-11-16T02:00:00Z'), accounts: homeAccounts().accounts });
const PAGE = { range: '7d', from: null, to: null, prov: 'all', split: false, cache: false, donut: 'tokens', heat: 'cost', cbmSort: 'cost' };
const HOUR = 3_600_000;
/** The KPI sub-line for `key` at `now`, as the page's Analytics model words it. */
const captionAt = (now, key) => usageView(PAYLOAD, PAGE, { now }).kpis.find((kpi) => kpi.key === key).sub.map((run) => run.text).join('');

export const tests = {
  async 'Analytics KPI captions stay inside dense phone and Duo cards'(t) {
    for (const pose of [
      { name: 'Duo-Safari', width: 382, height: 678, safe: '0,84,34,0', duo: true },
      { name: 'phone', width: 390, height: 844, safe: '0,0,34,0' },
      { name: 'Duo', width: 466, height: 678, safe: '0,84,34,0', duo: true },
    ]) {
      await t.newContext({
        viewport: { width: pose.width, height: pose.height },
        screen: pose.duo ? { width: 466, height: 678 } : undefined,
        isMobile: true,
        hasTouch: true,
        deviceScaleFactor: 1,
        serviceWorkers: 'block',
      });
      await t.goto('/__upgrade/ready');
      await t.app.signIn();
      await t.app.control({ uiVersion: 'current', oldWorker: false, delayClassicMs: 0, failPath: null });
      await t.page.route((u) => new URL(u).pathname === '/api/accounts/analytics', (r) => r.fulfill({ json: PAYLOAD }));
      await t.goto('/app/analytics?e2e&safe=' + pose.safe);
      await t.page.waitForSelector('.kpi-sub');
      await t.page.evaluate(() => document.fonts.ready);
      const info = await t.page.evaluate(() =>
        [...document.querySelectorAll('.kpi-sub')].map((caption) => {
          const card = caption.closest('.kpi').getBoundingClientRect();
          const walker = document.createTreeWalker(caption, NodeFilter.SHOW_TEXT);
          const bad = [];
          while (walker.nextNode()) {
            const range = document.createRange();
            range.selectNodeContents(walker.currentNode);
            for (const box of range.getClientRects()) {
              if (box.left < card.left - 0.5 || box.right > card.right + 0.5) {
                bad.push({ text: walker.currentNode.textContent, left: box.left, right: box.right, cardLeft: card.left, cardRight: card.right });
              }
            }
          }
          return { text: caption.textContent, bad };
        })
      );
      // The input total steps on the hour (the range's start moves with the clock), so the hour before this read is
      // accepted too: a page rendered a moment before a boundary reported the older one. The cost line moves by the
      // minute for the same reason, so only its wording is asserted.
      const now = await t.page.evaluate(() => Date.now());
      const inputs = [captionAt(now, 'in'), captionAt(now - HOUR, 'in')].filter((text) => text.length > 0);
      await t.check(
        pose.name + ': both reported captions are present',
        inputs.some((text) => info.some((c) => c.text.includes(text))) && info.some((c) => c.text.includes('API-equivalent estimate')),
        JSON.stringify({ expected: { input: inputs, cost: 'API-equivalent estimate' }, info })
      );
      await t.check(pose.name + ': every caption stays inside its card', info.length > 0 && info.every((c) => c.bad.length === 0), JSON.stringify(info));
      await t.shot('captions-' + pose.name, 'Synthetic Analytics captions at ' + pose.width + ' CSS pixels, no clipping or hidden text.');
    }
  },
};
