// Standalone metadata and painted status surfaces. Chromium does not implement iOS installation geometry;
// safe= supplies the measured inset, while t.pixels reads the actual GPU screenshot behind white OS glyphs.
const poses = [
  {
    name: 'Duo portrait',
    w: 669,
    h: 951,
    safe: '82,0,34,0',
    screen: { width: 466, height: 678 },
    chrome: 'band',
    x: 639,
    y: 20,
    top: 82,
  },
  {
    name: 'Duo retained portrait',
    w: 669,
    h: 869,
    safe: '82,0,34,0',
    screen: { width: 466, height: 678 },
    chrome: 'band',
    x: 639,
    y: 20,
    top: 82,
  },
  {
    name: 'Duo landscape',
    w: 951,
    h: 669,
    safe: '0,84,34,0',
    screen: { width: 466, height: 678 },
    chrome: 'rail',
    x: 921,
    y: 30,
    top: 216,
  },
  {
    name: 'Duo folded',
    w: 466,
    h: 678,
    safe: '0,84,34,0',
    screen: { width: 466, height: 678 },
    chrome: 'rail',
    x: 436,
    y: 90,
    top: 252,
  },
  {
    name: 'iPhone 17 Pro',
    w: 402,
    h: 874,
    safe: '62,0,34,0',
    screen: { width: 402, height: 874 },
    chrome: 'phone',
    x: 20,
    y: 20,
    top: 62,
  },
  {
    name: 'desktop with status inset',
    w: 1440,
    h: 1000,
    safe: '24,0,0,0',
    screen: { width: 1440, height: 1000 },
    chrome: 'desk',
    x: 20,
    y: 10,
    top: 24,
  },
];
const lum = (rgb) =>
  rgb
    .slice(0, 3)
    .map((v) => {
      v /= 255;
      return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
    })
    .reduce((s, v, i) => s + v * [0.2126, 0.7152, 0.0722][i], 0);
const contrast = (a, b) => {
  const x = lum(a),
    y = lum(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
};
async function darkPixels(t, p, label) {
  const image = await t.pixels({ x: p.x, y: p.y, width: 6, height: 6 }, { scroll: false });
  let min = Infinity;
  for (let i = 0; i < image.data.length; i += 4)
    min = Math.min(min, contrast([255, 255, 255], Array.from(image.data.subarray(i, i + 3))));
  await t.check(
    `${label}: white status glyph contrast is at least 7:1 on painted pixels`,
    min >= 7,
    `minimum ${min.toFixed(2)}:1`
  );
}
async function context(t, p, theme) {
  await t.newContext({
    viewport: { width: p.w, height: p.h },
    screen: p.screen,
    deviceScaleFactor: 1,
    isMobile: p.chrome !== 'desk',
    hasTouch: p.chrome !== 'desk',
    colorScheme: theme === 'light' ? 'dark' : 'light',
    serviceWorkers: 'block',
  });
  await t.page.addInitScript((theme) => {
    localStorage.setItem('aac-theme', theme);
  }, theme);
}
export const tests = {};
for (const p of poses)
  for (const theme of ['light', 'dark']) {
    tests[`standalone status: ${p.name} ${theme}`] = async (t) => {
      await context(t, p, theme);
      await t.goto(`/app/status?safe=${p.safe}`);
      await t.page.waitForSelector('.c2-row');
      await t.equal(
        'entry requests translucent standalone status',
        await t.page.evaluate(
          () =>
            document.querySelector('meta[name=\"apple-mobile-web-app-status-bar-style\"]')
              ?.content || null
        ),
        'black-translucent'
      );
      await t.equal(
        'measured pose keeps the expected chrome',
        await t.page.getAttribute('html', 'data-chrome'),
        p.chrome
      );
      const row =
        p.chrome === 'phone'
          ? '.topbar-in'
          : p.chrome === 'band'
            ? '.band-tabs'
            : p.chrome === 'rail'
              ? '.rail a'
              : '.chrome-in';
      const y = await t.page
        .locator(row)
        .first()
        .evaluate((e) => e.getBoundingClientRect().top);
      await t.check(
        'interactive chrome clears the safe top or rail reserve',
        y >= p.top - 0.5,
        `${y} >= ${p.top}`
      );
      await darkPixels(t, p, 'page');
      if (p.chrome === 'band') {
        const colors = await t.page.locator('.band-info').evaluate((el) => {
          const color = (e) =>
            getComputedStyle(e)
              .color.match(/[\d.]+/g)
              .slice(0, 3)
              .map(Number);
          return [...el.querySelectorAll('.bi1,.bi1 b,.bi2,.bi2 b')].map(color);
        });
        await t.check(
          'page line and summary keep text contrast on the dark band',
          colors.every((c) => contrast(c, [12, 18, 23]) >= 4.5),
          JSON.stringify(colors)
        );
      }
      if (
        (theme === 'light' &&
          ['Duo portrait', 'Duo landscape', 'iPhone 17 Pro'].includes(p.name)) ||
        (theme === 'dark' && ['Duo portrait', 'iPhone 17 Pro'].includes(p.name))
      )
        await t.shot(
          `status-${p.name.replaceAll(' ', '-').toLowerCase()}-${theme}`,
          'Dark system-status surface in the light theme; controls remain outside the reserved area.'
        );
      if (theme === 'light' && ['Duo portrait', 'iPhone 17 Pro'].includes(p.name)) {
        await t.page.locator('.c2-row').first().click();
        await t.page.waitForSelector('.sheet-close');
        await darkPixels(t, p, 'open pin sheet');
        await t.page.keyboard.press('Escape');
      }
    };
  }
for (const theme of ['light', 'dark']) {
  tests[`standalone status: start and offline first paint ${theme}`] = async (t) => {
    const p = poses.find((p) => p.name === 'iPhone 17 Pro');
    await t.newContext({
      viewport: { width: p.w, height: p.h },
      screen: p.screen,
      deviceScaleFactor: 1,
      isMobile: true,
      hasTouch: true,
      colorScheme: theme,
      serviceWorkers: 'block',
    });
    await t.page.addInitScript(() => {
      const set = () => document.documentElement.style.setProperty('--safe-top', '62px');
      if (document.documentElement) set();
      else document.addEventListener('DOMContentLoaded', set, { once: true });
    });
    // The start page is the frame's page: it reads its measured inset from safe=, as on the other poses; the offline
    // cover is the service worker's own page, which paints from the strip's --safe-top above.
    for (const page of ['start-cover', 'offline-cover']) {
      await t.goto('/__fixture/' + page + (page === 'start-cover' ? `?safe=${p.safe}` : ''));
      await t.equal(
        `${page}: translucent install metadata`,
        await t.page.evaluate(
          () =>
            document.querySelector('meta[name=\"apple-mobile-web-app-status-bar-style\"]')
              ?.content || null
        ),
        'black-translucent'
      );
      await darkPixels(t, p, page);
    }
  };
}
tests['standalone status: embedded content does not paint a second status surface'] = async (t) => {
  await context(t, poses[0], 'light');
  await t.goto('/app/status?embed=1&safe=82,0,34,0');
  await t.page.waitForSelector('.c2-row');
  await t.equal(
    'embedded chrome stays off',
    await t.page.getAttribute('html', 'data-chrome'),
    'none'
  );
  const hidden = await t.page.evaluate(() =>
    ['::before', '::after'].every((p) => {
      const s = getComputedStyle(document.body, p);
      return s.display === 'none' || s.content === 'none';
    })
  );
  await t.check('both status surfaces are absent inside the shell iframe', hidden);
};

// A failed WASM start must not leave its only message behind the newly overlaid system strip.
tests['standalone status: bootstrap failure keeps its message below the strip'] = async (t) => {
  const p = poses.find((p) => p.name === 'iPhone 17 Pro');
  await context(t, p, 'light');
  await t.page.addInitScript(() =>
    document.addEventListener(
      'DOMContentLoaded',
      () => document.documentElement.style.setProperty('--safe-top', '62px'),
      { once: true }
    )
  );
  await t.page.route('**/app/pkg/aac_web.js', (r) =>
    r.fulfill({
      body: 'export default async function(){throw new Error("fixture start failure")}',
      contentType: 'text/javascript',
    })
  );
  await t.goto('/app/status');
  await t.page.waitForFunction(() =>
    document.body.textContent.includes('This page could not start')
  );
  const y = await t.page.evaluate(() => {
    const range = document.createRange();
    range.selectNodeContents(document.querySelector('.start-message') || document.body);
    return range.getBoundingClientRect().top;
  });
  await t.check('bootstrap error text clears the status strip', y >= 62, `${y} >= 62`);
  await darkPixels(t, p, 'bootstrap failure');
};
