// A pinned refusal still has an explanation even when the affected thread has never reported.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { test } from 'node:test';

const src = fs.readFileSync(new URL('../leptos/src/pins.rs', import.meta.url), 'utf8');

test('the shared pin heading points to report problems and identifies cached copies', () => {
  const heading = src.slice(src.indexOf('fn sheet_heading('), src.indexOf('fn quiet_body('));
  assert.match(heading, /p\.u\.refusal\.is_some\(\).*See Report problems/s);
  assert.match(heading, /cached_notice\(&p\.u\)/);
  const body = src.slice(src.indexOf('fn LivePinBody('), src.indexOf('// ---- The plate'));
  assert.match(body, /sheet_heading\(p, live\.elapsed_seconds\(\)\)[\s\S]*?<Show when=/);
});

test('the pane and overlay share the live pin body', () => {
  assert.match(src, /class="c2-pane"[\s\S]*?<LivePinBody pin=pin\/>/);
  assert.match(src, /class="sheet-host c2-sheet-host"[\s\S]*?<LivePinBody pin=sheet_pin\/>/);
});
