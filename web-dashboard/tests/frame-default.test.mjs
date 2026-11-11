// The classic page's first script sends this device to the app frame (/app/<tab>) when it chose the frame, or when it
// chose nothing and FRAME_DEFAULT is 'new' (the flip, once Home, Accounts and Analytics are native). Runs the script
// itself against a stubbed location and storage.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const html = fs.readFileSync(new URL('../public/index.html', import.meta.url), 'utf8');
const script = html.match(/<script>(var FRAME_DEFAULT='(?:classic|new)';try\{if\(\(localStorage[\s\S]*?)<\/script>/)?.[1];

function run(source, { stored = null, path = '/', search = '' } = {}) {
  let to = null;
  const location = { pathname: path, search, replace: (url) => { to = url; } };
  vm.runInNewContext(source, { localStorage: { getItem: (k) => (k === 'aac-frame' ? stored : null) }, location, URLSearchParams });
  return to;
}

test('the classic page has one FRAME_DEFAULT, set to classic until the native pages land', () => {
  assert.ok(script, 'the redirect script with FRAME_DEFAULT');
  assert.equal((html.match(/FRAME_DEFAULT='/g) || []).length, 1);
  assert.match(script, /^var FRAME_DEFAULT='classic';/);
});

test('today: only a device that chose the frame goes to it', () => {
  assert.equal(run(script), null);
  assert.equal(run(script, { stored: 'new' }), '/app/home');
  assert.equal(run(script, { stored: 'new', path: '/analytics' }), '/app/analytics');
  assert.equal(run(script, { stored: 'classic' }), null);
});

test('after the flip: a fresh install opens the frame, a device that chose classic stays, ?classic=1 and ?content=1 stay', () => {
  const flipped = script.replace("var FRAME_DEFAULT='classic';", "var FRAME_DEFAULT='new';");
  assert.equal(run(flipped), '/app/home');
  assert.equal(run(flipped, { path: '/accounts' }), '/app/accounts');
  assert.equal(run(flipped, { search: '?view=scoreboard' }), '/app/scoreboard');
  assert.equal(run(flipped, { stored: 'classic' }), null);
  assert.equal(run(flipped, { search: '?classic=1' }), null);
  assert.equal(run(flipped, { search: '?content=1' }), null);
});
