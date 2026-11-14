import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import buildUi from '../../scripts/build-ui.js';
import routeModule from '../public/sw-route.js';

const { shellFingerprint, stagedAppFiles, stampedSwSource } = buildUi;

const PUBLIC = path.join(import.meta.dirname ?? '', '..', 'public');
const LEPTOS = path.join(import.meta.dirname ?? '', '..', 'leptos');
const WASM_ID = '586cb2ce4411'; // the wasm id stamped into sw.js before the shell stamp

function shellDir(files = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-shell-'));
  const seed = {
    'sw.js': "var BUILD_ID = '__AAC_BUILD_ID__';",
    'sw-route.js': '// routing',
    'view-model.mjs': 'export const a = 1;',
  };
  for (const [name, content] of Object.entries({ ...seed, ...files })) {
    fs.writeFileSync(path.join(dir, name), content);
  }
  return dir;
}

/** The staged frame page names its runtime by the wasm id (build-ui.js stagedAppFiles). */
const appFiles = (wasmId, css = 'body{}') =>
  new Map([
    ['app/index.html', Buffer.from(`<link rel="modulepreload" href="/pkg/${wasmId}/aac_web.js">`)],
    ['app/app.css', Buffer.from(css)],
  ]);

function stamp(dir, wasmId = WASM_ID, css = undefined) {
  const app = appFiles(wasmId, css);
  const shell = shellFingerprint(dir, app);
  const sw = stampedSwSource(
    fs.readFileSync(path.join(dir, 'sw.js'), 'utf8'),
    shell.sha256.slice(0, 12)
  );
  return { app, shell, sw };
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

test('the stamp is taken from the real packaged shell and frame page and is deterministic', () => {
  const a = shellFingerprint(PUBLIC, stagedAppFiles(LEPTOS, WASM_ID));
  const b = shellFingerprint(PUBLIC, stagedAppFiles(LEPTOS, WASM_ID));
  assert.equal(a.sha256, b.sha256);
  assert.ok(a.files >= 40, `expected the whole shell set, hashed ${a.files} files`);
  assert.notEqual(a.sha256.slice(0, 12), WASM_ID);
  // the staged page starts that build's runtime and its stylesheet by content
  const page = stagedAppFiles(LEPTOS, WASM_ID).get('app/index.html').toString('utf8');
  assert.match(page, new RegExp(`/pkg/${WASM_ID}/aac_web\\.js`));
  assert.match(page, /\/app\/app\.css\?v=[a-f0-9]{12}/);
  assert.doesNotMatch(page, /\{\{(PKG|CSS)\}\}/);
});

for (const [name, change] of [
  ['sw.js source', { 'sw.js': "var BUILD_ID = '__AAC_BUILD_ID__'; // changed" }],
  ['sw-route.js', { 'sw-route.js': '// routing v2' }],
  ['a .mjs module', { 'view-model.mjs': 'export const a = 2;' }],
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

test('a stylesheet change alone rolls the service worker', () => {
  const dir = shellDir();
  try {
    const before = stamp(dir, WASM_ID, 'body{}');
    const after = stamp(dir, WASM_ID, 'body{color:red}');
    assert.notEqual(after.shell.sha256, before.shell.sha256);
    assert.notEqual(after.sw, before.sw);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('a wasm-only change keeps pkg/ content-addressed but still rolls the worker via the frame page', () => {
  const dir = shellDir();
  try {
    const before = stamp(dir, 'aaaaaaaaaaaa');
    const after = stamp(dir, 'bbbbbbbbbbbb');
    // the staged frame page embeds the new pkg/ path, so the shell stamp rolls; the
    // pkg/ folder name itself stays the wasm hash, stamped elsewhere in the build.
    assert.notEqual(after.shell.sha256, before.shell.sha256);
    assert.match(after.app.get('app/index.html').toString('utf8'), /pkg\/bbbbbbbbbbbb\//);
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
