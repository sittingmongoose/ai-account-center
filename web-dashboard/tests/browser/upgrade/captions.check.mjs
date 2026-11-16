// The same synthetic Analytics data and Safari-width poses as max round 2, with actual text ranges.
export const tests = {
  async 'Analytics KPI captions stay inside dense phone and Duo cards'(t) {
    for (const pose of [
      { name: 'Duo-Safari', width: 382, height: 678, safe: '0,84,34,0', duo: true },
      { name: 'phone', width: 390, height: 844, safe: '0,0,34,0' },
      { name: 'Duo', width: 466, height: 678, safe: '0,84,34,0', duo: true },
    ]) {
      await t.newContext({ viewport: { width: pose.width, height: pose.height }, screen: pose.duo ? { width: 466, height: 678 } : undefined, isMobile: true, hasTouch: true, deviceScaleFactor: 1, serviceWorkers: 'block' });
      await t.goto('/app/analytics?e2e&safe=' + pose.safe);
      await t.page.waitForSelector('.kpi-sub');
      await t.page.evaluate(() => document.fonts.ready);
      const info = await t.page.evaluate(() => [...document.querySelectorAll('.kpi-sub')].map(caption => {
        const card = caption.closest('.kpi').getBoundingClientRect();
        const walker = document.createTreeWalker(caption, NodeFilter.SHOW_TEXT);
        const bad = [];
        while (walker.nextNode()) {
          const range = document.createRange();
          range.selectNodeContents(walker.currentNode);
          for (const box of range.getClientRects()) {
            if (box.left < card.left - 0.5 || box.right > card.right + 0.5) bad.push({ text: walker.currentNode.textContent, left: box.left, right: box.right, cardLeft: card.left, cardRight: card.right });
          }
        }
        return { text: caption.textContent, bad };
      }));
      await t.check(pose.name + ': both reported captions are present', info.some(c => c.text.includes('API-equivalent estimate')) && info.some(c => c.text.includes('94M uncached input tokens')), JSON.stringify(info));
      await t.check(pose.name + ': every caption stays inside its card', info.length > 0 && info.every(c => c.bad.length === 0), JSON.stringify(info));
      await t.shot('captions-' + pose.name, 'Synthetic Analytics captions at ' + pose.width + ' CSS pixels, no clipping or hidden text.');
    }
  },
};
