import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import buildUi from '../../scripts/build-ui.js';
import routeModule from '../public/sw-route.js';

const { shellFingerprint, versionedBridgeSource, stampedSwSource } = buildUi;

const PUBLIC = path.join(import.meta.dirname ?? '', '..', 'public');
const WASM_ID = '586cb2ce4411'; // the wasm id stamped into sw.js before this fix

function shellDir(files = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-shell-'));
  const seed = {
    'bridge.js': `import { init } from './pkg/ccs_account_dashboard.js';`,
    'index.html': '<!doctype html><html></html>',
    'sw.js': "var BUILD_ID = '__AAC_BUILD_ID__';",
    'sw-route.js': '// routing',
    'view-model.mjs': 'export const a = 1;',
  };
  for (const [name, content] of Object.entries({ ...seed, ...files })) {
    fs.writeFileSync(path.join(dir, name), content);
  }
  return dir;
}

function stamp(dir, wasmId = WASM_ID) {
  const bridge = versionedBridgeSource(
    fs.readFileSync(path.join(dir, 'bridge.js'), 'utf8'),
    wasmId
  );
  const shell = shellFingerprint(dir, bridge);
  const sw = stampedSwSource(
    fs.readFileSync(path.join(dir, 'sw.js'), 'utf8'),
    shell.sha256.slice(0, 12)
  );
  return { bridge, shell, sw };
}

test('identical shell inputs give an identical stamp', () => {
  const dir = shellDir();
  try {
    const a = stamp(dir);
    const b = stamp(dir);
    assert.equal(a.shell.sha256, b.shell.sha256);
    assert.equal(a.sw, b.sw);
    assert.match(a.shell.sha256.slice(0, 12), /^[a-f0-9]{12}$/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('the stamp is taken from the real packaged shell and is deterministic', () => {
  const bridge = versionedBridgeSource(
    fs.readFileSync(path.join(PUBLIC, 'bridge.js'), 'utf8'),
    WASM_ID
  );
  const a = shellFingerprint(PUBLIC, bridge);
  const b = shellFingerprint(PUBLIC, bridge);
  assert.equal(a.sha256, b.sha256);
  assert.ok(a.files >= 40, `expected the whole shell set, hashed ${a.files} files`);
  assert.notEqual(a.sha256.slice(0, 12), WASM_ID);
});

for (const [name, change] of [
  ['index.html', { 'index.html': '<!doctype html><html data-x="1"></html>' }],
  ['sw.js source', { 'sw.js': "var BUILD_ID = '__AAC_BUILD_ID__'; // changed" }],
  ['sw-route.js', { 'sw-route.js': '// routing v2' }],
  ['a .mjs module', { 'view-model.mjs': 'export const a = 2;' }],
  [
    'bridge.js source',
    { 'bridge.js': `import { init } from './pkg/ccs_account_dashboard.js'; // v2` },
  ],
]) {
  test(`changing ${name} alone rolls the service worker`, () => {
    const dir = shellDir();
    try {
      const before = stamp(dir);
      const [file, content] = Object.entries(change)[0];
      fs.writeFileSync(path.join(dir, file), content);
      const after = stamp(dir);
      assert.notEqual(after.shell.sha256, before.shell.sha256);
      assert.notEqual(after.sw, before.sw);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
}

test('a new .mjs file alone rolls the service worker', () => {
  const dir = shellDir();
  try {
    const before = stamp(dir);
    fs.writeFileSync(path.join(dir, 'extra.mjs'), 'export const extra = true;');
    const after = stamp(dir);
    assert.notEqual(after.shell.sha256, before.shell.sha256);
    assert.notEqual(after.sw, before.sw);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('a wasm-only change keeps pkg/ content-addressed but still rolls the worker via bridge.js', () => {
  const dir = shellDir();
  try {
    const before = stamp(dir, 'aaaaaaaaaaaa');
    const after = stamp(dir, 'bbbbbbbbbbbb');
    // bridge post-rewrite embeds the new pkg/ path, so the shell stamp rolls; the
    // pkg/ folder name itself stays the wasm hash, stamped elsewhere in the build.
    assert.notEqual(after.shell.sha256, before.shell.sha256);
    assert.match(after.bridge, /pkg\/bbbbbbbbbbbb\//);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('the new sw.js differs from the wasm-stamped bytes installed clients already run', () => {
  const dir = shellDir();
  try {
    const { shell, sw } = stamp(dir);
    const shellId = shell.sha256.slice(0, 12);
    assert.notEqual(shellId, WASM_ID);
    const oldSw = stampedSwSource(fs.readFileSync(path.join(dir, 'sw.js'), 'utf8'), WASM_ID);
    assert.notEqual(sw, oldSw, 'old worker clients must see a different sw.js');
    // The cache name rolls with the shell id, so activation drops the old cache.
    assert.equal(routeModule.cacheName(shellId), `aac-shell-${shellId}`);
    assert.notEqual(routeModule.cacheName(shellId), routeModule.cacheName(WASM_ID));
    // The worker carries exactly the shell id, no placeholder left.
    assert.equal(sw.split(`'${shellId}'`).length - 1, 1);
    assert.ok(!sw.includes('__AAC_BUILD_ID__'));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
