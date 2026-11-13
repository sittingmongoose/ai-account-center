// Duo rail, round 3 (owner report 2026-11-13, the real Duo on main d65fd77cb): with a Live Activity running, iOS draws
// its pill at the top of the status strip and moves the clock and signal ring down under it, and it reports none of
// that in the safe-area insets. Home, the rail's first button, sat under the ring, folded upright and open on its side.
//
// For each rail pose this lays the measured status area over the strip, with a Live Activity and without one, and
// requires every rail button's rectangle (Home first) to clear it by 8 pt (DEVICE-CHECKLIST item 2). All eight controls
// stay on screen at 44 pt or more, the rail does not scroll, and the menu buttons stay at the bottom.
//
// Measured from the owner's screenshots (share 2026-11-13-duo-round3: IMG_0498 at 2.95 px/pt, IMG_0499 at 2.10 px/pt;
// without a Live Activity, 2026-11-10-leptos-shell/device-feedback-1 IMG_0480 and DEVICE-RECIPE section 5):
//   folded upright:     the pill 26-146 pt, then the clock and ring on their glass down to 242 pt (alone: 168)
//   open on its side:   the pill 24-108 pt, then the clock and ring down to 204 pt (alone: 116)
//   folded on its side: iOS shows no status in the strip (device-feedback-2 IMG_0488), so there is nothing to clear.
// Synthetic fixtures, display-only safe= insets, the Duo's screen (466x678) so the frame detects the Duo.
//   browser-check run web-dashboard/tests/browser/frames --grep 'duo status clear'

const SCREEN = { width: 466, height: 678 };
const CLEAR = 8;
const TAP = 44;
const OPEN = { pill: [24, 108], activity: 204, clock: 116 };
const FOLDED = { pill: [26, 146], activity: 242, clock: 168 };
const POSES = [
  { name: 'open landscape, strip right', w: 951, h: 669, safe: '0,84,34,0', side: 'right', status: OPEN, shot: 'open-landscape' },
  { name: 'open landscape, strip left', w: 951, h: 669, safe: '0,0,34,84', side: 'left', status: OPEN },
  { name: 'open landscape, strip not reported yet', w: 951, h: 669, safe: '0,0,0,0', side: 'right', status: OPEN },
  { name: 'folded upright', w: 466, h: 678, safe: '0,84,34,0', side: 'right', status: FOLDED, shot: 'folded-upright' },
  { name: 'folded on its side, strip right', w: 678, h: 466, safe: '0,84,34,0', side: 'right', status: null },
  { name: 'folded on its side, strip left', w: 678, h: 466, safe: '0,0,34,84', side: 'left', status: null },
];

async function open(t, pose) {
  await t.newContext({
    viewport: { width: pose.w, height: pose.h },
    screen: SCREEN,
    isMobile: true,
    hasTouch: true,
    deviceScaleFactor: 1,
    colorScheme: 'dark',
    serviceWorkers: 'block',
  });
  await t.page.addInitScript(() => localStorage.setItem('aac-theme', 'dark'));
  await t.goto(`/app/home?e2e&safe=${pose.safe}`);
  await t.page.waitForSelector('.rail .rail-tools button');
  // The frame recomputes 120 ms and 350 ms after a change; let fonts and the last pass settle.
  await t.page.evaluate(() => document.fonts.ready);
  await t.page.waitForTimeout(450);
}

const geometry = () => {
  const rail = document.querySelector('.rail');
  const box = (e) => {
    const r = e.getBoundingClientRect();
    const name = e.getAttribute('aria-label') || e.textContent.trim();
    return { x: r.x, r: r.right, y: r.y, b: r.bottom, h: r.height, name: name.slice(0, 16) };
  };
  const shown = (e) => e.getBoundingClientRect().height > 0;
  return {
    chrome: document.documentElement.dataset.chrome,
    side: document.documentElement.dataset.railSide || '',
    reserve: parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--rail-reserve')) || 0,
    rail: box(rail),
    scroll: { top: rail.scrollTop, height: rail.scrollHeight, client: rail.clientHeight },
    tabs: [...rail.querySelectorAll(':scope > a')].filter(shown).map(box),
    tools: [...rail.querySelectorAll('.rail-tools button')].filter(shown).map(box),
    w: innerWidth,
    h: innerHeight,
  };
};

/** Whether two rectangles share any area. */
const overlaps = (a, b) => a.x < b.r && b.x < a.r && a.y < b.b && b.y < a.b;
const where = (c) => `${c.name} ${Math.round(c.x)},${Math.round(c.y)}-${Math.round(c.r)},${Math.round(c.b)}`;

/** The status area as iOS draws it over the strip, for the LOOK images only: the Live Activity's pill, then the clock
 * and signal ring on their glass, at the measured places. It takes no taps. */
const drawStatus = ({ rail, status, clear }) => {
  const layer = document.createElement('div');
  layer.dataset.kitStatus = '';
  const mid = (rail.x + rail.r) / 2;
  layer.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647;font:600 15px system-ui;color:#fff';
  const part = (css, text = '') => {
    const e = document.createElement('div');
    e.style.cssText = `position:absolute;box-sizing:border-box;display:flex;align-items:center;justify-content:center;${css}`;
    e.textContent = text;
    layer.append(e);
  };
  const [top, foot] = status.pill;
  part(`left:${mid - 20}px;width:40px;top:${top}px;height:${foot - top}px;border-radius:20px;background:#000;color:#5fe0b5;font-size:12px`, 'Live');
  part(
    `left:${mid - 26}px;width:52px;top:${foot + 4}px;height:${status.activity - foot - 4}px;border-radius:0 0 24px 24px;` +
      'background:linear-gradient(rgba(220,226,232,0),rgba(220,226,232,.55));flex-direction:column;justify-content:flex-start;padding-top:8px',
    '12:24'
  );
  part(`left:${mid - 17}px;width:34px;top:${status.activity - 44}px;height:34px;border-radius:50%;border:3px solid #fff`);
  // Where the status area ends, and the 8 pt the rail's first button must keep below it.
  part(`left:${rail.x}px;width:${rail.r - rail.x}px;top:${status.activity}px;height:${clear}px;background:rgba(255,80,80,.35)`);
  document.body.append(layer);
};

export const tests = {};

for (const pose of POSES)
  tests[`duo status clear: ${pose.name}`] = async (t) => {
    await open(t, pose);
    const g = await t.page.evaluate(geometry);
    const controls = [...g.tabs, ...g.tools];
    await t.check(
      `${pose.name}: the rail, on the strip's side, with its five tabs and three menu buttons`,
      g.chrome === 'rail' && g.side === pose.side && g.tabs.length === 5 && g.tools.length === 3,
      JSON.stringify({ chrome: g.chrome, side: g.side, tabs: g.tabs.length, tools: g.tools.length })
    );
    if (pose.status) {
      for (const [what, foot] of [
        [`a Live Activity (its pill, then the clock and ring to ${pose.status.activity} pt)`, pose.status.activity],
        [`no Live Activity (the clock and ring to ${pose.status.clock} pt)`, pose.status.clock],
      ]) {
        const area = { x: g.rail.x, r: g.rail.r, y: 0, b: foot + CLEAR };
        const first = g.tabs[0];
        await t.check(
          `${pose.name}: Home, the rail's first button, clears the status area with ${what} by ${CLEAR} pt`,
          !!first && first.name === 'Home' && !overlaps(first, area),
          first ? `${where(first)}; status area to ${foot} + ${CLEAR}` : 'no first tab'
        );
        const under = controls.filter((c) => overlaps(c, area));
        await t.check(
          `${pose.name}: no rail button sits in the status area with ${what}`,
          under.length === 0,
          under.length ? under.map(where).join('; ') : `first top ${Math.round(first?.y)}`
        );
      }
    } else {
      // The real Duo on its side shows no clock, ring or pill in its strip (IMG_0488): the rail starts near the top.
      await t.equal(`${pose.name}: no status in a sideways strip, so the rail starts 12 pt down`, g.reserve, 12);
    }
    const bottom = g.h - Math.max(Number(pose.safe.split(',')[2]), 10);
    const small = controls.filter((c) => c.h < TAP - 0.5);
    const off = controls.filter((c) => c.y < -0.5 || c.b > bottom + 0.5);
    await t.check(
      `${pose.name}: all eight controls are on screen above the home indicator, at ${TAP} pt or more`,
      controls.length === 8 && small.length === 0 && off.length === 0,
      [...small, ...off].map(where).join('; ') || `last ends at ${Math.round(controls.at(-1)?.b)} of ${bottom}`
    );
    await t.check(
      `${pose.name}: the rail fits without scrolling`,
      g.scroll.height <= g.scroll.client + 1 && g.scroll.top === 0,
      JSON.stringify(g.scroll)
    );
    const lastTab = g.tabs.at(-1);
    const lastTool = g.tools.at(-1);
    await t.check(
      `${pose.name}: the tabs come first and the menu buttons stay at the bottom`,
      !!lastTab && !!lastTool && lastTab.b <= g.tools[0].y + 0.5 && Math.abs(lastTool.b - bottom) <= 1,
      `tabs end ${Math.round(lastTab?.b)}, menu ${Math.round(g.tools[0]?.y)}-${Math.round(lastTool?.b)}, bottom ${bottom}`
    );
    if (pose.shot) {
      await t.page.evaluate(drawStatus, { rail: g.rail, status: pose.status, clear: CLEAR });
      await t.shot(
        `status-clear-${pose.shot}`,
        `${pose.name}, dark, with the owner's Live Activity drawn where iOS puts it (pill, clock and ring, red: the ` +
          `${CLEAR} pt below it): Home starts below the red band, the rail is one column, the menu buttons at the bottom.`
      );
    }
  };
