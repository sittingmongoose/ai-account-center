#!/usr/bin/env node
// D7 Home spec: the dashboard's own JS tests, turned into a case table that both builders run.
//
//   node tests/spec/views/home/generate.mjs           record and rewrite cases.json
//   node tests/spec/views/home/generate.mjs --check   fail when cases.json is not what the JS gives today
//
// Recording runs every web-dashboard/tests/*.test.mjs with record-hook.mjs, which notes each Home build the tests
// make (view-model.mjs `dashboardViewModel`: its data, the profiles, the Antigravity inventory and the page's
// context). synthetic.mjs adds the inputs the tests do not reach (other zones, the decline rules). Each case is then
// run through the real view-model.mjs here: `expected` is the exact JSON.stringify of the JS view, `presentation`
// the M3 envelope facts the retired TS front door computed for it (frozen in cases.json since src/ was deleted),
// `ctx.formatProbe` this engine's formatter outputs (or a synthetic case's other
// proven style) and `ctx.zone` this engine's display-zone offsets (view-model.mjs `homeZoneFacts`). The Rust test (crates/aac-views/tests/home_spec.rs) builds every case and must give
// the same bytes, or decline exactly where declines.json says. web-dashboard/tests/home-view-spec.test.mjs keeps the
// table current with the JS.
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { synthetic, restyle } from './synthetic.mjs';

// Date.parse reads non-ECMAScript spellings in local time: record and build in UTC, so every machine writes the same
// table (the display zone the views format in is set per case).
process.env.TZ = 'UTC';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../../../..');
const out = path.join(here, 'cases.json');
const check = process.argv.includes('--check');

function record() {
  const dir = mkdtempSync(path.join(process.env.TMPDIR || tmpdir(), 'home-spec-'));
  const file = path.join(dir, 'calls.ndjson');
  try {
    const tests = readdirSync(path.join(root, 'web-dashboard/tests'))
      .filter((name) => name.endsWith('.test.mjs') && name !== 'home-view-spec.test.mjs')
      .sort();
    for (const name of tests) {
      const run = spawnSync(
        process.execPath,
        [
          // The tests' own clock is frozen (the golden tests' fixture), so the recorded inputs are the same every run.
          '--import',
          path.join(root, 'web-dashboard/tests/fixtures/frozen-clock.mjs'),
          '--import',
          path.join(here, 'record-hook.mjs'),
          '--test',
          '--test-reporter=dot',
          `web-dashboard/tests/${name}`,
        ],
        {
          cwd: root,
          encoding: 'utf8',
          env: { ...process.env, TZ: 'UTC', HOME_SPEC_RECORD: file, HOME_SPEC_SOURCE: name, NO_COLOR: '1' },
        }
      );
      if (run.status !== 0) throw new Error(`${name} failed while recording:\n${run.stdout}\n${run.stderr}`);
    }
    if (!existsSync(file)) return [];
    return readFileSync(file, 'utf8')
      .split('\n')
      .filter(Boolean)
      .map((line) => JSON.parse(line));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** `homeReadInput(...).presentation` for each input. The TS front door that computed it (envelope.ts over
 * src/web-server/views/home-context.ts) was deleted with src/ on 2026-11-27: the committed presentations in
 * cases.json are frozen until a native envelope recorder replaces it. */
export function presentations() {
  throw new Error(
    'The TypeScript envelope recorder was retired with src/; cases.json presentations are frozen. Record them from the native Home envelope instead.'
  );
}

export async function build(recorded) {
  const { dashboardViewModel, homeFormatProbe, homeZoneFacts } = await import('../../../../web-dashboard/public/view-model.mjs');
  const { setDisplayTimeZone } = await import('../../../../web-dashboard/public/time-format.mjs');
  const locale = new Intl.NumberFormat().resolvedOptions().locale;
  if (locale !== 'en-US') throw new Error(`The spec is recorded in en-US; this Node reads ${locale}.`);
  const seen = new Set();
  const cases = [];
  const counts = new Map();
  const names = new Set();
  for (const input of [...recorded, ...synthetic(recorded)]) {
    const key = JSON.stringify([input.dashboard, input.profiles, input.antigravity, input.ctx, input.probe, input.facts]);
    if (seen.has(key)) continue;
    seen.add(key);
    const source = input.source || 'synthetic';
    const n = (counts.get(source) || 0) + 1;
    counts.set(source, n);
    if (!setDisplayTimeZone(input.ctx.timeZone)) throw new Error(`Bad zone ${input.ctx.timeZone}`);
    let expected = null;
    try {
      expected = JSON.stringify(
        dashboardViewModel(input.dashboard, {
          now: input.ctx.now,
          profiles: input.profiles,
          platform: input.ctx.platform,
          antigravityInventory: input.antigravity,
          intervalSeconds: input.ctx.intervalSeconds,
          username: input.ctx.username,
          host: input.ctx.host,
        })
      );
    } catch {
      // The JS builder throws for this input (the page shows its error state): Rust must decline it.
    }
    if (expected !== null && input.style) expected = restyle(expected, input.style);
    const name = input.name || `${source.replace(/\.test\.mjs$/, '')}#${n}`;
    if (names.has(name)) throw new Error(`Two cases are named ${name}`);
    names.add(name);
    cases.push({
      name,
      dashboard: input.dashboard,
      profiles: input.profiles,
      antigravity: input.antigravity,
      // The page's zone facts come from this engine (Node), the envelope's words from Bun: where their time zone data
      // differ (Africa/Casablanca from 2026-09-20), Rust must decline (`zone`).
      ctx: {
        ...input.ctx,
        formatProbe: input.probe ?? homeFormatProbe(),
        zone: input.ctx.zone ?? homeZoneFacts(input.ctx.timeZone, input.ctx.now),
      },
      presentation: null,
      expected,
      ...(input.style ? { style: input.style } : {}),
      ...(input.facts ? { facts: input.facts } : {}),
    });
  }
  setDisplayTimeZone('America/New_York');
  const facts = presentations(cases.map(({ ctx, dashboard, profiles, antigravity }) => ({ ctx, dashboard, profiles, antigravity })));
  cases.forEach((c, i) => {
    // `facts: 'no-dates'` (decline/facts): the envelope without its clock and day words; Rust must refuse.
    c.presentation = c.facts === 'no-dates' ? { ...facts[i], dates: {} } : facts[i];
  });
  return cases;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const cases = await build(record());
  const text = `${JSON.stringify({ version: 1, cases }, null, 1)}\n`;
  if (check) {
    const current = existsSync(out) ? readFileSync(out, 'utf8') : '';
    if (current !== text) {
      console.error('[X] tests/spec/views/home/cases.json is not what the JS builds today; run generate.mjs.');
      process.exit(1);
    }
    console.log(`[OK] ${cases.length} Home cases match the JS`);
  } else {
    writeFileSync(out, text);
    console.log(`[OK] wrote ${cases.length} Home cases (${Math.round(text.length / 1024)} KiB)`);
  }
}
