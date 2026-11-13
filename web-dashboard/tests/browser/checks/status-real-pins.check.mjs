// Opt-in: every real pinned thread as the Status page draws it, row and sheet, written to the run folder as
// real-pins-<format>.json so two builds can be compared line by line (2026-11-13 status regression).
//   browser-check run web-dashboard/tests/browser/checks --grep 'status real pins' \
//     --set realPins=1 --set reports=<copy of the reports share> [--set registry=<copy>] [--set repo=<other tree>] [--set shoot=<title>]
// The server reads a copy of the reports (never the share) and this machine's T3 pins read-only; the doorbell is off
// and nothing is answered. Without realPins the test records that it was skipped.
import fs from 'node:fs';
import path from 'node:path';

const FORMATS = [
  { name: 'desktop', w: 1440, h: 900, mobile: false },
  { name: 'phone', w: 402, h: 812, mobile: true },
];

/** One roster row, field by field, in the page's own classes (the same in every build since the C v2 roster). */
function readRows(t) {
  return t.page.$$eval('.c2-row', (rows) =>
    rows.map((row) => {
      const text = (sel) => row.querySelector(sel)?.textContent.trim() ?? null;
      return {
        title: text('.c2-title'),
        age: text('.c2-age'),
        active: text('.c2-active'),
        word: text('.c2-l2 b'),
        lead: text('.c2-l2'),
        track: row.querySelectorAll('.c2-l3 .fill, .c2-l3 i').length,
        stage: text('.c2-stage'),
        model: text('.c2-model'),
        problem: text('.c2-prob'),
      };
    })
  );
}

/** The open sheet: its heading, problems, every fact with its right column, each section's head and lines. */
function readSheet(t, sheet) {
  return t.page.$eval(sheet, (root) => {
    const clean = (s) => (s ?? '').replace(/\s+/g, ' ').trim();
    const facts = [];
    for (const dt of root.querySelectorAll('.fx dt')) {
      const main = dt.nextElementSibling;
      const right = main?.nextElementSibling;
      facts.push({ label: clean(dt.textContent), main: clean(main?.textContent), right: clean(right?.textContent) });
    }
    const sections = [...root.querySelectorAll('.c2-sec')].map((sec) => ({
      head: clean(sec.querySelector('.c2-sec-head')?.textContent),
      lines: [...sec.querySelectorAll('li, p')].map((el) => clean(el.textContent)).filter(Boolean),
    }));
    return {
      state: clean(root.querySelector('.c2-state')?.textContent),
      name: clean(root.querySelector('.c2-name')?.textContent),
      meta: clean(root.querySelector('.c2-meta')?.textContent),
      problems: [...root.querySelectorAll('.c2-prob')].map((p) => clean(p.textContent)),
      quiet: clean(root.querySelector('.c2-quiet')?.textContent) || null,
      facts,
      sections,
      lines: root.innerText.split('\n').map((l) => l.trim()).filter(Boolean),
    };
  });
}

export const tests = {
  async 'status real pins: every row and sheet'(t) {
    if (!t.args.realPins) {
      await t.record('real pins', true, 'skipped: pass --set realPins=1 --set reports=<copy> to read the real pins');
      return;
    }
    for (const format of FORMATS) {
      if (format.mobile)
        await t.newContext({ viewport: { width: format.w, height: format.h }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 });
      else await t.viewport(format.w, format.h);
      await t.app.signIn();
      await t.goto('/app/status', { settleMs: 300 });
      // The pins arrive with the first collect, after the reports scan: wait for the roster, not a fixed time.
      await t.until('the pinned roster is drawn', () => document.querySelectorAll('.c2-row').length > 0, undefined, { timeout: 120000, hard: true });
      const sheet = format.mobile ? '.c2-sheet-host .c2-sheet' : '.c2-pane .c2-sheet';
      const rows = await readRows(t);
      // The whole page's words too (headline, strip, sections), for a diff beyond the pinned plate.
      const page = await t.page.evaluate(() => document.body.innerText.split('\n').map((l) => l.trim()).filter(Boolean));
      const out = { format: format.name, at: new Date().toISOString(), page, rows: [] };
      for (let i = 0; i < rows.length; i++) {
        const row = t.page.locator('.c2-row').nth(i);
        await row.scrollIntoViewIfNeeded();
        await row.click();
        await t.page.waitForFunction(
          ([s, name]) => document.querySelector(`${s} .c2-name`)?.textContent.trim() === name,
          [sheet, rows[i].title],
          { timeout: 10000 }
        );
        const got = await readSheet(t, sheet);
        out.rows.push({ row: rows[i], sheet: got });
        if (t.args.shoot && rows[i].title.includes(String(t.args.shoot))) {
          if (format.mobile) await t.shot(`real-pin-${format.name}`, `${format.name}: ${rows[i].title}, its sheet`);
          else {
            // The whole plate in one image: every roster corner and the open sheet from its top, in a tall window.
            await t.viewport(format.w, 2400);
            await t.crop(`real-pin-${format.name}`, '.plate.sec-pinned .c2-body', `${format.name}: the roster's corners and ${rows[i].title}'s sheet`);
            await t.viewport(format.w, format.h);
          }
        }
        if (format.mobile) {
          await t.page.keyboard.press('Escape');
          await t.page.waitForFunction((s) => !document.querySelector(s), sheet, { timeout: 5000 }).catch(() => {});
        }
      }
      const file = path.join(t.run.dir, `real-pins-${format.name}.json`);
      fs.writeFileSync(file, JSON.stringify(out, null, 2));
      await t.check(`${format.name}: ${rows.length} pinned threads read`, rows.length > 0 && out.rows.length === rows.length, file);
    }
  },
};
