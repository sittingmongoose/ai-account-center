import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { PROVIDER_REGISTRY } from '../public/view-model.mjs';
import { QUOTA_PROVIDERS } from '../public/analytics-quota.mjs';

// Regression test for the dark-mode provider marks (2026-10-02): on a fresh dark Analytics load the
// quota-history and reset-agenda marks for every provider except Claude and Codex stayed blank. The
// artwork and the variant selection were correct; the images are decoded asynchronously by the browser
// and the marks first instantiated in those late-created rows completed their loads after the event
// loop went idle, so their frames were lost until the next repaint (hover, scroll, theme switch).
// The fix preloads every mark image at boot and keeps the loop alive until the loads settle. These
// tests lock the variant coverage and both halves of the mechanism.

const here = path.dirname(fileURLToPath(import.meta.url));
const markSlint = fs.readFileSync(path.join(here, '..', 'ui', 'components', 'mark.slint'), 'utf8');
const dashboardSlint = fs.readFileSync(path.join(here, '..', 'ui', 'dashboard.slint'), 'utf8');
const marksDir = path.join(here, '..', 'ui', 'marks');

// Every image asset the Slint Marks registry embeds: name -> file path in ui/marks/.
const ASSETS = Object.fromEntries(
  [...markSlint.matchAll(/out property <image> ([a-z0-9-]+): @image-url\("([^"]+)"\);/g)]
    .map(m => [m[1], m[2].replace('../marks/', '')]),
);
// Provider ids the registry answers for (the `if Marks.known(...)` gate in ProviderMark).
const knownFn = markSlint.slice(markSlint.indexOf('function known('), markSlint.indexOf('function scale('));
const KNOWN = new Set([...knownFn.matchAll(/id == "([^"]+)"/g)].map(m => m[1]));
const lightFn = markSlint.slice(markSlint.indexOf('function has-light-art('), markSlint.indexOf('function known('));
const LIGHT_IDS = new Set([...lightFn.matchAll(/id == "([^"]+)"/g)].map(m => m[1]));
// Images the boot preloader instantiates (empty before the fix: no MarksPreloader component).
const preloaderBlock = markSlint.includes('component MarksPreloader')
  ? markSlint.slice(markSlint.indexOf('component MarksPreloader'))
  : '';
const PRELOADED = new Set([...preloaderBlock.matchAll(/Marks\.([a-z0-9-]+)/g)].map(m => m[1]));

const viewModelIds = new Set([
  ...PROVIDER_REGISTRY.map(p => p.id),
  ...QUOTA_PROVIDERS.map(([id]) => id),
]);

test('every provider id the view models can name is known to the Slint Marks registry', () => {
  assert.ok(viewModelIds.size >= 9, `expected the 9 providers, saw ${viewModelIds.size}`);
  for (const id of viewModelIds) assert.ok(KNOWN.has(id), `Marks.known("${id}") is false: its mark never renders`);
});

test('every known provider has its dark artwork on disk, and its light artwork exactly when published', () => {
  assert.ok(KNOWN.size >= 9);
  for (const id of KNOWN) {
    const dark = [`${id}.svg`, `${id}.png`].find(f => fs.existsSync(path.join(marksDir, f)));
    assert.ok(dark, `no dark-surface artwork for "${id}" in ui/marks/`);
    const lightFile = path.join(marksDir, `${id}-light.svg`);
    assert.equal(
      fs.existsSync(lightFile), LIGHT_IDS.has(id),
      `"${id}": has-light-art says ${LIGHT_IDS.has(id)} but ${id}-light.svg ${fs.existsSync(lightFile) ? 'exists' : 'is missing'}`,
    );
  }
});

test('each published light variant is different artwork from its dark variant', () => {
  assert.ok(LIGHT_IDS.size > 0);
  for (const id of LIGHT_IDS) {
    const dark = [`${id}.svg`, `${id}.png`].map(f => path.join(marksDir, f)).find(f => fs.existsSync(f));
    const light = fs.readFileSync(path.join(marksDir, `${id}-light.svg`));
    assert.ok(!fs.readFileSync(dark).equals(light), `"${id}" light and dark artwork are byte-identical`);
  }
});

test('the boot preloader instantiates every mark and platform image, with no stale references', () => {
  const assets = Object.keys(ASSETS);
  assert.ok(assets.length >= 17, `expected 14 marks + 3 platform glyphs, saw ${assets.length}`);
  for (const name of assets) assert.ok(PRELOADED.has(name), `Marks.${name} is not preloaded at boot`);
  for (const name of PRELOADED) assert.ok(name in ASSETS, `preloader references unknown Marks.${name}`);
});

test('the preloader is instantiated in the Dashboard shell', () => {
  assert.match(dashboardSlint, /import \{[^}]*MarksPreloader[^}]*\} from "components\/mark\.slint";/);
  assert.match(dashboardSlint, /MarksPreloader\s*\{/);
});

test('a settle keep-alive covers late image loads for several seconds after boot', () => {
  const timer = /settle-timer := Timer \{[^}]*interval: (\d+)ms;[^}]*running: root\.settle-ticks < (\d+);[^}]*triggered => \{ root\.settle-ticks \+= 1; \}/s
    .exec(dashboardSlint);
  assert.ok(timer, 'no settle-timer keeping the loop alive while late images load');
  const [, intervalMs, ticks] = timer.map(Number);
  assert.ok(ticks * intervalMs >= 3000, `settle window is only ${ticks * intervalMs}ms`);
});
