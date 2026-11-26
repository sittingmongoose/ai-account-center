// D7: the Home case table (tests/spec/views/home/cases.json) is what view-model.mjs builds today. Every stored case
// is built again here from its stored inputs, in its display zone, and must give the stored bytes; it carries the M3
// envelope facts the TS front door computes (src/web-server/views/home-context.ts). The Rust builder runs the same table
// (crates/aac-views/tests/home_spec.rs), so a JS change that is not regenerated fails here before Rust can drift.
// Regenerate with `node tests/spec/views/home/generate.mjs`; `--check` also re-records the JS tests' inputs.
// generate.mjs records in UTC: a non-ECMAScript date (the decline/date case) is read in local time.
process.env.TZ = 'UTC';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dashboardViewModel, homeFormatProbe } from '../public/view-model.mjs';
import { setDisplayTimeZone, getDisplayTimeZone } from '../public/time-format.mjs';
import { restyle } from '../../tests/spec/views/home/synthetic.mjs';

const read = (name) => JSON.parse(readFileSync(new URL(`../../tests/spec/views/home/${name}`, import.meta.url), 'utf8'));
const { version, cases } = read('cases.json');
const declines = read('declines.json');

test('the Home table is version 1, with recorded, synthetic and decline cases', () => {
  assert.equal(version, 1);
  assert.ok(cases.length >= 150, `${cases.length} cases`);
  const names = new Set(cases.map((c) => c.name));
  assert.equal(names.size, cases.length, 'names are unique');
  for (const name of Object.keys(declines)) assert.ok(names.has(name), `declines.json names ${name}`);
  for (const prefix of ['view-model#', 'frame-dashboard#', 'zone/', 'edge/', 'decline/', 'probe/narrow', 'probe/safari'])
    assert.ok(cases.some((c) => c.name.startsWith(prefix)), prefix);
  // the recording runs in en-US, as this Node writes it
  assert.deepEqual(cases.find((c) => c.name.startsWith('view-model#')).ctx.formatProbe, homeFormatProbe());
});

test('every stored Home case is what view-model.mjs builds today, byte for byte', () => {
  const zone = getDisplayTimeZone();
  try {
    for (const c of cases) {
      assert.ok(setDisplayTimeZone(c.ctx.timeZone), `${c.name}: zone ${c.ctx.timeZone}`);
      let built = null;
      try {
        built = JSON.stringify(
          dashboardViewModel(c.dashboard, {
            now: c.ctx.now,
            profiles: c.profiles,
            platform: c.ctx.platform,
            antigravityInventory: c.antigravity,
            intervalSeconds: c.ctx.intervalSeconds,
            username: c.ctx.username,
            host: c.ctx.host,
          })
        );
      } catch {
        /* stored as null: the JS throws for it */
      }
      if (built !== null && c.style) built = restyle(built, c.style);
      assert.equal(built, c.expected, `${c.name}: cases.json is stale; run node tests/spec/views/home/generate.mjs`);
      if (c.expected === null) assert.ok(declines[c.name], `${c.name}: the JS throws, so Rust must decline it`);
    }
  } finally {
    setDisplayTimeZone(zone);
  }
});

test('every case carries the M3 envelope facts; only decline/facts lacks the clock and day words', () => {
  // The facts were recorded from the retired TS front door (homeReadInput) and are frozen since src/ was deleted.
  for (const c of cases) {
    const p = c.presentation;
    assert.ok(p && typeof p === 'object', c.name);
    assert.deepEqual(Object.keys(p).sort(), ['dates', 'epochs', 'order'], c.name);
    assert.equal(Object.keys(p.dates).length === 0, c.name === 'decline/facts', c.name);
    assert.ok(p.dates[String(c.ctx.now)] || c.name === 'decline/facts', `${c.name}: the page's clock has its words`);
  }
});
