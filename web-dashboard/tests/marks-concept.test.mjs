import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// Locks for the provider marks (dark-icons round 2, 2026-10-03), now drawn by the Leptos app: the server's provider
// ids, the published optical scales and light-variant table, and the single-registry rule that all marks load through
// leptos/src/ui/icons.rs (never recolour or redraw official artwork; leptos/src/ui/marks/sources.json is the
// provenance record, moved there from the retired Slint UI).

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.join(here, '..', '..');
const icons = fs.readFileSync(path.join(here, '..', 'leptos', 'src', 'ui', 'icons.rs'), 'utf8');
const marksDir = path.join(here, '..', 'leptos', 'src', 'ui', 'marks');
const sources = JSON.parse(fs.readFileSync(path.join(marksDir, 'sources.json'), 'utf8'));
const serverTable = fs.readFileSync(
  path.join(repo, 'src', 'web-server', 'services', 'dashboard-provider-table.ts'), 'utf8');

const between = (text, from, to) => text.slice(text.indexOf(from), text.indexOf(to, text.indexOf(from)));
const markFn = between(icons, 'pub fn ProviderMark(', 'let box_style');
const KNOWN = new Set([...markFn.matchAll(/"([a-z0-9-]+)"/g)].map((m) => m[1]));
const lightFn = between(icons, 'fn has_light_art(', '\n}');
const LIGHT_IDS = new Set([...lightFn.matchAll(/"([a-z0-9-]+)"/g)].map((m) => m[1]));
const scaleFn = between(icons, 'pub fn mark_scale(', '_ => 1.0');
const SCALES = Object.fromEntries([...scaleFn.matchAll(/"([a-z0-9-]+)" => ([^,]+),/g)].map((m) => [m[1], m[2].trim()]));
const tableStart = serverTable.indexOf('DASHBOARD_PROVIDER_IDS');
const tableBlock = serverTable.slice(tableStart, serverTable.indexOf(']);', tableStart));
const SERVER_IDS = [...tableBlock.matchAll(/'([a-z0-9-]+)'/g)].map((m) => m[1]);

test('every provider id the server can send has a mark', () => {
  assert.ok(SERVER_IDS.length >= 9, `expected the 9 server ids, saw ${SERVER_IDS.length}`);
  for (const id of SERVER_IDS) assert.ok(KNOWN.has(id), `server id "${id}" is unknown: its mark never renders`);
});

test('the optical scales match the published sources.json scales', () => {
  for (const [id, entry] of Object.entries(sources.providers)) {
    assert.ok(id in SCALES, `no scale for "${id}"`);
    if (id === 'kimi-code') {
      // the owner's pick: the official app icon on its own plate, drawn 14% smaller than the measured scale so it
      // carries the same visual mass as the open glyphs.
      assert.equal(SCALES[id], '0.97 * 0.86', `Kimi plate correction changed: ${SCALES[id]}`);
      continue;
    }
    assert.equal(Number(SCALES[id]), entry.scale, `"${id}" scale ${SCALES[id]} != published ${entry.scale}`);
  }
});

test('the published light-variant table matches the artwork on disk and the light selector', () => {
  for (const [id, entry] of Object.entries(sources.providers)) {
    const published = entry.out?.light != null;
    const onDisk = fs.existsSync(path.join(marksDir, `${id}-light.svg`));
    if (id === 'kimi-code') {
      // The bare-K SVGs are retired per the decision note; the plated PNG is the only artwork.
      assert.match(String(entry.decision), /retired/, 'Kimi decision note lost');
      assert.ok(fs.existsSync(path.join(marksDir, 'kimi-code.png.b64')), 'Kimi app icon missing');
      assert.ok(!onDisk && !fs.existsSync(path.join(marksDir, 'kimi-code.svg')), 'retired Kimi SVG back on disk');
      assert.ok(!LIGHT_IDS.has(id), 'Kimi must not select a light variant');
      continue;
    }
    assert.equal(onDisk, published, `"${id}": published light=${published} but ${id}-light.svg ${onDisk ? 'exists' : 'is missing'}`);
    assert.equal(LIGHT_IDS.has(id), published, `"${id}": has_light_art says ${LIGHT_IDS.has(id)} but published light=${published}`);
  }
});

test('all provider artwork loads through the marks registry alone', () => {
  const offenders = [];
  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) { walk(full); continue; }
      if (!entry.name.endsWith('.rs') || full.endsWith(path.join('ui', 'icons.rs'))) continue;
      if (/include_str!\("[^"]*marks\//.test(fs.readFileSync(full, 'utf8'))) offenders.push(path.relative(repo, full));
    }
  };
  walk(path.join(here, '..', 'leptos', 'src'));
  assert.deepEqual(offenders, [], `provider artwork embedded outside icons.rs: ${offenders.join(', ')}`);
});
