// A pin's refusal is drawn on every branch of its card and sheet that draws the pin's body, so a standalone
// refusal on a fresh pin is never invisible on a wide desktop (wave-2 landing review M1).
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const src = fs
  .readFileSync(path.resolve(here, '../leptos/src/pins.rs'), 'utf8')
  .split('#[cfg(test)]')[0];

// The wide body and the live phone body share their heading/notices before any quiet, cached or fresh branch.
test('the thread sheet draws its refusal notice before every branch of its body', () => {
  const heading = src.slice(src.indexOf('fn sheet_heading('), src.indexOf('fn quiet_body('));
  assert.match(heading, /refusal_notice\(&p\.u\)/, 'the shared heading draws refusal_notice');
  assert.match(heading, /cached_notice\(&p\.u\)/, 'a cached copy says so');
  const body = src.slice(src.indexOf('fn sheet_body('), src.indexOf('fn LivePinBody('));
  assert.match(body, /\{sheet_heading\(p\)\}\{body\}/, 'wide notices precede every body branch');
  const live = src.slice(src.indexOf('fn LivePinBody('), src.indexOf('// ---- The plate'));
  assert.ok(live.indexOf('map(sheet_heading)') >= 0, 'live body uses the same notices');
  assert.ok(
    live.indexOf('map(sheet_heading)') < live.indexOf('<Show when='),
    'live notices precede every body branch'
  );
});

test('the pane and phone use the wide and live bodies with shared notices', () => {
  assert.match(src, /class="c2-pane"[\s\S]*?sheet_body\(&p,/, 'the wide pane draws the wide body');
  assert.match(
    src,
    /class="sheet-host c2-sheet-host"[\s\S]*?<LivePinBody pin=sheet_pin\/>/,
    'the phone sheet draws the live body'
  );
});
