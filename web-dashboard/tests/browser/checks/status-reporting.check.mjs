// What agents report, as the Status page shows it (2026-11-13, status-reporting FINDINGS.md): a card's label as a
// chip leading every card (open deck, index, review, answered), waits on others beside an owner wait, the human
// update's age apart from the heartbeat's, a quiet thread's last doing line, and a shared stream board named as such.
// Synthetic data only: GET /api/status answers with a made-up view built on web-dashboard/leptos/fixtures, and
// POST /api/status/answers is answered in the page, so nothing reaches an answer store.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const LEPTOS = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../leptos');
const fixture = (name) => JSON.parse(fs.readFileSync(path.join(LEPTOS, 'fixtures', name), 'utf8'));

// iPhone 17 Pro, the Duo open upright, and a desktop window (status-page.check.mjs's poses).
const FORMATS = [
  { name: 'phone', w: 402, h: 812, safe: '0,0,34,0', mobile: true },
  { name: 'duo-open', w: 669, h: 869, safe: '0,0,34,0', mobile: true },
  { name: 'desktop', w: 1440, h: 900, safe: '0,0,0,0', mobile: false },
  // The Duo folded with its rail: the narrowest sheet, where a long time once squeezed a fact to a word a line.
  { name: 'duo-folded', w: 466, h: 678, safe: '0,84,34,0', mobile: true },
];

const OWNER_WAIT = 'Open the preview and answer card SM-0 and the design questions SM-1 to SM-8';
const SUMMARY = 'The review said READY; the press test is paused until the browsers are free.';
const DOING = "Reading the scout's notes for batch 3 of the onboarding review.";
// pm7-build's real shape on 2026-11-13: the hook rewrote thread.json 20 min ago and carries this doing line; the status
// (and these words) is 2 h old. Wave 8 kept the words but dropped from the roster that the thread was active.
const ALIVE_DOING = 'Morning: PM7 12a is on main and the dialog fix waits on card Q7; batch 2 is fixing one review finding.';

/** Three open cards as agents label them: a plain label, a label unlike its id, and one that stored none. */
function cards() {
  const seed = { ...fixture('status-design-view.json').cards.open[2] };
  const card = (id, label, title, writer, askedBy) => ({
    ...seed,
    id,
    label,
    title,
    writer,
    askedBy,
    kind: 'question',
    askedAt: new Date(Date.now() - 3_600_000).toISOString(),
    ageSeconds: 3600,
    sections: [],
    evidence: [{ label: 'Lead state', path: '/mnt/Cursor/share/example/LEAD-STATE.md' }],
  });
  return [
    card('vm.status-lead--R8', 'R8', 'Install the status fixes tonight, or wait for the morning build', 'vm.status-lead', 'Status lead'),
    card('vm.pm7-build--Q1', 'PM7-Q1', 'Which reviewer should re-check the onboarding sweep before it lands', 'vm.pm7-build', 'PM7 build'),
    card('vm.lanes--Q9', '', 'Keep the work lanes as five or fold them into three', 'vm.lanes', 'Lanes lead'),
  ];
}

const update = (fields) => ({
  machine: 'vm',
  reportState: 'fresh',
  percent: null,
  percentNote: null,
  blocked: null,
  looping: { answer: 'no', text: 'No.' },
  left: null,
  done: null,
  reportNote: null,
  staleNote: null,
  unreadableNote: null,
  summary: null,
  ...fields,
});

/** The settings-managers shape, as the composer sends it now; a quiet thread with no status file; a shared board. */
function pins() {
  const now = Date.now();
  const iso = (ago) => new Date(now - ago * 1000).toISOString();
  return {
    asOfMs: now,
    head: '6 pinned in T3 · 1 looping · 1 waiting on you',
    counts: { pinned: 6, looping: 1, waitingOnYou: 1 },
    machineProblems: [],
    statusProblems: [],
    updates: [
      update({
        id: 'pin-settings',
        title: 'Settings managers',
        who: 'Puppet Master · settings-lead · Fable 5.2 high',
        now: SUMMARY,
        summary: SUMMARY,
        percent: { text: '93%', done: 14, of: 15 },
        blocked: {
          text: `Waiting on you: ${OWNER_WAIT}; Answers to the work-lanes cards WL1 to WL5. Blocked: Browsers on the VM and Nas1 are reserved (waiting on resource); Second reviewer pass on the page frame (waiting on review); The fix worker's rebuild (waiting on agent).`,
          waitingOnYou: true,
        },
        left: [
          { id: 'SWEEP', title: 'Press test', state: 'blocked' },
          { id: 'PAGE-FRAME', title: 'Page frame', state: 'review' },
        ],
        done: [{ id: 'SHARE', title: 'Share', state: 'done' }],
        updated: { text: 'Updated 2 min ago', at: iso(120), ageSeconds: 120 },
        statusUpdated: { text: 'Updated 1 day ago', at: iso(35 * 3600), ageSeconds: 35 * 3600 },
        heartbeat: { text: 'Active 2 min ago', at: iso(120), ageSeconds: 120 },
        board: { writer: 'vm.settings-lead', project: 'puppet-master', stream: 'SETTINGS-MANAGERS', projectLabel: 'Puppet Master', streamLabel: 'Settings managers', shared: false },
        waits: [
          { on: 'owner', text: OWNER_WAIT, from: 'thread', since: iso(37 * 3600) },
          { on: 'resource', text: 'Browsers on the VM and Nas1 are reserved for stream F.', task: 'SWEEP', from: 'tasks', since: iso(36 * 3600), until: iso(30 * 3600) },
          { on: 'owner', text: 'Answers to the work-lanes cards WL1 to WL5.', task: 'WORK-LANES', from: 'tasks' },
          { on: 'review', text: 'Second reviewer pass on the page frame.', task: 'PAGE-FRAME', from: 'tasks' },
          { on: 'agent', text: "The fix worker's rebuild.", short: 'Rebuild after the GLM fixes', task: 'GLM-FIXES', from: 'tasks' },
        ],
        reported: { state: 'blocked', writer: 'vm.settings-lead' },
      }),
      update({
        id: 'pin-carried',
        title: 'Carried doing line',
        who: 'Puppet Master · rebase-worker · Opus 5.5 high',
        // The hook carries thread.json's doing line; it differs from the 20-hour-old summary.
        now: 'Rebasing the lane branch onto main.',
        summary: 'Batch 3 is in review.',
        blocked: { text: 'None.', waitingOnYou: false },
        updated: { text: 'Updated 1 min ago', at: iso(60), ageSeconds: 60 },
        statusUpdated: { text: 'Updated 20 hours ago', at: iso(20 * 3600), ageSeconds: 20 * 3600 },
        heartbeat: { text: 'Active 1 min ago', at: iso(60), ageSeconds: 60 },
        reported: { state: 'working', writer: 'vm.rebase-worker' },
      }),
      update({
        id: 'pin-scout',
        title: 'Scout notes',
        who: 'scout · Opus 5.5',
        reportState: 'stale',
        staleNote: 'No status update for 2 days. If it is finished, unpin it in T3.',
        now: DOING,
        updated: { text: 'Updated 2 days ago', at: iso(2 * 86400 + 600), ageSeconds: 2 * 86400 + 600 },
        heartbeat: { text: 'Active 2 days ago', at: iso(2 * 86400 + 600), ageSeconds: 2 * 86400 + 600 },
        looping: { answer: 'cantTell', text: "Can't tell." },
        // Its last task list, from the same 2-day-old report: a quiet sheet still shows it, with its age.
        percent: { text: '50%', done: 1, of: 2 },
        left: [{ id: 'SCOUT-3', title: 'Scout batch 3', state: 'working' }],
        done: [{ id: 'SCOUT-2', title: 'Scout batch 2', state: 'done' }],
        reported: { state: 'idle', writer: 'vm.scout' },
      }),
      update({
        id: 'pin-gone',
        title: 'Old notes',
        who: 'notes · Opus 5.5',
        reportState: 'stale',
        staleNote: 'No status update for 3 days. If it is finished, unpin it in T3.',
        now: 'Filed the last notes.',
        updated: { text: 'Updated 3 days ago', at: iso(3 * 86400), ageSeconds: 3 * 86400 },
        heartbeat: { text: 'Active 3 days ago', at: iso(3 * 86400), ageSeconds: 3 * 86400 },
        reported: { state: 'idle', writer: 'vm.notes' },
      }),
      update({
        id: 'pin-split',
        title: 'Split v3',
        who: 'Puppet Master · split-worker · Opus 5.5 high',
        now: 'Splitting the v3 files.',
        summary: 'Splitting the v3 files.',
        percent: { text: '0%', done: 0, of: 1 },
        blocked: { text: 'None.', waitingOnYou: false },
        looping: {
          answer: 'yes',
          text: "Yes. On the shared Projects board kept by vm.projects-lead, P-BATCH2 has gone back for fixes 4 times without a passing review. This thread's own report says it is not looping.",
          task: 'P-BATCH2',
          rounds: 4,
        },
        left: [{ id: 'P-BATCH2', title: 'Batch 2', state: 'fixing' }],
        done: [],
        updated: { text: 'Updated 3 min ago', at: iso(200), ageSeconds: 200 },
        statusUpdated: { text: 'Updated 3 min ago', at: iso(200), ageSeconds: 200 },
        heartbeat: { text: 'Active 3 min ago', at: iso(200), ageSeconds: 200 },
        board: { writer: 'vm.projects-lead', project: 'puppet-master', stream: 'P', projectLabel: null, streamLabel: 'Projects', shared: true },
        reported: { state: 'working', writer: 'vm.split-worker' },
      }),
      update({
        id: 'pin-alive',
        title: 'Background build',
        who: 'Puppet Master · background-build · Opus 5.5 xhigh',
        now: ALIVE_DOING,
        summary: ALIVE_DOING,
        percent: { text: '50%', done: 2, of: 4 },
        blocked: { text: 'None.', waitingOnYou: false },
        left: [
          { id: 'BG-3', title: 'Background activity sweep', state: 'review' },
          { id: 'BG-4', title: 'Land batch 2', state: 'todo' },
        ],
        done: [
          { id: 'BG-1', title: 'Idle timer', state: 'done' },
          { id: 'BG-2', title: 'Wake on focus', state: 'done' },
        ],
        updated: { text: 'Updated 20 min ago', at: iso(1200), ageSeconds: 1200 },
        statusUpdated: { text: 'Updated 2 hours ago', at: iso(7200), ageSeconds: 7200 },
        heartbeat: { text: 'Active 20 min ago', at: iso(1200), ageSeconds: 1200 },
        reported: {
          state: 'working',
          // not vm.pm7-build: that writer has an open card here, which would make it wait on the owner
          writer: 'vm.background-build',
          next: 'Land batch 2 once its review passes.',
          stages: [
            { label: 'Build', state: 'done' },
            { label: 'Review', state: 'now', round: 2 },
            { label: 'Land', state: 'next' },
          ],
        },
      }),
    ],
  };
}

function view() {
  const base = fixture('status-design-view.json');
  const answered = fixture('status-view.json').cards.answered.map((a) => ({
    ...a,
    card: { ...a.card, id: 'vm.status-lead--R7', label: 'R7', title: 'Ship the strip on its own branch' },
  }));
  return {
    ...base,
    spendByStream: fixture('spend-by-stream.json'),
    cards: { ...base.cards, open: cards(), answered, batches: [] },
    pinned: pins(),
    // The Projects lead's board that Split v3 works on: its items and the list's own time (20 h old).
    projects: [
      ...(base.projects ?? []),
      {
        id: 'puppet-master-p',
        label: 'Puppet Master',
        streams: [
          {
            id: 'P',
            label: 'Projects',
            owner: 'vm.projects-lead',
            goal: 'Projects page',
            source: 'tasks',
            asOf: new Date(Date.now() - 20 * 3600 * 1000).toISOString(),
            stale: false,
            items: [{ id: 'P-BATCH2', title: 'Batch 2', state: 'fixing', reviews: [] }],
          },
        ],
      },
    ],
  };
}

async function openStatus(t, format, data) {
  if (format.mobile)
    await t.newContext({ viewport: { width: format.w, height: format.h }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 });
  else await t.viewport(format.w, format.h);
  await t.app.signIn();
  await t.page.route((u) => new URL(u).pathname === '/api/status', (r) => r.fulfill({ json: data }));
  await t.page.route(
    (u) => new URL(u).pathname === '/api/status/answers',
    async (r) => {
      const body = JSON.parse(r.request().postData() || '{}');
      await r.fulfill({ json: { results: (body.answers || []).map((a) => ({ cardId: a.cardId, outcome: 'recorded' })) } });
    }
  );
  await t.goto(`/app/status?safe=${format.safe}`, { settleMs: 300 });
  await t.page.waitForSelector('#cards');
}

/** The label chip of the first element matching `scope`: text, place and whether it shares its line with the title. */
function chip(t, scope) {
  return t.page.evaluate((scope) => {
    const host = document.querySelector(scope);
    const el = host?.querySelector('.q-label');
    if (!el) return null;
    const r = el.getBoundingClientRect();
    const lead = el.closest('.q-lead');
    const cs = getComputedStyle(el);
    const titleFont = getComputedStyle(host).fontFamily;
    // The label is the first thing in its title: nothing drawn before it.
    const first = (() => {
      const walker = document.createTreeWalker(host, NodeFilter.SHOW_TEXT);
      for (let n = walker.nextNode(); n; n = walker.nextNode()) {
        if (!n.textContent.trim()) continue;
        return el.contains(n);
      }
      return false;
    })();
    // The label and the title's first word on one line: the lead run is a single line box.
    const leadRects = lead ? [...lead.getClientRects()] : [];
    const word = lead ? lead.lastChild : null;
    const range = document.createRange();
    if (word) range.selectNodeContents(word);
    const w = word ? range.getBoundingClientRect() : null;
    return {
      text: el.textContent,
      id: el.getAttribute('title'),
      shown: r.width > 0 && r.height > 0 && r.left >= 0 && r.right <= innerWidth + 0.5,
      first,
      oneLine: leadRects.length === 1 && !!w && Math.abs(w.top + w.height / 2 - (r.top + r.height / 2)) < r.height,
      outlined: parseFloat(cs.borderTopWidth) >= 1 && cs.borderTopStyle === 'solid',
      unfilled: cs.backgroundColor === 'rgba(0, 0, 0, 0)' || cs.backgroundColor === 'transparent',
      ownType: cs.fontFamily !== titleFont,
      notPill: parseFloat(cs.borderTopLeftRadius) <= 4,
    };
  }, scope);
}

const NEXT = '.q-send > button:nth-of-type(2)';
const CARD = 'article.q-card';

export const tests = {
  // The owner, 2026-11-13: "I don't see the card labels on the decision card that were asked for. So when a chat
  // references a card, the label matches."
  async 'status card label chip leads every card'(t) {
    const data = view();
    const want = ['R8', 'PM7-Q1', 'Q9'];
    for (const format of FORMATS) {
      await openStatus(t, format, data);
      await t.page.waitForSelector(CARD);
      for (let i = 0; i < want.length; i++) {
        const got = await chip(t, `${CARD} .q-title`);
        await t.check(`${format.name}: card ${i + 1} leads with its label chip`, got?.text === want[i] && got.shown && got.first, JSON.stringify(got));
        await t.check(`${format.name}: ${want[i]} shares its line with the title's first word`, !!got?.oneLine, JSON.stringify(got));
        await t.check(`${format.name}: ${want[i]} is a distinct chip (outlined, its own type, unfilled, not a pill)`, !!(got?.outlined && got.ownType && got.unfilled && got.notPill), JSON.stringify(got));
        await t.equal(`${format.name}: ${want[i]} carries its full id`, got?.id, data.cards.open[i].id);
        if (i === 0 && format.name === 'phone') await t.shot('label-chip-phone', 'phone: the open card leads with its label chip, R8');
        if (i === 0 && format.name === 'duo-open') await t.shot('label-chip-duo-open', 'Duo open: the open card leads with its label chip');
        if (i < want.length - 1) await t.page.click(NEXT);
      }
      // The full writer-prefixed id is in the card's technical detail, for a chat that quoted it.
      await t.page.click(`${CARD} .q-more .disclosure`);
      await t.text(`${CARD} .q-tech .q-id`, /Card id: vm\.lanes--Q9/);
    }

    // The question index leads each row with the chip, and finds a card by label, id suffix or full id.
    await openStatus(t, FORMATS[2], data);
    await t.page.waitForSelector(CARD);
    await t.check('index rows lead with their labels', async () =>
      JSON.stringify(await t.page.$$eval('.q-list .q-item .q-t > .q-lead > .q-label:first-child', (l) => l.map((x) => x.textContent))) === JSON.stringify(want));
    for (const [typed, label] of [['vm.pm7-build--Q1', 'PM7-Q1'], ['q9', 'Q9'], ['R8', 'R8']]) {
      await t.page.fill('.q-list input[type="search"]', typed);
      await t.text(`${CARD} .q-title .q-label`, label);
    }
    await t.page.fill('.q-list input[type="search"]', '');

    // Review step: the chip leads the reviewed card's title.
    await t.page.click(`${CARD} .opt-pick >> nth=0`);
    await t.page.click('.q-send .primary');
    await t.page.waitForSelector('.review');
    const review = await chip(t, '.review .review-title');
    await t.check('review: the reviewed card leads with its chip', review?.text === 'R8' && review.first && review.oneLine, JSON.stringify(review));

    // Answered history: the chip leads each answered card.
    await openStatus(t, FORMATS[2], data);
    await t.page.click('.q-foot-actions .disclosure');
    const answered = await chip(t, 'ul.answered li b');
    await t.check('answered: the answered card leads with its chip', answered?.text === 'R7' && answered.first, JSON.stringify(answered));
  },

  async 'status waits on others show beside an owner wait'(t) {
    for (const format of [FORMATS[2], FORMATS[0], FORMATS[3]]) {
      await openStatus(t, format, view());
      const sheet = format.mobile ? '.c2-sheet-host .c2-sheet' : '.c2-pane .c2-sheet';
      await t.page.locator('.c2-row', { hasText: 'Settings managers' }).click();
      await t.page.waitForSelector(`${sheet} .fx`);
      const facts = await t.page.$$eval(`${sheet} .fx > *`, (els) => els.map((e) => e.textContent.trim()));
      const at = (label) => facts[facts.indexOf(label) + 1] ?? '';
      await t.check(`${format.name}: For you says the owner wait`, at('For you').includes(OWNER_WAIT), at('For you'));
      await t.check(`${format.name}: For you holds no one else's wait`, !/Browsers|reviewer|rebuild/i.test(at('For you')), at('For you'));
      const waiting = at('Waiting on');
      for (const [head, words] of [['A machine (SWEEP)', 'Browsers on the VM and Nas1'], ['A review (PAGE-FRAME)', 'Second reviewer pass'], ['An agent (GLM-FIXES)', 'Rebuild after the GLM fixes']])
        await t.check(`${format.name}: Waiting on shows ${head}`, waiting.includes(head) && waiting.includes(words), waiting);
      await t.check(`${format.name}: not "Nothing else" while others are waited on`, !/Nothing else/.test(waiting), waiting);
      if (format.mobile) {
        // A long time ("was due Nov 12, 1:20 AM") goes under its fact; the fact keeps most of the sheet's width.
        const fit = await t.page.evaluate((s) => {
          const fx = document.querySelector(`${s} .fx`).getBoundingClientRect();
          const dds = [...document.querySelectorAll(`${s} .fx dd:not(.t)`)].map((d) => d.getBoundingClientRect().width);
          return { share: Math.min(...dds) / fx.width, fx: fx.width };
        }, sheet);
        await t.check(`${format.name}: every fact keeps at least half the sheet's width`, fit.share >= 0.5, JSON.stringify(fit));
      }
      const sheetText = await t.page.locator(`${sheet} .fx`).textContent();
      await t.equal(`${format.name}: the owner wait shows once`, sheetText.split(OWNER_WAIT).length - 1, 1);
      if (!format.mobile) await t.shot('waits-and-ages-desktop', 'desktop: the owner wait once, the other waits beside it, and the two ages');
    }
  },

  async 'status an old summary shows its own age beside the heartbeat'(t) {
    for (const format of [FORMATS[2], FORMATS[0]]) {
      await openStatus(t, format, view());
      const sheet = format.mobile ? '.c2-sheet-host .c2-sheet' : '.c2-pane .c2-sheet';
      await t.page.locator('.c2-row', { hasText: 'Settings managers' }).click();
      await t.page.waitForSelector(`${sheet} .fx`);
      const facts = await t.page.$$eval(`${sheet} .fx > *`, (els) => els.map((e) => e.textContent.trim()));
      const i = facts.indexOf('Updated');
      await t.check(`${format.name}: Updated says both ages, the human update first`, /^updated 35 h ago · active 2 min ago$/.test(facts[i + 1]), facts[i + 1]);
      const n = facts.indexOf('Now');
      await t.check(`${format.name}: the carried Now line says its age`, /^as of 35 h ago$/.test(facts[n + 2]), facts[n + 2]);
      if (format.mobile) await t.shot('waits-and-ages-phone', 'phone: the waits and the two ages in the sheet');
      // A carried doing line that differs from the summary is dated the same way (review 1, L4).
      if (format.mobile) await t.page.keyboard.press('Escape');
      await t.page.locator('.c2-row', { hasText: 'Carried doing line' }).click();
      await t.page.waitForFunction((s) => document.querySelector(`${s} .c2-name`)?.textContent === 'Carried doing line', sheet);
      const carried = await t.page.$$eval(`${sheet} .fx > *`, (els) => els.map((e) => e.textContent.trim()));
      const c = carried.indexOf('Now');
      await t.check(`${format.name}: a carried doing line says the summary's age`, carried[c + 1] === 'Rebasing the lane branch onto main.' && carried[c + 2] === 'as of 20 h ago', JSON.stringify(carried.slice(c, c + 3)));
      if (format.mobile) await t.page.keyboard.press('Escape');
      // The roster row's corner says the summary's age, never the heartbeat's 2 min.
      if (format.mobile) await t.page.keyboard.press('Escape');
      const corner = (await t.page.locator('.c2-row', { hasText: 'Settings managers' }).locator('.c2-age').textContent()).trim();
      await t.equal(`${format.name}: the roster row says the summary's age`, corner, '35 h');
    }
  },

  async 'status a quiet thread shows its last doing line'(t) {
    for (const format of [FORMATS[2], FORMATS[0]]) {
      await openStatus(t, format, view());
      const sheet = format.mobile ? '.c2-sheet-host .c2-sheet' : '.c2-pane .c2-sheet';
      await t.page.locator('.c2-row', { hasText: 'Scout notes' }).click();
      await t.page.waitForSelector(`${sheet} .c2-quiet`);
      await t.text(`${sheet} .c2-quiet`, /No status update for 2 days\./);
      await t.text(`${sheet} .c2-quiet`, new RegExp(`Its last doing line, 2 d ago: ${DOING.replace(/[.']/g, '.')}`));
      // Quiet hides nothing it reported (2026-11-13): its tasks and progress stay under the note, the doing line once.
      const tasks = await t.page.locator(`${sheet} .c2-sec`, { hasText: 'Tasks' }).last().innerText().catch(() => '');
      await t.check(`${format.name}: a quiet sheet still lists its tasks`, tasks.includes('Scout batch 3'), tasks);
      await t.check(`${format.name}: a quiet sheet still shows its progress`, (await t.page.locator(`${sheet} .c2-sec`, { hasText: 'Stages' }).innerText().catch(() => '')).includes('1 of 2 tasks done'));
      const said = await t.page.locator(sheet).innerText();
      await t.equal(`${format.name}: the doing line shows once`, said.split("Reading the scout's notes").length - 1, 1);
      if (format.mobile) await t.page.keyboard.press('Escape');
      // A quiet thread that reported nothing but words keeps its note alone, not three "nothing reported" sections.
      await t.page.locator('.c2-row', { hasText: 'Old notes' }).click();
      await t.page.waitForFunction((s) => document.querySelector(`${s} .c2-name`)?.textContent === 'Old notes', sheet);
      await t.text(`${sheet} .c2-quiet`, /No status update for 3 days\..*Its last doing line, 3 d ago: Filed the last notes\./);
      await t.count(`${sheet} .c2-sec`, 0, `${format.name}: a quiet thread with only words shows its note alone`);
      if (format.mobile) await t.page.keyboard.press('Escape');
    }
  },

  // Opt-in: `--set realView=<view.json>` draws a Status view built offline from a read-only copy of the reports share
  // (never the share itself, never a real answer), and checks the same things on real cards and pins. Without it the
  // test only records that it was skipped.
  async 'status on a real reports copy'(t) {
    if (!t.args.realView) {
      await t.record('real view', true, 'skipped: pass --set realView=<view.json> to draw a real reports copy');
      return;
    }
    const data = JSON.parse(fs.readFileSync(String(t.args.realView), 'utf8'));
    const labels = new Map(data.cards.open.map((c) => [c.id, c.label || c.id.split('--').pop()]));
    for (const format of [FORMATS[0], FORMATS[1]]) {
      await openStatus(t, format, data);
      await t.page.waitForSelector(CARD);
      const got = await chip(t, `${CARD} .q-title`);
      await t.check(`${format.name}: the first real card leads with its stored label`, got?.text === labels.get(got?.id) && got.first && got.oneLine, JSON.stringify(got));
      await t.shot(`real-card-${format.name}`, `${format.name}: a real open card with its label chip`);
    }
    // The index lists every real card's label, verbatim, as the reports stored it.
    await openStatus(t, FORMATS[2], data);
    await t.page.waitForSelector(CARD);
    const shown = await t.page.$$eval('.q-list .q-item .q-label', (l) => l.map((x) => [x.getAttribute('title'), x.textContent]));
    await t.check('index: each shown label is its card\'s stored label', shown.length > 0 && shown.every(([id, text]) => labels.get(id) === text), JSON.stringify(shown.slice(0, 5)));
    const settings = data.pinned.updates.find((u) => (u.waits ?? []).some((w) => w.on !== 'owner') && (u.waits ?? []).some((w) => w.on === 'owner'));
    if (settings) {
      await t.page.locator('.c2-row', { hasText: settings.title }).first().click();
      await t.page.waitForSelector('.c2-pane .c2-sheet .fx');
      const facts = await t.page.$$eval('.c2-pane .c2-sheet .fx > *', (els) => els.map((e) => e.textContent.trim()));
      const waiting = facts[facts.indexOf('Waiting on') + 1] ?? '';
      await t.check(`${settings.title}: its other waits show beside the owner wait`, !/Nothing else/.test(waiting) && waiting.length > 0, waiting);
      await t.shot('real-waits-desktop', `desktop: ${settings.title}, a real owner wait with the waits on others beside it`);
    }
  },

  async 'status a shared stream board is named'(t) {
    await openStatus(t, FORMATS[2], view());
    await t.page.locator('.c2-row', { hasText: 'Split v3' }).click();
    await t.page.waitForSelector('.c2-pane .c2-sheet');
    await t.text('.c2-pane .c2-sec-end[data-board="shared"]', 'Shared Projects board, kept by vm.projects-lead');
    await t.text('.c2-pane .c2-prob', /On the shared Projects board kept by vm\.projects-lead, P-BATCH2/);
  },

  // 2026-11-13 regression (owner: "even less data on threads that were decent before"). On the owner's real pins wave 8
  // dropped: the roster's activity age, a shared-board worker's task-list time and the task and rounds of its loop line,
  // and on a phone each wait's time. Every other line stayed; these checks keep all of it.
  async 'status a live thread with an older summary keeps everything it showed'(t) {
    for (const format of [FORMATS[2], FORMATS[0]]) {
      await openStatus(t, format, view());
      const sheet = format.mobile ? '.c2-sheet-host .c2-sheet' : '.c2-pane .c2-sheet';
      const row = t.page.locator('.c2-row', { hasText: 'Background build' });
      // The roster: the words' age, and under it that the thread was active 20 min ago (the old corner's news).
      await t.equal(`${format.name}: the corner says the words' age`, (await row.locator('.c2-age').textContent()).trim(), '2 h');
      await t.equal(`${format.name}: the corner says when it was last active`, (await row.locator('.c2-active').textContent().catch(() => '')).trim(), 'active 20 min');
      await t.check(`${format.name}: the row leads with its doing line`, (await row.locator('.c2-l2').textContent()).includes('Morning: PM7 12a is on main'));
      await t.check(`${format.name}: the row shows its stage progress`, /2 of 3/.test(await row.locator('.c2-stage').textContent()));
      await row.click();
      await t.page.waitForFunction((s) => document.querySelector(`${s} .c2-name`)?.textContent === 'Background build', sheet);
      const facts = await t.page.$$eval(`${sheet} .fx > *`, (els) => els.map((e) => e.textContent.trim()));
      const at = (label, k = 1) => facts[facts.indexOf(label) + k] ?? '';
      await t.equal(`${format.name}: Now keeps the doing line`, at('Now'), ALIVE_DOING);
      await t.equal(`${format.name}: Now says how old the words are`, at('Now', 2), 'as of 2 h ago');
      await t.equal(`${format.name}: Next is shown`, at('Next'), 'Land batch 2 once its review passes.');
      await t.equal(`${format.name}: Updated says both ages`, at('Updated'), 'updated 2 h ago · active 20 min ago');
      const stages = await t.page.$$eval(`${sheet} .c2-stages .lab`, (l) => l.map((x) => x.textContent.trim()));
      await t.check(`${format.name}: every stage is drawn`, JSON.stringify(stages) === JSON.stringify(['Build', 'Review, round 2', 'Land']), JSON.stringify(stages));
      const tasks = await t.page.locator(`${sheet} .c2-sec`, { hasText: 'Tasks' }).last().innerText();
      for (const title of ['Background activity sweep', 'Land batch 2'])
        await t.check(`${format.name}: Tasks lists ${title}`, tasks.includes(title), tasks);
      await t.check(`${format.name}: the task list says how old it is`, /as of .*, 2 h old/.test(tasks), tasks);
      if (format.mobile) await t.page.keyboard.press('Escape');
      // A thread with no stages still shows its progress in the row (Settings managers: 14 of 15 tasks done).
      const settings = t.page.locator('.c2-row', { hasText: 'Settings managers' });
      await t.check(`${format.name}: a row without stages shows its task progress`, /14 of 15 tasks done/.test(await settings.locator('.c2-stage').textContent()));
      await t.equal(`${format.name}: Settings managers says it was active 2 min ago`, (await settings.locator('.c2-active').textContent().catch(() => '')).trim(), 'active 2 min');
      if (!format.mobile) await t.shot('regression-alive-desktop', 'desktop: a live thread with a 2 h old summary: corner 2 h over active 20 min, Now dated, stages and tasks');
    }
  },

  async 'status a worker on a shared board keeps its tasks, their time and its loop'(t) {
    for (const format of [FORMATS[2], FORMATS[0]]) {
      await openStatus(t, format, view());
      const sheet = format.mobile ? '.c2-sheet-host .c2-sheet' : '.c2-pane .c2-sheet';
      const row = t.page.locator('.c2-row', { hasText: 'Split v3' });
      const problem = (await row.locator('.c2-prob').textContent()).trim();
      await t.check(`${format.name}: the row's loop line keeps the task and its rounds`, problem.includes('P-BATCH2') && problem.includes('4 times') && /shared board/.test(problem), problem);
      await t.check(`${format.name}: the row shows the worker's task progress`, /0 of 1 tasks done/.test(await row.locator('.c2-stage').textContent()));
      await row.click();
      await t.page.waitForFunction((s) => document.querySelector(`${s} .c2-name`)?.textContent === 'Split v3', sheet);
      const head = (await t.page.locator(`${sheet} .c2-sec-end[data-board="shared"]`).innerText()).replace(/\s+/g, ' ').trim();
      await t.check(`${format.name}: the Tasks head names the board's keeper`, head.includes('Shared Projects board, kept by vm.projects-lead'), head);
      await t.check(`${format.name}: the Tasks head keeps the list's own time`, /as of .*20 h old/.test(head), head);
      const tasks = await t.page.locator(`${sheet} .c2-sec`, { hasText: 'Tasks' }).last().innerText();
      await t.check(`${format.name}: the worker still lists its tasks`, tasks.includes('Batch 2'), tasks);
      await t.text(`${sheet} .c2-prob`, /On the shared Projects board kept by vm\.projects-lead, P-BATCH2 has gone back for fixes 4 times/);
      if (format.mobile) await t.shot('regression-shared-phone', 'phone: a worker on a shared board: its loop, its tasks, the keeper and the list time');
    }
  },

  async 'status each wait keeps its own time'(t) {
    for (const format of [FORMATS[2], FORMATS[0], FORMATS[3]]) {
      await openStatus(t, format, view());
      const sheet = format.mobile ? '.c2-sheet-host .c2-sheet' : '.c2-pane .c2-sheet';
      await t.page.locator('.c2-row', { hasText: 'Settings managers' }).click();
      await t.page.waitForSelector(`${sheet} .fx`);
      const got = await t.page.evaluate((s) => {
        const dt = [...document.querySelectorAll(`${s} .fx dt`)].find((d) => d.textContent.trim() === 'Waiting on');
        const main = dt?.nextElementSibling;
        const right = main?.nextElementSibling;
        const sweep = [...(main?.querySelectorAll('p') ?? [])].find((p) => p.textContent.includes('(SWEEP)'));
        const time = sweep?.querySelector('.fx-when');
        const top = (el) => el.getBoundingClientRect().top;
        return {
          sweepTime: time?.textContent.trim() ?? null,
          // drawn inside its own wait, under its words, not beside another wait
          inside: !!time && top(time) >= top(sweep) && time.getBoundingClientRect().bottom <= sweep.getBoundingClientRect().bottom + 0.5,
          right: right?.textContent.trim() ?? null,
        };
      }, sheet);
      await t.check(`${format.name}: the SWEEP wait carries its own time`, /^was due /.test(got.sweepTime ?? '') && got.inside, JSON.stringify(got));
      await t.equal(`${format.name}: no times stacked apart from their waits`, got.right, '');
    }
  },
};
