// The classic page's first script sends this device to the app frame (/app/<tab>) when it chose the frame, or when it
// chose nothing and FRAME_DEFAULT is 'new' (flipped once Home, Accounts and Analytics were native). Runs the script
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

test('the classic page has one FRAME_DEFAULT, and it is the frame', () => {
  assert.ok(script, 'the redirect script with FRAME_DEFAULT');
  assert.equal((html.match(/FRAME_DEFAULT='/g) || []).length, 1);
  assert.match(script, /^var FRAME_DEFAULT='new';/);
});

test('a fresh install opens the frame; a device that chose classic stays; ?classic=1, ?content=1 and ?e2e stay', () => {
  assert.equal(run(script), '/app/home');
  assert.equal(run(script, { path: '/accounts' }), '/app/accounts');
  assert.equal(run(script, { search: '?view=scoreboard' }), '/app/scoreboard');
  assert.equal(run(script, { stored: 'new', path: '/analytics' }), '/app/analytics');
  assert.equal(run(script, { stored: 'classic' }), null);
  assert.equal(run(script, { search: '?classic=1' }), null);
  assert.equal(run(script, { search: '?content=1' }), null);
  assert.equal(run(script, { search: '?e2e' }), null);
  assert.equal(run(script, { search: '?e2e&view=accounts' }), null);
});

test('with the constant set back to classic only a device that chose the frame goes to it (the way back)', () => {
  const back = script.replace("var FRAME_DEFAULT='new';", "var FRAME_DEFAULT='classic';");
  assert.equal(run(back), null);
  assert.equal(run(back, { stored: 'new' }), '/app/home');
  assert.equal(run(back, { stored: 'classic' }), null);
});
