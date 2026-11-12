// Duo polish 2 (owner report 2, 2026-11-12, the real Duo on main eb5b96a2b). Synthetic fixtures; display-only safe=
// insets; the measured Duo screen (466x678) so the frame detects the Duo.
//   1. The landscape rail, folded and open, strip on either side, reads as one column from the top edge to the bottom
//      edge: the clock and signal ring sit on the rail's own surface, with no darker block above it and no edge where
//      the rail "starts", also under an open sheet's scrim. Controls still start below the strip's reserve.
//   2. Open upright (band), every page's line in the band clears the screen's rounded top corners (about 55 pt) by
//      12 pt (DEVICE-CHECKLIST.md), however long the page's summary is, with no extra gap above it.
// browser-check run web-dashboard/tests/browser/frames --grep 'duo polish'
import { readFileSync } from 'node:fs';

const SCREEN = { width: 466, height: 678 };
const RADIUS = 55;
const MARGIN = 12;
const RAIL_POSES = [
  { name: 'open landscape, strip right', w: 951, h: 669, safe: '0,84,34,0', side: 'right', reserve: 136 },
  { name: 'open landscape, strip left', w: 951, h: 669, safe: '0,0,34,84', side: 'left', reserve: 136 },
  { name: 'folded upright', w: 466, h: 678, safe: '0,84,34,0', side: 'right', reserve: 184 },
  { name: 'folded on its side, strip right', w: 678, h: 466, safe: '0,84,34,0', side: 'right', reserve: 12 },
  { name: 'folded on its side, strip left', w: 678, h: 466, safe: '0,0,34,84', side: 'left', reserve: 12 },
];
const BAND_POSES = [
  { name: 'open upright', w: 669, h: 951, safe: '82,0,34,0' },
  { name: 'open upright, retained 869 window', w: 669, h: 869, safe: '82,0,34,0' },
];
const PAGES = { status: '.c2-row', scoreboard: '.picks', home: '.shell', analytics: '.shell', accounts: '.shell' };

// The owner's real Status summary shape (three lines in the band at 669 pt), with made-up thread names.
function longStatusView() {
  const view = JSON.parse(
    readFileSync(new URL('../../../leptos/fixtures/status-design-view.json', import.meta.url))
  );
  view.headline.runs = [
    { text: '2 questions', strong: true, tone: 'warn' },
    { text: ' for you · ' },
    { text: '2 pinned threads are looping', strong: true, tone: 'crit' },
    { text: ' (Improve Example Page Readability, task X-BATCH2) · ' },
    { text: '3 pinned threads wait on you', strong: true, tone: 'warn' },
    { text: ' (Build Example Concept and Background Activity) · 51 of 83 items landed · ' },
    { text: '3 blocked', strong: true, tone: 'warn' },
    { text: ' · ' },
    { text: '1 source not current', strong: true, tone: 'warn' },
  ];
  return view;
}

async function open(t, pose, theme, page = 'status', view = null) {
  await t.newContext({
    viewport: { width: pose.w, height: pose.h },
    screen: SCREEN,
    isMobile: true,
    hasTouch: true,
    deviceScaleFactor: 1,
    colorScheme: theme,
    serviceWorkers: 'block',
  });
  await t.page.addInitScript((theme) => localStorage.setItem('aac-theme', theme), theme);
  if (view)
    await t.page.route(
      (u) => new URL(u).pathname === '/api/status',
      (r) => r.fulfill({ json: view })
    );
  await t.goto(`/app/${page}?e2e&safe=${pose.safe}`);
  await t.page.waitForSelector(PAGES[page]);
  // The frame recomputes 120 ms and 350 ms after a change; let fonts and the last pass settle.
  await t.page.evaluate(() => document.fonts.ready);
  await t.page.waitForTimeout(450);
}

const lum = (rgb) =>
  rgb
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
const hex = (c) => '#' + c.map((v) => v.toString(16).padStart(2, '0')).join('');

/** The painted colour of each row of a one-pixel column, top to bottom. */
async function column(t, x, height) {
  const img = await t.pixels({ x, y: 0, width: 1, height }, { scroll: false });
  const rows = [];
  for (let y = 0; y < img.height; y++) {
    const i = y * img.width * 4;
    rows.push([img.data[i], img.data[i + 1], img.data[i + 2]]);
  }
  return rows;
}

/** One colour from the top edge to the bottom edge: the first row that differs from the top row, if any. */
function seam(rows, tolerance = 3) {
  const top = rows[0];
  const y = rows.findIndex((c) => c.some((v, k) => Math.abs(v - top[k]) > tolerance));
  return y < 0 ? null : { y, above: hex(rows[y - 1]), below: hex(rows[y]) };
}

/** No hard edge: the first row that jumps from the row above it. An open sheet's soft shadow falls on the rail, so
 * under a scrim the column may shade gradually but must not step. */
function step(rows, tolerance = 3) {
  const y = rows.findIndex((c, i) => i > 0 && c.some((v, k) => Math.abs(v - rows[i - 1][k]) > tolerance));
  return y < 0 ? null : { y, above: hex(rows[y - 1]), below: hex(rows[y]) };
}

const railGeometry = () => {
  const rail = document.querySelector('.rail').getBoundingClientRect();
  const css = getComputedStyle(document.documentElement);
  return {
    chrome: document.documentElement.dataset.chrome,
    side: document.documentElement.dataset.railSide || '',
    rail: { x: rail.x, r: rail.right, y: rail.y, b: rail.bottom, w: rail.width },
    reserve: parseFloat(css.getPropertyValue('--rail-reserve')) || 0,
    controls: [...document.querySelectorAll('.rail a, .rail button')]
      .map((e) => e.getBoundingClientRect())
      .filter((r) => r.height)
      .map((r) => ({ x: r.x, r: r.right, y: r.y })),
    h: innerHeight,
  };
};

async function railColumns(t, label, g, scrim = false) {
  // Inside the rail, clear of its controls (60 pt wide, centred) and of its edge line.
  const inner = g.side === 'left' ? Math.floor(g.rail.r) - 6 : Math.ceil(g.rail.x) + 5;
  // The rail's edge line toward the page.
  const edge = g.side === 'left' ? Math.ceil(g.rail.r) - 1 : Math.floor(g.rail.x);
  const surface = await column(t, inner, g.h);
  const line = await column(t, edge, g.h);
  const find = scrim ? step : seam;
  const s = find(surface);
  await t.check(
    scrim
      ? `${label}: the rail dims as one column, with no edge where its top meets the rest`
      : `${label}: the rail's surface is one colour from the top edge to the bottom edge`,
    !s,
    s ? `at y=${s.y} ${s.above} turns ${s.below} (x=${inner})` : `${hex(surface[0])} at x=${inner}`
  );
  const l = find(line);
  await t.check(
    `${label}: the rail's edge line runs unbroken from the top edge`,
    !l,
    l ? `at y=${l.y} ${l.above} turns ${l.below} (x=${edge})` : `${hex(line[0])} at x=${edge}`
  );
  return surface;
}

export const tests = {};

for (const pose of RAIL_POSES)
  for (const theme of ['dark', 'light'])
    tests[`duo polish: rail ${pose.name}, ${theme}, is one column`] = async (t) => {
      await open(t, pose, theme);
      const g = await t.page.evaluate(railGeometry);
      await t.check(
        `${pose.name}: the rail is on the strip's side`,
        g.chrome === 'rail' && g.side === pose.side,
        JSON.stringify({ chrome: g.chrome, side: g.side })
      );
      await t.check(
        `${pose.name}: the rail runs from the top edge to the bottom edge`,
        Math.abs(g.rail.y) < 0.5 && Math.abs(g.rail.b - g.h) < 0.5,
        JSON.stringify(g.rail)
      );
      await t.check(
        `${pose.name}: every rail control starts below the strip's ${pose.reserve} pt reserve`,
        g.reserve === pose.reserve && g.controls.length >= 8 && g.controls.every((c) => c.y >= pose.reserve - 0.5),
        JSON.stringify({ reserve: g.reserve, tops: g.controls.map((c) => Math.round(c.y)) })
      );
      const surface = await railColumns(t, `${pose.name} ${theme}`, g);
      // The clock and signal ring are white in either theme (black-translucent): their part of the rail stays dark.
      const top = surface.slice(2, Math.max(4, pose.reserve - 2));
      const min = Math.min(...top.map((c) => contrast([255, 255, 255], c)));
      await t.check(
        `${pose.name} ${theme}: white status glyphs keep 7:1 on the rail's top`,
        min >= 7,
        `minimum ${min.toFixed(2)}:1`
      );
      // Under a sheet's scrim the whole rail dims together: no undimmed block left at its top.
      await t.page.locator('.c2-row').first().click();
      await t.page.waitForSelector('.sheet-close');
      await t.page.waitForTimeout(400);
      const under = await railColumns(t, `${pose.name} ${theme} under an open sheet`, g, true);
      const dim = Math.min(...under.slice(2, Math.max(4, pose.reserve - 2)).map((c) => contrast([255, 255, 255], c)));
      await t.check(
        `${pose.name} ${theme} under an open sheet: white status glyphs keep 7:1`,
        dim >= 7,
        `minimum ${dim.toFixed(2)}:1`
      );
      await t.page.keyboard.press('Escape');
      if (theme === 'dark' && pose.side === 'right' && pose.w > 900)
        await t.shot(
          'rail-open-landscape-right-dark',
          'Open landscape, strip right (the owner pose): the rail is one column from the top edge; the 136 pt above Home is the rail itself.'
        );
      if (theme === 'light' && pose.side === 'left' && pose.w > 900)
        await t.shot(
          'rail-open-landscape-left-light',
          'Open landscape, strip left, light theme: the rail is one dark column (white status glyphs), mirrored to the left.'
        );
    };

/** Every line of text in the band: its box, against the screen's rounded top corners. */
const bandLines = () => {
  const info = document.querySelector('.band-info');
  const tabs = document.querySelector('.band-tabs').getBoundingClientRect();
  const head = document.querySelector('.band-head').getBoundingClientRect();
  const lines = [];
  const walker = document.createTreeWalker(info, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) {
    const n = walker.currentNode;
    if (!n.textContent.trim()) continue;
    const range = document.createRange();
    range.selectNodeContents(n);
    for (const r of range.getClientRects())
      if (r.width > 0 && r.height > 0)
        lines.push({ x: r.x, r: r.right, y: r.y, b: r.bottom, text: n.textContent.trim().slice(0, 24) });
  }
  const box = info.getBoundingClientRect();
  return {
    chrome: document.documentElement.dataset.chrome,
    w: innerWidth,
    lines,
    info: { y: box.y, b: box.bottom, h: box.height },
    tabs: { y: tabs.y, b: tabs.bottom },
    head: { b: head.bottom },
    summaryLines: new Set(lines.filter((l) => l.y > lines[0].b - 1).map((l) => Math.round(l.y))).size,
    toasts: document.querySelector('.toasts').getBoundingClientRect().top,
  };
};

/** Whether a point dx, dy from a screen corner is `margin` clear of that rounded corner (frame.check.mjs). */
const cornerClear = (dx, dy) =>
  dx >= RADIUS || dy >= RADIUS || Math.hypot(RADIUS - dx, RADIUS - dy) <= RADIUS - MARGIN;

for (const pose of BAND_POSES)
  for (const [page, long] of [
    ['status', true],
    ['status', false],
    ['scoreboard', false],
    ['home', false],
    ['analytics', false],
    ['accounts', false],
  ])
    tests[`duo polish: band ${pose.name}, ${page}${long ? ' (owner-length summary)' : ''} clears the corners`] =
      async (t) => {
        await open(t, pose, 'dark', page, long ? longStatusView() : null);
        const s = await t.page.evaluate(bandLines);
        const label = `${pose.name} ${page}${long ? ' long' : ''}`;
        await t.equal(`${label}: band chrome`, s.chrome, 'band');
        await t.check(`${label}: the band shows the page line`, s.lines.length >= 1, JSON.stringify(s.lines));
        const bad = s.lines.filter(
          (l) => l.y < 0 || !cornerClear(l.x, l.y) || !cornerClear(s.w - l.r, l.y)
        );
        await t.check(
          `${label}: no line of the band within ${MARGIN} pt of a rounded top corner or above the top edge`,
          bad.length === 0,
          JSON.stringify(bad.length ? bad : s.lines.slice(0, 2))
        );
        // The band is the 82 pt strip; it grows only by what a long summary needs below the corner's clearance.
        if (s.info.h > 82.5)
          await t.check(
            `${label}: dense: a taller band starts its page line where the corner allows, no lower`,
            s.lines[0].y <= 34,
            `first line top ${s.lines[0].y.toFixed(1)}, band ${s.info.h.toFixed(1)}`
          );
        await t.check(
          `${label}: the band's text ends above the tab row`,
          s.lines.every((l) => l.b <= s.tabs.y + 0.5) && s.info.b <= s.tabs.y + 0.5,
          JSON.stringify({ info: s.info, tabs: s.tabs })
        );
        if (s.summaryLines <= 1)
          await t.check(
            `${label}: a one-line summary keeps the band at the 82 pt strip`,
            s.info.h <= 82.5,
            JSON.stringify(s.info)
          );
        await t.check(
          `${label}: toasts start below the band`,
          s.toasts >= s.head.b - 0.5,
          JSON.stringify({ toasts: s.toasts, head: s.head })
        );
        await t.page.locator('.band-tools .icon-btn[aria-label="Account"]').click();
        await t.page.waitForSelector('.acct-menu');
        const menu = await t.page.evaluate(() => document.querySelector('.acct-menu').getBoundingClientRect().top);
        await t.check(
          `${label}: the account menu opens below the band, not over its button`,
          menu >= s.head.b - 0.5,
          JSON.stringify({ menu, head: s.head })
        );
        await t.page.keyboard.press('Escape');
        if (page === 'status' && long && pose.h === 951)
          await t.shot(
            'band-status-long-summary',
            'Open upright, Status with the owner-length summary: the page line sits clear of the rounded top-left corner; the band grows down, the tabs follow.'
          );
        if (page === 'scoreboard' && pose.h === 951)
          await t.shot('band-scoreboard', 'Open upright, Scoreboard: one-line summary, the band stays 82 pt.');
      };
