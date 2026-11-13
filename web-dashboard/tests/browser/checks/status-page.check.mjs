// The Status page (Leptos, /app/status) served by this checkout's dist/ui: panels, the question deck, the media viewer
// and the review-and-send flow, on desktop and on a phone-sized touch screen.
// Synthetic data only: GET /api/status answers with web-dashboard/leptos/fixtures/*.json (made-up threads), card files
// come from fixtures/media, and POST /api/status/answers is stubbed in the page, so a test tap never reaches an answer
// store, real or temporary. The stub keeps what the page sent so the check can assert on it.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', name), 'utf8'));
const TYPES = {
  '.jpg': 'image/jpeg',
  '.png': 'image/png',
  '.mp4': 'video/mp4',
  '.webp': 'image/webp',
};
// iPhone 17 Pro's CSS viewport. Touch and DPR 2; WebKit itself is only tested on the simulator.
const PHONE = {
  viewport: { width: 402, height: 874 },
  isMobile: true,
  hasTouch: true,
  deviceScaleFactor: 2,
};
const FORMATS = [
  { name: 'desktop', w: 1440, h: 900, safe: '0,0,0,0', mobile: false, chrome: 'desk' },
  // The installed app's real poses (review U7): no top inset on the iPhone or the open Duo, the folded Duo turned.
  { name: 'iphone', w: 402, h: 812, safe: '0,0,34,0', mobile: true, chrome: 'phone' },
  { name: 'duo-folded', w: 466, h: 678, safe: '0,84,34,0', mobile: true, chrome: 'rail' },
  { name: 'duo-folded-turned', w: 678, h: 466, safe: '0,84,34,0', mobile: true, chrome: 'rail' },
  { name: 'duo-open-upright', w: 669, h: 869, safe: '0,0,34,0', mobile: true, chrome: 'phone' },
  { name: 'duo-portrait', w: 669, h: 951, safe: '82,0,34,0', mobile: true, chrome: 'band' },
  { name: 'duo-landscape', w: 951, h: 669, safe: '0,84,34,0', mobile: true, chrome: 'rail' },
  // The Duo open on its side while iOS reports no strip (a rotated open app): still the rail (owner, 2026-11-10).
  { name: 'open-window', w: 951, h: 669, safe: '0,0,0,0', mobile: true, chrome: 'rail' },
];

// Every status call the page makes, answered here. Returns the answer sets the page sent.
async function stubStatus(t, view) {
  const sent = [];
  await t.page.route(
    (u) => new URL(u).pathname === '/api/status',
    (r) => r.fulfill({ json: view })
  );
  await t.page.route(
    (u) => new URL(u).pathname === '/api/status/answers',
    async (r) => {
      const body = JSON.parse(r.request().postData() || '{}');
      sent.push(body);
      const results = (body.answers || []).map((a) => ({ cardId: a.cardId, outcome: 'recorded' }));
      await r.fulfill({ json: { results } });
    }
  );
  await stubMedia(t);
  // Things to open: the read-only deliverable route, stubbed (a fixture image for pictures, a page otherwise).
  // On the context, so the tab an Open button opens is served too.
  await t.page.context().route(
    (u) => new URL(u).pathname === '/api/status/deliverable',
    (r) => {
      const wanted = new URL(r.request().url()).searchParams.get('path') || '';
      return /\.(png|jpe?g|webp)$/i.test(wanted)
        ? r.fulfill({ contentType: 'image/jpeg', body: fs.readFileSync(path.join(LEPTOS, 'fixtures', 'media', 'option-a.jpg')) })
        : r.fulfill({ contentType: 'text/html', body: '<h1>Deliverable</h1>' });
    }
  );
  return sent;
}

// Card files from the fixture's media folder (with byte ranges for video).
async function stubMedia(t) {
  // Card files: the fixture's media by file name (option-a.jpg, option-b.mp4, ...), with byte ranges for video. A
  // file the fixture folder doesn't have (the server view's writer images) gets a stand-in of the same kind.
  await t.page.route(
    (u) => new URL(u).pathname.startsWith('/api/status/files/'),
    async (r) => {
      let name = path.basename(new URL(r.request().url()).pathname);
      if (!fs.existsSync(path.join(LEPTOS, 'fixtures', 'media', name)))
        name = /\.(mp4|webm)$/i.test(name) ? 'option-b.mp4' : 'option-a.jpg';
      const file = path.join(LEPTOS, 'fixtures', 'media', name);
      const body = fs.readFileSync(file);
      const type = TYPES[path.extname(name)] || 'application/octet-stream';
      const range = /bytes=(\d+)-(\d*)/.exec(r.request().headers().range || '');
      if (!range)
        return r.fulfill({
          status: 200,
          body,
          headers: { 'content-type': type, 'accept-ranges': 'bytes' },
        });
      const start = Number(range[1]);
      const end = range[2] ? Math.min(Number(range[2]), body.length - 1) : body.length - 1;
      await r.fulfill({
        status: 206,
        body: body.subarray(start, end + 1),
        headers: {
          'content-type': type,
          'accept-ranges': 'bytes',
          'content-range': `bytes ${start}-${end}/${body.length}`,
        },
      });
    }
  );
}

async function openStatus(t, view, query = '') {
  await t.app.signIn();
  const sent = await stubStatus(t, view);
  await t.goto(`/app/status${query}`, { settleMs: 300 });
  await t.page.waitForSelector('#cards');
  return sent;
}

const SECTIONS = [
  '.sec-questions',
  '.sec-pinned',
  '.sec-machines',
  '.sec-phones',
  '.sec-spend',
];
// The deck's "Next" (the last card's reads Review).
const NEXT = '.q-send > button:nth-of-type(2)';
const CARD = 'article.q-card';

// Signed answers the server wrote to the kit's temporary reports folder (never the share).
function signedAnswers(t) {
  const dir = path.join(t.state.reports, 'v1', 'answers');
  return fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith('.json')) : [];
}


// The summary strip's widths (owner, 2026-11-12): iPhone, Duo folded, Duo open upright (top bar, then the status band),
// Duo open on its side, desktop.
const STRIP_FORMATS = [
  { name: 'phone-390', w: 390, h: 844, safe: '0,0,34,0', mobile: true, chrome: 'phone' },
  { name: 'duo-folded-466', w: 466, h: 678, safe: '0,84,34,0', mobile: true, chrome: 'rail' },
  { name: 'duo-upright-669', w: 669, h: 869, safe: '0,0,34,0', mobile: true, chrome: 'phone' },
  { name: 'duo-band-669', w: 669, h: 951, safe: '82,0,34,0', mobile: true, chrome: 'band' },
  { name: 'duo-landscape-951', w: 951, h: 669, safe: '0,84,34,0', mobile: true, chrome: 'rail' },
  { name: 'desktop-1440', w: 1440, h: 900, safe: '0,0,0,0', mobile: false, chrome: 'desk' },
  // Narrower than any installed pose: the folded Duo's Safari tab (382 wide with the rail), counts in two rows of three.
  { name: 'duo-safari-382', w: 382, h: 580, safe: '0,84,34,0', mobile: true, chrome: 'rail', maxRows: 3, screen: { width: 466, height: 678 } },
];

// The owner's case of 2026-11-12 (2 questions, 2 looping, 3 waiting on you, 3 blocked, 51 of 83, 1 source not
// current) with a long synthetic title, shaped as status-view-strip.ts writes it.
function stripView() {
  const view = { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json') };
  const long = 'Make every Project Management panel readable at a glance on phones and the Duo';
  const first = view.pinned.updates[0];
  view.pinned = {
    ...view.pinned,
    updates: [
      { ...first, id: 'pin-long', title: long, looping: { answer: 'yes', text: 'Yes. P-BATCH2 has gone back for fixes 4 times.', task: 'P-BATCH2', rounds: 4 } },
      ...view.pinned.updates,
    ],
  };
  view.headline = {
    ...view.headline,
    strip: {
      items: [
        { key: 'questions', value: '2', label: 'questions for you', short: 'questions', tone: 'warn', section: 'questions', detail: '2 questions for you.' },
        { key: 'looping', value: '2', label: 'threads looping', short: 'looping', tone: 'bad', section: 'pinned', detail: `2 pinned threads are looping: ${long} (task P-BATCH2), Build PMConcept7 and Background Activity (task F-07).` },
        { key: 'waiting', value: '3', label: 'waiting on you', short: 'wait on you', tone: 'warn', section: 'pinned', detail: '3 pinned threads wait on you: Build PMConcept7 and Background Activity, Build two new tabs in AI Account Center, Tool improvements 1-12.' },
        { key: 'blocked', value: '3', label: 'blocked', short: 'blocked', tone: 'warn', section: null, detail: 'Blocked: F-07 Tray icon sharp at 125% scale, B-3 Pick the reviewers, C-1 Disk floor.' },
        { key: 'done', value: '51/83', label: 'items done', short: 'done', tone: null, section: null, detail: '51 of 83 items landed or done (dropped items are not counted).' },
        { key: 'sources', value: '1', label: 'source not current', short: 'not current', tone: 'warn', section: null, detail: 'Not current: Agent reports: 1 file unreadable since 5:17 PM.' },
      ],
      worst: { state: 'looping', word: 'Looping', tone: 'bad', threadId: 'pin-long', title: long, task: 'P-BATCH2', detail: 'Yes. P-BATCH2 has gone back for fixes 4 times.' },
    },
  };
  return view;
}

// What the strip draws: each visible count with its number, its shown label and their boxes, the worst thread's line,
// and how many rows they take.
async function stripLayout(t) {
  return t.page.evaluate(() => {
    const shown = (el) => !!el && el.getClientRects().length > 0;
    const box = (el) => {
      const r = el.getBoundingClientRect();
      return { x: r.x, y: r.y, w: r.width, h: r.height, top: r.top, bottom: r.bottom, left: r.left, right: r.right };
    };
    const lineH = (el) => parseFloat(getComputedStyle(el).lineHeight) || parseFloat(getComputedStyle(el).fontSize) * 1.3;
    const items = [...document.querySelectorAll('.st-head .sum-i')].filter(shown).map((el) => {
      const n = el.querySelector('.sum-n');
      const label = [...el.querySelectorAll('.sum-l > span')].find(shown);
      return {
        key: el.dataset.key,
        value: n.textContent,
        label: label ? label.textContent : '',
        box: box(el),
        n: box(n),
        l: label ? box(label) : null,
        labelOneLine: !!label && label.getClientRects().length === 1 && label.getBoundingClientRect().height < lineH(label) * 1.5,
        cut: el.scrollWidth > el.clientWidth + 1,
      };
    });
    const w = document.querySelector('.st-head .sum-worst');
    const part = (sel) => (shown(w?.querySelector(sel)) ? box(w.querySelector(sel)) : null);
    const worst = shown(w)
      ? {
          box: box(w),
          title: w.querySelector('.sw-title').textContent,
          hover: w.getAttribute('title'),
          task: w.querySelector('.sw-task')?.textContent ?? null,
          word: part('.sw-word'),
          t: part('.sw-title'),
          k: part('.sw-task'),
          titleLines: w.querySelector('.sw-title').getClientRects().length,
          titleCut: w.querySelector('.sw-title').scrollWidth > w.querySelector('.sw-title').clientWidth + 1,
          titleH: w.querySelector('.sw-title').getBoundingClientRect().height,
          titleLineH: lineH(w.querySelector('.sw-title')),
        }
      : null;
    // Rows, by the cells' bottom edges (the row aligns them at the bottom).
    const cells = [...items.map((i) => i.box), ...(worst ? [worst.box] : [])];
    const rows = [];
    for (const c of cells) {
      const row = rows.find((r) => Math.abs(r.bottom - c.bottom) < 6);
      if (row) row.n += 1;
      else rows.push({ bottom: c.bottom, n: 1, items: 0 });
    }
    for (const i of items) rows.find((r) => Math.abs(r.bottom - i.box.bottom) < 6).items += 1;
    const band = document.querySelector('.band-info .bi2.bs');
    const strip = document.querySelector('.st-head .sum');
    return {
      items,
      worst,
      rows: rows.map((r) => ({ n: r.n, items: r.items })),
      right: Math.max(0, ...cells.map((c) => c.right)),
      safeRight: parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--safe-right')) || 0,
      width: innerWidth,
      sideways: document.documentElement.scrollWidth > innerWidth + 1,
      strip: shown(strip) ? box(strip) : null,
      band: shown(band)
        ? { text: band.textContent, h: band.getBoundingClientRect().height, cut: band.scrollWidth > band.clientWidth + 1, values: [...band.querySelectorAll('.bs-i b')].map((b) => b.textContent) }
        : null,
    };
  });
}

export const tests = {
  async '72 questions stay bounded and searchable'(t) {
    const view = fixture('status-view.json');
    const seed = view.cards.open[0];
    view.cards.open = Array.from({ length: 72 }, (_, i) => ({
      ...seed,
      id: `vm.inbox--Q${i + 1}`,
      label: `Q${i + 1}`,
      title: `Inbox question ${i + 1}`,
      writer: `vm.inbox-${i % 3}`,
      askedBy: `Inbox thread ${i % 3}`,
    }));
    for (const width of [1440, 402]) {
      await t.viewport(width, 900);
      await openStatus(t, view);
      await t.page.waitForSelector(CARD);
      const index = width === 1440 ? '.q-list' : '.q-list-phone';
      if (width === 402) await t.page.click('.q-step .disclosure');
      await t.count(`${index} .q-item`, 8);
      await t.check('question index fits half the viewport', () =>
        t.page.evaluate((selector) => {
          return (
            document.querySelector(`${selector} .q-index-items`).getBoundingClientRect().height <=
            innerHeight * 0.46
          );
        }, index)
      );
      await t.page.fill(`${index} input[type="search"]`, 'Inbox question 72');
      await t.count(`${index} .q-item`, 1);
      await t.page.click(`${index} .q-item`);
      await t.text(`${CARD} .q-title`, /Inbox question 72/);
    }
  },
  // The owner, 2026-11-11: "The cards arent labeled so idk which are the ones you are referring to." Every open card
  // shows its label where he looks and the index groups by batch. A card that stored no label shows the id after
  // `--`, which is what chats quote (2026-11-13; the server already fills it so).
  async 'status every open card shows its label'(t) {
    // Videos left out: on the Mac's Chrome a card video's request is not caught by the page route (TOOL-ISSUES 39).
    const noVideo = (x) => (Array.isArray(x) ? x.filter((v) => v?.kind !== 'video').map(noVideo) : x && typeof x === 'object' ? Object.fromEntries(Object.entries(x).map(([k, v]) => [k, noVideo(v)])) : x);
    const view = noVideo(fixture('server-view.json'));
    view.cards.open = view.cards.open.map((c, i) => (i === 1 ? { ...c, label: '' } : c));
    const labelled = view.cards.open.map((c) => c.label || c.id.split('--').pop());
    for (const [w, h] of [[1760, 1000], [390, 844]]) {
      await t.viewport(w, h);
      await openStatus(t, view);
      await t.page.waitForSelector(CARD);
      const seen = [];
      let unlabelled = 0;
      for (let i = 0; i < view.cards.open.length; i++) {
        const got = await t.page.evaluate(() => {
          const el = document.querySelector('article.q-card .q-title .q-label');
          if (!el) return null;
          const r = el.getBoundingClientRect();
          return { text: el.textContent, shown: r.width > 0 && r.height > 0 && r.left >= 0 && r.right <= innerWidth };
        });
        if (got?.shown) seen.push(got.text);
        if (!got) unlabelled++;
        if (i < view.cards.open.length - 1) await t.page.click(NEXT);
      }
      await t.check(`${w}: every open card shows its label in its head`, labelled.every((l) => seen.includes(l)), seen.join(', '));
      await t.equal(`${w}: a card that stored no label shows its id suffix`, unlabelled, 0);
      if (w === 1760) {
        // Fresh, on the first card: the index's first page holds the F batch.
        await openStatus(t, view);
        await t.page.waitForSelector(CARD);
        await t.text('.q-list .q-index-items', /F questions · Nov 5 · from /);
        await t.check('index rows lead with the label', async () =>
          (await t.page.$$eval('.q-list .q-item .q-label', (l) => l.map((x) => x.textContent))).includes('F-Q4'));
        await t.page.fill('.q-list input[type="search"]', 'F-Q4');
        await t.text(`${CARD} .q-title .q-label`, 'F-Q4');
      }
    }
  },

  // No stubs: the real server reads the kit's fixture reports folder, serves card media with byte ranges, records the
  // answer in the temporary home's store, signs it into the temporary reports folder, and rings nobody (doorbell off).
  async 'status end to end on the server'(t) {
    await t.viewport(1440, 900);
    await t.app.signIn();
    await t.goto('/app/status', { settleMs: 300 });
    await t.page.waitForSelector(CARD);
    // The fixture's note-only answer is a written response: it closed its card (owner, 2026-11-11), so 8 stay open.
    await t.text('.sec-questions .plate-meta', /\b8 open/);
    await t.check('card video plays from the server (byte ranges)', async () =>
      t.page.evaluate(async () => {
        const v = [...document.querySelectorAll('.media video')][0];
        if (!v) return 'the first card has no video';
        v.muted = true;
        try {
          await v.play();
          v.pause();
          return true;
        } catch (e) {
          return String(e);
        }
      })
    );
    // Answer the first question card in the deck, then send from the review.
    let found = false;
    for (let i = 0; i < 8 && !found; i++) {
      found = await t.page.evaluate(() => {
        const card = document.querySelector('article.q-card');
        return (
          !!card &&
          ['question', ''].includes(card.dataset.kind || '') &&
          !!card.querySelector('.opt-pick:not([disabled])')
        );
      });
      if (!found) await t.page.click(NEXT);
    }
    await t.check('a question card to answer', found);
    const before = signedAnswers(t).length;
    await t.page.click(`${CARD} .opt-pick >> nth=0`);
    await t.page.click('.q-send .primary');
    await t.page.waitForSelector('.review');
    await t.text('.review .q-send .primary', /Send 1 answer/);
    await t.page.click('.review .q-send .primary');
    await t.page.waitForSelector('.sent');
    await t.text('.sent .ok', /Sent 1 at/);
    await t.equal(
      'one signed answer written to the test reports folder',
      signedAnswers(t).length,
      before + 1
    );
    await t.until(
      'the answered card leaves the deck',
      () => /\b7 open/.test(document.querySelector('.sec-questions .plate-meta')?.textContent || ''),
      null,
      { timeout: 15000 }
    );
    await t.audit('end to end 1440');
  },

  async 'status desktop'(t) {
    await t.viewport(1440, 900);
    await openStatus(t, fixture('status-view.json'));
    for (const section of SECTIONS) await t.count(section, 1);
    await t.count('#usage, #threads, .sec-usage, .sec-threads', 0);
    await t.page.waitForSelector(CARD);
    await t.text('.sec-questions .plate-meta', /6 open/);
    await t.audit('status 1440');
    await t.shot(
      'status-desktop',
      'Status page at 1440 px: panel grid, question deck, no clipped text'
    );
    for (const width of [1024, 1920]) {
      await t.viewport(width, 900);
      await t.page.waitForTimeout(200);
      await t.audit(`status ${width}`);
      await t.snap(`status-${width}`);
    }
  },

  // The view the server branch builds from scripts/status-fixture.mjs (--with-kinds --with-problems): nulls, damaged
  // report files and unreachable machines must read plainly, not break the page.
  async 'status from a server view'(t) {
    await t.viewport(1440, 900);
    await openStatus(t, fixture('server-view.json'));
    await t.page.waitForSelector(CARD);
    // The fixture's note-only answer is a written response: it closed its card (owner, 2026-11-11), so 8 stay open.
    await t.text('.sec-questions .plate-meta', /\b9 open/);
    await t.text('.sec-machines', /ssh refused/);
    await t.text('.sec-machines', /under the 50 GB floor/);
    await t.text('.sec-questions .plate-note', /unreadable/i);
    await t.check('nothing reads as zero for a machine with no reading', async () =>
      t.page.evaluate(() => {
        const nas = [...document.querySelectorAll('.mach-table .row')].find((m) =>
          /Nas1/.test(m.textContent)
        );
        return !!nas && !/\b0(\.0)? ?GB free/.test(nas.textContent);
      })
    );
    // The threads' card additions: a verdict filter, a judges' split, a sample that sends nothing, verbatim quotes.
    await t.count('.q-seg .seg button', 2); // All and Adapt, the fixture's one verdict
    await t.page.click('.q-seg .seg button >> text=Adapt');
    await t.count('.q-list .q-item.sel[data-label="F-Q4"]', 1);
    await t.visible('text=The judges split', 'split judges are flagged');
    await t.page.click('.q-seg .seg button >> text=All');
    // C-S1 is last in the grouped deck (its thread's only card), on the index's second page: reach it by its label.
    await t.page.fill('.q-list input[type="search"]', 'C-S1');
    await t.text(`${CARD} .q-title .q-label`, 'C-S1');
    await t.visible('.sample-banner', 'a sample card says no answer is wanted');
    await t.check('a sample card cannot be answered', async () =>
      t.page.evaluate(() =>
        [...document.querySelectorAll('article.q-card .opt-pick')].every((b) => b.disabled)
      )
    );
    // Typing a card's label goes straight to it (ST-Q2 sits with ST-D1, its thread's other card).
    await t.page.fill('.q-list input[type="search"]', 'st-q2');
    await t.text(`${CARD} .q-title .q-label`, 'ST-Q2');
    await t.page.click('.q-list .q-item[data-label="ST-Q2"]');
    await t.count(`${CARD} .quote`, 1);
    await t.count(`${CARD} .plan-change`, 1);
    await t.audit('server view 1440');
    await t.crop(
      'status-machines',
      '.sec-machines',
      'unreachable machines say why, and show no invented readings'
    );
  },

  // The design's formats, standalone (the page's own chrome), with the insets measured on the simulators
  // (DUO-MEASURED.md) given through ?safe=. WebKit itself is checked on the simulators.
  async 'status layout in each format'(t) {
    const view = {
      ...fixture('status-design-view.json'),
      spendByStream: fixture('spend-by-stream.json'),
    };
    for (const f of FORMATS) {
      await t.newContext({
        viewport: { width: f.w, height: f.h },
        isMobile: f.mobile,
        hasTouch: f.mobile,
        deviceScaleFactor: 1,
      });
      await openStatus(t, view, `?safe=${f.safe}`);
      await t.page.waitForSelector(CARD);
      await t.audit(`${f.name} ${f.w}x${f.h}`);
      await t.equal(
        `${f.name}: chrome`,
        await t.page.evaluate(() => document.documentElement.dataset.chrome),
        f.chrome
      );
      await t.check(
        `${f.name}: no sideways scroll`,
        await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1)
      );
      await t.check(
        `${f.name}: the content is as wide as its window, not its widest child`,
        await t.page.evaluate(
          () => document.querySelector('.content').getBoundingClientRect().width <= innerWidth
        )
      );
      await t.count('#usage, #threads', 0);
      // Line breaks: "Recommended" stays with its label, and each row's percent with its name.
      await t.check(
        `${f.name}: "Recommended" is never alone on a line`,
        await t.page.evaluate(() => {
          const rec = document.querySelector('.opt-rec');
          if (!rec) return true;
          const rects = rec.getClientRects();
          const label = rec.closest('.opt-label');
          const line = document.createRange();
          line.selectNodeContents(label.firstChild);
          const words = [...line.getClientRects()];
          return rects.length === 1 && words.some((w) => Math.abs(w.bottom - rects[0].bottom) < 4);
        })
      );
      if (f.chrome === 'band') {
        await t.count('.band-info a, .band-info button', 0);
        await t.check(
          'band: the tabs sit below the 82 px band',
          await t.page.evaluate(
            () => document.querySelector('.band-tabs').getBoundingClientRect().top >= 82
          )
        );
      }
      if (f.chrome === 'rail') {
        await t.check(
          `${f.name}: the rail is inside the 84 px strip, below the clock`,
          await t.page.evaluate(() => {
            const rail = document.querySelector('.rail').getBoundingClientRect();
            const first = document.querySelector('.rail a').getBoundingClientRect();
            return (
              Math.round(rail.width) === 84 &&
              first.top >=
                parseFloat(
                  getComputedStyle(document.documentElement).getPropertyValue('--rail-reserve')
                )
            );
          })
        );
      }
      if (f.name === 'duo-landscape') {
        await t.page.click('.plate-tools.only-land .btn >> nth=1'); // the second question has choices
        await t.page.waitForSelector('.opts .opt');
        await t.check(
          'landscape: the question card lies across the screen',
          await t.page.evaluate(() => {
            const opts = [...document.querySelectorAll('.opts .opt')].map((o) =>
              o.getBoundingClientRect()
            );
            return (
              opts.length === 3 &&
              Math.abs(opts[0].top - opts[1].top) < 2 &&
              opts[1].left > opts[0].right - 2
            );
          })
        );
        await t.count('.plate-tools.only-land .btn', 3);
      }
    }
  },

  // The summary strip (owner, 2026-11-12: the headline sentence "just looks like an out of place paragraph"). At each
  // width the counts and the worst pinned thread take at most two rows, no count or word sits alone, nothing is cut
  // but the long title (whole on hover and on a tap), every number and word drawn is the view's, and a tap jumps to
  // the count's section or opens its sentence.
  async 'status summary strip at each width'(t) {
    const view = stripView();
    const want = view.headline.strip;
    for (const f of STRIP_FORMATS) {
      await t.newContext({ viewport: { width: f.w, height: f.h }, ...(f.screen ? { screen: f.screen } : {}), isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1 });
      await openStatus(t, view, `?safe=${f.safe}`);
      await t.page.waitForSelector('.st-head .sum-worst');
      await t.equal(`${f.name}: chrome`, await t.page.evaluate(() => document.documentElement.dataset.chrome), f.chrome);
      const m = await stripLayout(t);
      const name = f.name;
      // The sentence is gone; the strip is there.
      await t.count('.st-head .headline', 0);
      await t.check(`${name}: at most ${f.maxRows ?? 2} rows (${m.rows.map((r) => r.n).join(' + ')})`, m.rows.length <= (f.maxRows ?? 2));
      await t.check(`${name}: no sideways scroll, the strip inside the screen`, !m.sideways && m.right <= m.width - m.safeRight + 1);
      if (f.chrome === 'band') {
        // The band takes no taps: the counts are drawn there on one line, the page keeps the worst thread's line.
        await t.equal(`${name}: no counts on the page under the band`, m.items.length, 0);
        await t.check(`${name}: the band's counts on one line, none cut off`, !!m.band && m.band.h <= 22 && !m.band.cut);
        await t.equal(`${name}: the band's numbers are the view's`, m.band?.values, want.items.map((i) => i.value));
        for (const i of want.items)
          await t.check(`${name}: the band says "${i.value} ${i.short}"`, !!m.band && m.band.text.includes(`${i.value} ${i.short}`));
      } else {
        await t.equal(`${name}: the counts are the view's, in its order`, m.items.map((i) => [i.key, i.value]), want.items.map((i) => [i.key, i.value]));
        await t.check(
          `${name}: each label is the view's long or short words`,
          m.items.every((i) => { const v = want.items.find((x) => x.key === i.key); return i.label === v.label || i.label === v.short; })
        );
        await t.check(`${name}: every label on one line, under its number`, m.items.every((i) => i.labelOneLine && Math.abs(i.l.left - i.n.left) < 1.5 && i.l.top >= i.n.bottom - 2));
        await t.check(`${name}: no count is cut`, m.items.every((i) => !i.cut));
        const itemRows = m.rows.filter((r) => r.items > 0);
        await t.check(`${name}: the counts are one row, or no row holds one count alone`, itemRows.length === 1 || itemRows.every((r) => r.items >= 2));
      }
      // The worst pinned thread: one line, its state word and task kept with the title, the title cut and whole on hover.
      const w = m.worst;
      await t.check(`${name}: the worst thread's line is drawn`, !!w);
      await t.equal(`${name}: its title and task are the view's`, [w.title, w.task], [want.worst.title, want.worst.task]);
      await t.check(`${name}: the title is one line`, w.titleLines === 1 && w.titleH < w.titleLineH * 1.5);
      await t.check(`${name}: hovering shows the whole title`, (w.hover ?? '').includes(want.worst.title));
      const beside = Math.abs(w.word.top - w.t.top) > 4;
      await t.check(
        `${name}: "Looping" and the task never sit alone (${beside ? 'beside the counts' : 'one line'})`,
        beside ? Math.abs(w.word.bottom - w.k.bottom) < 4 && w.word.top > w.t.top : Math.abs(w.word.bottom - w.t.bottom) < 4 && Math.abs(w.k.bottom - w.t.bottom) < 4
      );
      if (f.chrome === 'desk' || f.name === 'duo-landscape-951')
        await t.check(`${name}: the worst thread sits beside the counts (one row)`, m.rows.length === 1);
      if (m.strip) await t.crop(`strip-${name}`, { x: 0, y: Math.max(0, m.strip.y - 8), width: f.w, height: m.strip.h + 16 }, `summary strip at ${f.w}x${f.h} (${f.chrome})`);
      // A tap on the worst thread opens its whole title and the composer's sentence; a second tap closes them.
      await t.page.click('.st-head .sum-worst');
      await t.page.waitForSelector('.sum-more[data-for="worst"]');
      await t.equal(`${name}: the tap opens the thread's sentence`, await t.page.textContent('.sum-more'), want.worst.detail);
      await t.check(
        `${name}: the tapped title shows whole`,
        await t.page.evaluate(() => { const e = document.querySelector('.sum-worst.open .sw-title'); return !!e && e.scrollWidth <= e.clientWidth + 1; })
      );
      await t.page.click('.st-head .sum-worst');
      await t.count('.sum-more', 0);
      if (f.chrome === 'band') continue;
      // A count no one section explains opens its sentence; one that has a section jumps to it, clear of the header.
      await t.page.click('.sum-i[data-key="sources"]');
      await t.equal(`${name}: "not current" names the sources`, await t.page.textContent('.sum-more'), want.items.find((i) => i.key === 'sources').detail);
      await t.equal(`${name}: and says it is open`, await t.page.getAttribute('.sum-i[data-key="sources"]', 'aria-expanded'), 'true');
      await t.page.click('.sum-i[data-key="looping"]');
      await t.count('.sum-more', 0);
      await t.until(`${name}: "looping" scrolls Pinned threads to the top, below the header`, () => {
        const r = document.querySelector('.sec-pinned').getBoundingClientRect();
        return r.top >= 0 && r.top < 140;
      });
      await t.check(
        `${name}: Pinned threads' head is not under the sticky header`,
        await t.page.evaluate(() => {
          const sec = document.querySelector('.sec-pinned').getBoundingClientRect();
          const bar = document.querySelector('.chrome, .topbar');
          const shown = bar && bar.getClientRects().length > 0 && getComputedStyle(bar).position === 'sticky';
          return !shown || sec.top >= bar.getBoundingClientRect().bottom - 1;
        })
      );
    }
    // Eight counts (a decision and a budget on top): still no count alone on a row, nothing cut, nothing sideways.
    const eight = stripView();
    eight.headline.strip.items.splice(1, 0, { key: 'decisions', value: '1', label: 'decision to look at', short: 'decision', tone: 'warn', section: 'questions', detail: '1 decision to look at.' });
    eight.headline.strip.items.splice(6, 0, { key: 'budgets', value: '1', label: 'over budget (estimate)', short: 'over budget', tone: 'bad', section: 'spend', detail: '1 budget is at or over its limit (estimate).' });
    for (const f of STRIP_FORMATS.filter((x) => x.chrome !== 'band')) {
      await t.newContext({ viewport: { width: f.w, height: f.h }, ...(f.screen ? { screen: f.screen } : {}), isMobile: f.mobile, hasTouch: f.mobile, deviceScaleFactor: 1 });
      await openStatus(t, eight, `?safe=${f.safe}`);
      await t.page.waitForSelector('.st-head .sum-worst');
      const m = await stripLayout(t);
      const name = `${f.name}, eight counts`;
      await t.equal(`${name}: all eight drawn`, m.items.map((i) => i.value).length, 8);
      await t.check(`${name}: no count alone on a row (${m.rows.map((r) => r.items).join(' + ')})`, m.rows.filter((r) => r.items > 0).every((r) => r.items >= 2 || m.rows.filter((x) => x.items > 0).length === 1));
      await t.check(`${name}: at most three rows`, m.rows.length <= 3);
      await t.check(`${name}: every label on one line, nothing cut`, m.items.every((i) => i.labelOneLine && !i.cut));
      await t.check(`${name}: no sideways scroll`, !m.sideways && m.right <= m.width - m.safeRight + 1);
      if (m.strip) await t.crop(`strip-eight-${f.name}`, { x: 0, y: Math.max(0, m.strip.y - 8), width: f.w, height: m.strip.h + 16 }, `eight counts at ${f.w}x${f.h}`);
    }
    // A server older than the strip keeps its sentence.
    const old = fixture('status-design-view.json');
    delete old.headline.strip;
    await t.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });
    await openStatus(t, old, '?safe=0,0,0,0');
    await t.count('.st-head .sum', 0);
    await t.count('.st-head .headline', 1);
  },

  // The design fixture's own strip: the view's numbers, and a quiet unknown drawn as "–", never 0.
  async 'status summary strip draws the view, unknowns included'(t) {
    const view = { ...fixture('status-view.json') };
    await t.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1 });
    await openStatus(t, view, '?safe=0,0,34,0');
    await t.page.waitForSelector('.st-head .sum-i');
    const m = await stripLayout(t);
    await t.equal('the counts are the view\'s', m.items.map((i) => [i.key, i.value]), view.headline.strip.items.map((i) => [i.key, i.value]));
    await t.check('at most two rows', m.rows.length <= 2);
    await t.equal(
      'the unknown pinned count is a quiet "–"',
      await t.page.evaluate(() => { const n = document.querySelector('.sum-i[data-key="pinned"] .sum-n'); return n && [n.textContent, n.dataset.tone]; }),
      ['–', 'quiet']
    );
    await t.count('.st-head .sum-worst', 0);
  },

  // The chrome follows the window, not the screen. The kit leaves the screen at the window size, so every plain
  // 951x669 context reads as the Duo once the page tells the device from its screen: a small desktop window on a
  // 1440x900 screen keeps the desktop header (review 9702412d F4), and an iPhone on its side draws the rail and
  // pads its page by the 62 px left inset (review 9702412d F1).
  async 'status chrome follows the window, not the screen'(t) {
    const view = {
      ...fixture('status-design-view.json'),
      spendByStream: fixture('spend-by-stream.json'),
    };

    await t.newContext({
      viewport: { width: 951, height: 669 },
      screen: { width: 1440, height: 900 },
    });
    await openStatus(t, view);
    await t.page.waitForSelector(CARD);
    await t.equal(
      'a 951x669 window on a 1440x900 screen',
      await t.page.evaluate(() => document.documentElement.dataset.chrome),
      'desk'
    );
    await t.check(
      'the desktop header is drawn',
      await t.page.evaluate(
        () =>
          !!document.querySelector('header.chrome') &&
          getComputedStyle(document.querySelector('header.chrome')).display !== 'none'
      )
    );
    // The app frame always renders all four nav bars and hides the unused ones with display:none,
    // so the assertion is that they are hidden, not that they are absent (review 622ba7d0 F1).
    await t.check(
      'no other chrome is drawn',
      await t.page.evaluate(() =>
        [...document.querySelectorAll('.rail, .topbar, .tabbar, .band-head')].every(
          (e) => getComputedStyle(e).display === 'none'
        )
      )
    );

    // iPhone 17 Pro on its side: the notch insets land left and right, the rail takes the right strip.
    await t.newContext({
      viewport: { width: 874, height: 402 },
      screen: { width: 874, height: 402 },
      isMobile: true,
      hasTouch: true,
      deviceScaleFactor: 2,
    });
    await openStatus(t, view, '?safe=0,62,21,62');
    await t.page.waitForSelector(CARD);
    await t.equal(
      'an iPhone on its side',
      await t.page.evaluate(() => document.documentElement.dataset.chrome),
      'rail'
    );
    // The inset is padding on main.content itself, so its own edge sits at 0; the shown tab's route is what moves (the
    // frame keeps every tab's route, the hidden ones first when Home is native).
    await t.check(
      'the page pads by the 62 px left inset',
      await t.page.evaluate(
        () =>
          Math.round(
            document.querySelector('.content > .route:not([hidden])').getBoundingClientRect().left
          ) >= 62
      )
    );
  },

  // In the shell's iframe the page draws no chrome and keeps no room for one.
  async 'status inside the iframe keeps no chrome'(t) {
    await t.newContext({
      viewport: { width: 466, height: 678 },
      isMobile: true,
      hasTouch: true,
      deviceScaleFactor: 1,
    });
    await openStatus(t, fixture('status-design-view.json'), '?embed=1&safe=0,84,34,0');
    await t.equal(
      'no chrome inside the iframe',
      await t.page.evaluate(() => document.documentElement.dataset.chrome),
      'none'
    );
    await t.count('.rail, .band-head, .tabbar, .topbar, header.chrome', 0);
    await t.check(
      'nothing is reserved on the right',
      await t.page.evaluate(
        () => getComputedStyle(document.querySelector('.content')).width === `${innerWidth}px`
      )
    );
  },

  // Fix round 1, R1: at the Duo's landscape size the deck's own Back, Next and Review row is replaced by the plate head,
  // but the review screen's Send is the only way to send, so it has to stay.
  async 'status review can be sent at the Duo landscape size'(t) {
    for (const [name, query] of [
      ['with the rail', '?safe=0,84,34,0'],
      ['with no insets', '?safe=0,0,0,0'],
      ['in the iframe', '?embed=1'],
    ]) {
      await t.newContext({
        viewport: { width: 951, height: 669 },
        isMobile: true,
        hasTouch: true,
        deviceScaleFactor: 1,
      });
      const sent = await openStatus(
        t,
        { ...fixture('status-design-view.json'), spendByStream: fixture('spend-by-stream.json') },
        query
      );
      await t.page.waitForSelector(CARD);
      await t.page.locator('.plate-tools.only-land .btn', { hasText: /^Next$/ }).click();
      await t.page.waitForSelector('.opts .opt');
      await t.page.locator(`${CARD} .opt-pick`).nth(1).click();
      await t.page.locator('.plate-tools.only-land .primary').click();
      await t.page.waitForSelector('.review');
      const send = t.page.locator('.review .q-send .primary');
      const shown = await send.isVisible();
      await t.check(`${name}: the final Send is on screen`, shown);
      await t.check(
        `${name}: Back to the questions is on screen`,
        await t.page
          .locator('.review .q-send button', { hasText: /Back to the questions/ })
          .isVisible()
      );
      await t.check(
        `${name}: the head has no Next or Review during the review`,
        !(await t.page.locator('.plate-tools.only-land .primary').isVisible())
      );
      if (!shown) continue; // a click on a hidden button would only time out
      await t.text('.review .q-send .primary', /Send 1 answer/);
      await send.click();
      await t.page.waitForSelector('.sent');
      await t.text('.sent .ok', /Sent 1 at/);
      await t.equal(`${name}: one send`, sent.length, 1);
    }
  },

  // Fix round 1, R2: fixtures/pinned-composer-cases.json is the real output of native/aac-status's composer.
  async 'status says when a pinned report cannot be read'(t) {
    const cases = fixture('pinned-composer-cases.json');
    const withPins = (pinned) => ({ ...fixture('status-design-view.json'), pinned });
    const NOTE = /Could not be read since 8:33 PM\./;
    const opacity = (selector) =>
      t.page.evaluate((s) => {
        let value = 1;
        for (let e = document.querySelector(s); e; e = e.parentElement)
          value *= parseFloat(getComputedStyle(e).opacity);
        return value;
      }, selector);

    await t.viewport(1440, 900);
    await openStatus(t, withPins(cases.unreadableWithTasks));
    await t.page.waitForSelector('.c2-pane .c2-sheet');
    await t.text('.c2-pane .c2-sheet', NOTE);
    await t.text('.c2-pane .c2-sheet', /cached/i);
    await t.check('pane: the cached copy is dimmed', (await opacity('.c2-pane .c2-cols')) < 0.6);
    await t.check(
      'pane: the problem shows once, in the sheet, not also on the open row',
      await t.page.evaluate(() => getComputedStyle(document.querySelector('.c2-row.sel .c2-prob') ?? document.body).display === 'none' || !document.querySelector('.c2-row.sel .c2-prob'))
    );

    await t.newContext(PHONE);
    await openStatus(t, withPins(cases.unreadableWithTasks));
    await t.page.waitForSelector('.c2-row');
    await t.text('.c2-row', NOTE);
    await t.text('.c2-row', /cached/i);
    await t.page.locator('.c2-row').first().click();
    await t.page.waitForSelector('.c2-sheet-host .c2-sheet');
    await t.text('.c2-sheet-host .c2-sheet', NOTE);
    await t.text('.c2-sheet-host .c2-sheet', /cached/i);
    await t.check('sheet: the cached copy is dimmed', (await opacity('.c2-sheet-host .c2-cols')) < 0.6);

    // The composer's own test case, which has no task list behind it.
    await t.newContext(PHONE);
    await openStatus(t, withPins(cases.unreadable));
    await t.page.waitForSelector('.c2-row');
    await t.text('.c2-row', NOTE);
    await t.page.locator('.c2-row').first().click();
    await t.page.waitForSelector('.c2-sheet-host .c2-sheet');
    await t.text('.c2-sheet-host .c2-sheet', NOTE);
    await t.text('.c2-sheet-host .c2-sheet', /cached/i);
  },

  // Fix round 1, R3: the server doesn't say which task is looping, and a blocked task can sit ahead of it.
  async 'status pinned row names no task the server did not'(t) {
    const cases = fixture('pinned-composer-cases.json');
    await t.newContext(PHONE);
    await openStatus(t, {
      ...fixture('status-design-view.json'),
      pinned: cases.loopingBehindBlocked,
    });
    await t.page.waitForSelector('.c2-row');
    const line = await t.page.locator('.c2-row .c2-prob').first().textContent();
    await t.check('row: says Looping', /^Looping\b/.test(line), line);
    await t.check('row: names no task as the looping one', !/Looping on/.test(line), line);
    await t.page.locator('.c2-row').first().click();
    await t.page.waitForSelector('.c2-sheet-host .c2-sheet');
    await t.text('.c2-sheet-host .c2-sheet', /F-07 has gone back for fixes 3 times/);
  },

  // Concept C v2 (owner S1): the roster, the five fixed facts, stages, things to open, grouped tasks, one problem line.
  async 'status concept C v2 pinned threads'(t) {
    const c2 = fixture('status-c2-pins.json');
    const view = { ...fixture('status-design-view.json'), pinned: c2.pinned, projects: c2.projects };
    await t.viewport(1440, 900);
    await openStatus(t, view);
    await t.page.waitForSelector('.c2-pane .c2-sheet');
    await t.count('.sec-progress', 0);
    await t.check('Needs you is the first group', /^Needs you/.test(await t.page.locator('.c2-group').first().textContent()));
    await t.text('.c2-row.sel .c2-l2', /^Ready for you 2 things to open/);
    await t.text('.c2-pane .c2-state', /^Ready for you/);
    await t.check('five fixed facts, in order', async () =>
      JSON.stringify(await t.page.$$eval('.c2-pane .fx dt', (d) => d.map((x) => x.textContent))) ===
      JSON.stringify(['For you', 'Waiting on', 'Now', 'Next', 'Updated'])
    );
    await t.text('.c2-pane .fx', /was due/);
    await t.text('.c2-pane', /Reviews, round 2, 3 of 6/);
    await t.count('.c2-pane .c2-stages li', 6);
    await t.check('the owner step waits in amber', await t.page.$$eval('.c2-pane .c2-stages li[data-st="you"]', (l) => l.length === 1));
    // Things to open: main first with the blue Open through the route; a file outside the share offers Copy path.
    const open = t.page.locator('.c2-pane .to-row.main a.btn.primary');
    await t.check('main opens through the deliverable route', /^\/api\/status\/deliverable\?path=share%2F/.test(await open.getAttribute('href')));
    await t.text('.c2-pane .to-list', /Kept outside the share, so it cannot be opened here/);
    await t.text('.c2-pane .to-list', /Copy path/);
    await t.text('.c2-pane .to-row.main .to-mark', /^New$/);
    await t.text('.c2-pane .to-row.main', /1 more is kept outside the share/);
    const [popup] = await Promise.all([t.page.waitForEvent('popup'), open.click()]);
    await popup.close();
    await t.text('.c2-pane .to-row.main .to-mark', /^Opened /);
    await t.page.click('.c2-pane .to-row.main .to-sub button.link');
    await t.text('.c2-pane .to-older', /Superseded by v2/);
    // Tasks: grouped; one unknown state marks one row; the big group folds.
    await t.text('.c2-pane', /reported as "queued"/);
    await t.text('.c2-pane', /Show all 8/);
    await t.text('.c2-pane', /in work/i);
    // The README flag shows on its closed row only.
    await t.text('.c2-roster', /Questions in a README, not filed as cards\./);
    await t.check('the Progress section is gone', (await t.page.locator('text=Progress').count()) === 0);
    // A report written before the new fields: nothing invented.
    await t.page.locator('.c2-row', { hasText: 'Build PMConcept7' }).click();
    await t.text('.c2-pane .c2-state', /^Reporting/);
    await t.text('.c2-pane .fx', /Not reported/);
    await t.text('.c2-pane', /Own estimate 70%/);
    await t.text('.c2-silent', /Exact Agent Tooling Assessment/);

    for (const [w, h] of [[390, 844], [402, 874], [466, 900], [669, 900], [951, 900], [1760, 1000]]) {
      await t.viewport(w, h);
      await openStatus(t, view);
      await t.page.waitForSelector('.c2-row');
      await t.check(`${w}: no sideways scroll`, await t.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
      const plates = await t.page.evaluate(() => {
        const rows = [...document.querySelectorAll('.c2-row')].slice(0, 2).map((r) => r.getBoundingClientRect());
        return { radius: parseFloat(getComputedStyle(document.querySelector('.c2-row')).borderTopLeftRadius), gap: rows[1].top - rows[0].bottom };
      });
      if (w < 900) {
        await t.check(`${w}: each pin is its own plate`, plates.radius > 0 && plates.gap >= 8, JSON.stringify(plates));
        if (w === 390) await t.shot('c2-roster-390', '390 px: pinned threads as separate plates (C2 phone)');
        await t.page.locator('.c2-row').first().click();
        await t.page.waitForSelector('.c2-sheet-host .c2-sheet');
        await t.check(`${w}: the sheet opens with the five facts`, (await t.page.locator('.c2-sheet-host .fx dt').count()) === 5);
        await t.check(`${w}: the sheet fits the width`, await t.page.evaluate(() => document.querySelector('.c2-sheet-host .sheet').getBoundingClientRect().right <= innerWidth + 1));
        await t.crop(`c2-sheet-${w}`, '.c2-sheet-host .sheet', `${w} px: a pinned thread opened as a sheet (C2 phone)`);
      } else {
        await t.check(`${w}: the pane sits beside the roster`, await t.page.evaluate(() => {
          const r = document.querySelector('.c2-roster').getBoundingClientRect(), p = document.querySelector('.c2-pane').getBoundingClientRect();
          return p.left >= r.right - 1 && p.width > 300;
        }));
        await t.crop(`c2-desktop-${w}`, '.sec-pinned', `${w} px: roster beside the thread pane (C2 wide)`);
      }
    }
  },

  // Data the server doesn't send yet is said so, never guessed.
  async 'status says what it cannot show'(t) {
    await t.viewport(1440, 900);
    await openStatus(t, fixture('status-view.json'));
    await t.text('.sec-spend', /isn't available from this server yet/);
    await t.text('.sec-pinned', /unavailable/);
    const down = {
      ...fixture('status-design-view.json'),
      pinned: { unavailable: "Pinned threads unavailable: T3's state file changed shape." },
      spendByStream: {
        state: 'unavailable',
        unavailable:
          'Spend by stream is unavailable: quota-watch has not written a report this server can read.',
        periods: [],
      },
    };
    await t.page.unroute((u) => new URL(u).pathname === '/api/status');
    await t.page.route(
      (u) => new URL(u).pathname === '/api/status',
      (r) => r.fulfill({ json: down })
    );
    await t.goto('/app/status', { settleMs: 300 });
    await t.page.waitForSelector('#cards');
    await t.text('.sec-pinned', /changed shape/);
    await t.text('.sec-spend', /has not written a report/);
    await t.check(
      'no percentages invented',
      await t.page.evaluate(() => !/\d%/.test(document.querySelector('.sec-spend').textContent))
    );
  },

  async 'status deck on a phone'(t) {
    await t.newContext(PHONE);
    const sent = await openStatus(t, fixture('status-view.json'));
    await t.page.waitForSelector(CARD);
    await t.audit('phone status');
    await t.shot(
      'phone-card',
      'first card on a 402 px phone: card fills the width, option media, readable spacing'
    );

    // A wide screenshot opens full screen turned sideways on an upright phone; Rotate, full size and Close work.
    await t.page.click('.media-open >> nth=0');
    await t.page.waitForSelector('.viewer img');
    await t.until(
      'wide screenshot opens sideways',
      () => document.querySelector('.viewer')?.dataset.rotated === '1'
    );
    await t.shot(
      'phone-viewer',
      'screenshot full screen, sideways: fills the screen, bar reachable'
    );
    await t.page.click('.viewer-frame img');
    await t.until(
      'tap shows full size',
      () => document.querySelector('.viewer')?.dataset.actual === '1'
    );
    await t.page.click('.viewer-btn >> text=Rotate');
    await t.until(
      'Rotate turns it upright',
      () => !document.querySelector('.viewer')?.dataset.rotated
    );
    await t.page.click('.viewer-btn.close');
    await t.count('.viewer', 0);

    // The video plays inline (byte ranges served) and opens in the viewer muted, with the system player one tap away.
    await t.check('inline video ready', async () =>
      t.page.evaluate(() => {
        const v = document.querySelector('.media video');
        return !!v && v.controls && v.playsInline && !!v.poster;
      })
    );
    await t.page.click('.media[data-kind="video"] button >> text=Full screen');
    await t.page.waitForSelector('.viewer video');
    await t.check('viewer video is muted and inline', async () =>
      t.page.evaluate(() => {
        const v = document.querySelector('.viewer video');
        return v.muted && v.playsInline && v.autoplay;
      })
    );
    await t.visible('.viewer-btn >> text=System player', 'system player button');
    await t.page.click('.viewer-btn.close');

    // Walk the deck: answer, object with words, mark an action done; then review and send once.
    await t.page.click('.opt-pick >> nth=0');
    await t.page.click(NEXT); // S-Q1 (Scout comparison)
    await t.page.waitForSelector('.block[data-type="compare"]');
    await t.page.click(NEXT); // C-Q5a (exception), first of its batch
    await t.visible('.q-rule', 'an exception to a standing rule shows first');
    await t.page.click(NEXT); // C-Q5c, the rest of the batch stays together
    await t.text(`${CARD} .q-title .q-label`, 'C-Q5c');
    await t.page.click(NEXT); // C-Q5b (objection), outside the batch
    await t.page.click('.q-actions .btn');
    await t.visible('text=An objection needs a reason', 'an objection asks for words');
    await t.page.fill('textarea.note', 'Keep whole-file hashes for the audit folder only.');
    await t.page.click(NEXT); // T-A1 (action)
    await t.page.click('.q-actions .btn');
    await t.text('.q-actions .btn', /Marked done/);
    await t.equal('nothing sent before the review', sent.length, 0);
    await t.page.click(NEXT); // Review
    await t.page.waitForSelector('.review');
    await t.text('.review .q-send .primary', /Send 3 answers/);
    await t.shot('phone-review', 'review screen: each card says what will be sent');
    await t.page.click('.review .q-send .primary');
    await t.page.waitForSelector('.sent');
    await t.text('.sent .ok', /Sent 3 at/);
    const entries = sent[0]?.answers || [];
    await t.equal('one send', sent.length, 1);
    await t.check(
      'the send carries its id (a content hash, so a retry is recorded once)',
      /^[0-9a-f]{32}$/.test(sent[0]?.sendId || '')
    );
    await t.equal(
      'kinds sent',
      entries
        .map((a) => a.kind)
        .sort()
        .join(','),
      'answer,done,object'
    );
    await t.check(
      'every entry names the exact card version',
      entries.every((a) => a.cardId && a.cardSha256 && Number.isInteger(a.cardSeq))
    );
    await t.check(
      'the objection carries its words',
      entries.some((a) => a.kind === 'object' && /audit folder/.test(a.note))
    );
  },

  // The note box follows the draft without moving the cursor on a re-read, and a send's result outlives a read that
  // empties the cards while it is on its way (the review stays; "No questions waiting" waits for the result).
  async 'status typing and a send that outlives the cards'(t) {
    await t.viewport(1440, 900);
    await t.app.signIn();
    const full = fixture('status-view.json');
    let current = full;
    let gets = 0;
    let release;
    const held = new Promise((resolve) => (release = resolve));
    await t.page.route(
      (u) => new URL(u).pathname === '/api/status',
      (r) => {
        gets += 1;
        return r.fulfill({ json: current });
      }
    );
    await t.page.route(
      (u) => new URL(u).pathname === '/api/status/answers',
      async (r) => {
        const body = JSON.parse(r.request().postData() || '{}');
        await held;
        await r.fulfill({
          json: {
            results: (body.answers || []).map((a) => ({ cardId: a.cardId, outcome: 'recorded' })),
          },
        });
      }
    );
    await stubMedia(t);
    await t.goto('/app/status', { settleMs: 300 });
    await t.page.waitForSelector(`${CARD} textarea.note`);
    const reread = async () => {
      const before = gets;
      await t.page.evaluate(() => window.dispatchEvent(new Event('online')));
      for (let i = 0; i < 50 && gets === before; i++) await t.page.waitForTimeout(100);
      await t.page.waitForTimeout(300);
      return gets > before;
    };
    await t.page.fill('textarea.note', 'abc def');
    await t.page.evaluate(() => document.querySelector('textarea.note').setSelectionRange(3, 3));
    await t.page.keyboard.type('X');
    await t.check('a re-read happened while typing', await reread());
    await t.check('typing survives a re-read: same words, cursor where it was', async () =>
      t.page.evaluate(() => {
        const a = document.querySelector('textarea.note');
        return a.value === 'abcX def' && a.selectionStart === 4 && document.activeElement === a;
      })
    );
    await t.page.keyboard.type('Y');
    await t.check('the next keystroke lands at the cursor', async () =>
      t.page.evaluate(() => document.querySelector('textarea.note').value === 'abcXY def')
    );
    // Send that note; while the POST is held, a read finds no cards.
    await t.page.click('.q-send .primary');
    await t.page.waitForSelector('.review .q-send .primary');
    await t.page.click('.review .q-send .primary');
    current = { ...full, cards: { ...full.cards, open: [], batches: [] } };
    await t.check('a re-read happened mid-send', await reread());
    await t.check('the review stays while the send is on its way', async () =>
      t.page.evaluate(
        () =>
          !!document.querySelector('.review') &&
          !/No questions waiting/.test(document.querySelector('.sec-questions')?.textContent || '')
      )
    );
    release();
    await t.page.waitForSelector('.sent', { timeout: 10000 });
    await t.text('.sent .ok', /Sent 1 at/);
    await t.page.click('.sent button >> text=Back to the questions');
    await t.text('.sec-questions .q-empty', /No questions waiting/);
  },

  // A send that fails after a read emptied the cards keeps its review and says nothing is left to send; an idle
  // review whose cards closed gives way, and a question that arrives later opens on the deck.
  async 'status a failed send and an idle review when the cards empty'(t) {
    await t.viewport(1440, 900);
    await t.app.signIn();
    const full = fixture('status-view.json');
    const empty = { ...full, cards: { ...full.cards, open: [], batches: [] } };
    let current = full;
    let gets = 0;
    let release;
    const held = new Promise((resolve) => (release = resolve));
    await t.page.route(
      (u) => new URL(u).pathname === '/api/status',
      (r) => {
        gets += 1;
        return r.fulfill({ json: current });
      }
    );
    await t.page.route(
      (u) => new URL(u).pathname === '/api/status/answers',
      async (r) => {
        await held;
        await r.fulfill({ status: 500, json: { error: 'Boom.', code: 'answers_failed' } });
      }
    );
    await stubMedia(t);
    await t.goto('/app/status', { settleMs: 300 });
    await t.page.waitForSelector(`${CARD} textarea.note`);
    const reread = async () => {
      const before = gets;
      await t.page.evaluate(() => window.dispatchEvent(new Event('online')));
      for (let i = 0; i < 50 && gets === before; i++) await t.page.waitForTimeout(100);
      await t.page.waitForTimeout(300);
      return gets > before;
    };
    // an idle review: the cards close under it, then a new question arrives
    await t.page.click('.q-send .primary');
    await t.page.waitForSelector('.review');
    current = empty;
    await t.check('re-read (cards gone)', await reread());
    await t.text('.sec-questions .q-empty', /No questions waiting/);
    current = full;
    await t.check('re-read (a question back)', await reread());
    await t.check('the question opens on the deck, not the old review', async () =>
      t.page.evaluate(
        () => !!document.querySelector('article.q-card') && !document.querySelector('.review')
      )
    );
    // a send that fails after the cards emptied
    await t.page.fill('textarea.note', 'one note');
    await t.page.click('.q-send .primary');
    await t.page.click('.review .q-send .primary');
    current = empty;
    await t.check('re-read mid-send', await reread());
    release();
    await t.page.waitForSelector('.review [role="alert"]', { timeout: 10000 });
    await t.text('.review [role="alert"]', /nothing is left to send/);
    await t.check('the failure says nothing about sending again', async () =>
      t.page.evaluate(
        () =>
          !/Sending the same answers again/.test(
            document.querySelector('.review [role="alert"]').textContent
          )
      )
    );
    await t.page.click('.review .q-send button >> text=Back');
    await t.text('.sec-questions .q-empty', /No questions waiting/);
    // the 500 this test answered on purpose is the only console error allowed
    await t.noConsoleErrors('console (only the stubbed 500 on /api/status/answers)', {
      allow: [/status of 500 .*\/api\/status\/answers/],
    });
  },

  // With dashboard sign-in off the server refuses both tabs in its own words (403 signin_required): the page says that
  // sentence, not "The server answered 403.", and stops asking until the setting changes.
  async 'status and scoreboard say why the server refuses them'(t) {
    await t.viewport(1440, 900);
    await t.app.signIn();
    const refusal = {
      error: 'signin_required',
      message: 'These tabs need dashboard sign-in turned on.',
    };
    let reads = 0;
    for (const api of ['/api/status', '/api/scoreboard']) {
      await t.page.route(
        (u) => new URL(u).pathname === api,
        (r) => {
          reads++;
          return r.fulfill({ status: 403, json: refusal });
        }
      );
    }
    await t.goto('/app/status', { settleMs: 300 });
    await t.until(
      "Status says the server's sentence",
      () =>
        /These tabs need dashboard sign-in turned on\./.test(
          document.querySelector('.route:not([hidden]) .banner')?.textContent || ''
        ),
      null,
      { timeout: 15000 }
    );
    await t.check(
      'no bare status code',
      !/answered 403/.test(await t.page.locator('.route:not([hidden])').textContent())
    );
    await t.goto('/app/scoreboard', { settleMs: 300 });
    await t.until(
      "Scoreboard says the server's sentence",
      () =>
        /These tabs need dashboard sign-in turned on\./.test(
          document.querySelector('.route:not([hidden]) .banner')?.textContent || ''
        ),
      null,
      { timeout: 15000 }
    );
    const settled = reads;
    await t.page.waitForTimeout(35000);
    await t.check(`no re-read while refused (${reads - settled} more)`, reads - settled <= 1);
    await t.noConsoleErrors('console (only the refused reads)', { allow: [/status of 403/] });
  },
};
