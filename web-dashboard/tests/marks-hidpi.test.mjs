import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { premultiplySvgTextureUploads } from '../public/renderer.mjs';

// Regression test for the dark-mode provider marks (MARKS-HIDPI, 2026-10-03): on Jared's machines every
// SVG mark in dark mode drew with hard, stair-stepped edges, a fattened silhouette and stray saturated
// pixels (red on Claude's tips), while light mode looked right. Slint 1.18.1 flags SVG <img> textures as
// premultiplied, but WebGL uploads them with straight alpha (measured: a 50% #D97757 pixel reads back as
// [217,120,88,128] on the M3's ANGLE Metal and on SwiftShader), so femtovg drew each edge pixel at full
// colour. Light paper hid it (the over-bright edge clips to white); dark paper showed it. The fix uploads
// exactly the SVG images premultiplied (public/renderer.mjs); these tests pin the mechanism.

const here = path.dirname(fileURLToPath(import.meta.url));
const read = (...parts) => fs.readFileSync(path.join(here, '..', ...parts), 'utf8');
const UNPACK_PREMULTIPLY_ALPHA_WEBGL = 0x9241;

/** A browser-shaped scope: URL, HTMLImageElement and two WebGL contexts that log every upload. */
function fakeBrowser() {
  let next = 0;
  const URL = { createObjectURL: blob => `blob:http://dash/${++next}-${blob.type}`, revokeObjectURL: () => {} };
  class HTMLImageElement { constructor(src) { this.src = src; } }
  const uploads = [];
  const context = name => class {
    constructor() { this.UNPACK_PREMULTIPLY_ALPHA_WEBGL = UNPACK_PREMULTIPLY_ALPHA_WEBGL; this.premultiply = false; this.stores = []; }
    getParameter(flag) { assert.equal(flag, UNPACK_PREMULTIPLY_ALPHA_WEBGL); return this.premultiply; }
    pixelStorei(flag, value) { assert.equal(flag, UNPACK_PREMULTIPLY_ALPHA_WEBGL); this.premultiply = Boolean(value); this.stores.push(Boolean(value)); }
    texImage2D(...args) { uploads.push({ context: name, call: 'texImage2D', premultiplied: this.premultiply, source: args.at(-1) }); return 'image'; }
    texSubImage2D(...args) { uploads.push({ context: name, call: 'texSubImage2D', premultiplied: this.premultiply, source: args.at(-1) }); return 'sub'; }
  };
  return { URL, HTMLImageElement, WebGL2RenderingContext: context('webgl2'), WebGLRenderingContext: context('webgl'), uploads };
}

test('SVG images upload premultiplied; PNG images, pixel buffers and the context default are unchanged', () => {
  const scope = fakeBrowser();
  assert.equal(premultiplySvgTextureUploads(scope), true);
  const svg = new scope.HTMLImageElement(scope.URL.createObjectURL({ type: 'image/svg+xml' }));
  const png = new scope.HTMLImageElement(scope.URL.createObjectURL({ type: 'image/png' }));
  const gl = new scope.WebGL2RenderingContext();
  // femtovg 0.27's HtmlImageElement upload: texSubImage2D(target, level, x, y, format, type, image)
  assert.equal(gl.texSubImage2D(0x0de1, 0, 0, 0, 0x1908, 0x1401, svg), 'sub');
  gl.texSubImage2D(0x0de1, 0, 0, 0, 0x1908, 0x1401, png);
  gl.texImage2D(0x0de1, 0, 0x1908, 8, 8, 0, 0x1908, 0x1401, new Uint8Array(256));
  gl.texSubImage2D(0x0de1, 0, 0, 0, 8, 8, 0x1908, 0x1401, new Uint8Array(256), 0);
  assert.deepEqual(scope.uploads.map(u => u.premultiplied), [true, false, false, false]);
  assert.equal(gl.premultiply, false, 'the unpack flag is restored after the SVG upload');
  assert.deepEqual(gl.stores, [true, false]);
});

test('both WebGL context kinds and both upload calls are covered', () => {
  const scope = fakeBrowser();
  premultiplySvgTextureUploads(scope);
  const svg = new scope.HTMLImageElement(scope.URL.createObjectURL({ type: 'image/svg+xml' }));
  for (const Context of [scope.WebGL2RenderingContext, scope.WebGLRenderingContext]) {
    const gl = new Context();
    gl.texImage2D(0x0de1, 0, 0x1908, 0x1908, 0x1401, svg);
    gl.texSubImage2D(0x0de1, 0, 0, 0, 0x1908, 0x1401, svg);
  }
  assert.deepEqual(scope.uploads.map(u => `${u.context}.${u.call}:${u.premultiplied}`), [
    'webgl2.texImage2D:true', 'webgl2.texSubImage2D:true', 'webgl.texImage2D:true', 'webgl.texSubImage2D:true',
  ]);
});

test('a context that already premultiplies keeps doing so, and a failed upload still restores the flag', () => {
  const scope = fakeBrowser();
  premultiplySvgTextureUploads(scope);
  const svg = new scope.HTMLImageElement(scope.URL.createObjectURL({ type: 'image/svg+xml' }));
  const gl = new scope.WebGL2RenderingContext();
  gl.premultiply = true;
  gl.texSubImage2D(0x0de1, 0, 0, 0, 0x1908, 0x1401, svg);
  assert.equal(scope.uploads.at(-1).premultiplied, true);
  assert.equal(gl.premultiply, true);

  const lost = new Error('context lost');
  const failing = fakeBrowser();
  failing.WebGL2RenderingContext.prototype.texSubImage2D = () => { throw lost; };
  premultiplySvgTextureUploads(failing);
  const image = new failing.HTMLImageElement(failing.URL.createObjectURL({ type: 'image/svg+xml' }));
  const gl2 = new failing.WebGL2RenderingContext();
  assert.throws(() => gl2.texSubImage2D(0x0de1, 0, 0, 0, 0x1908, 0x1401, image), lost);
  assert.equal(gl2.premultiply, false);
});

test('installing twice wraps once; a revoked SVG url is forgotten; no WebGL means no change', () => {
  const scope = fakeBrowser();
  assert.equal(premultiplySvgTextureUploads(scope), true);
  const upload = scope.WebGL2RenderingContext.prototype.texSubImage2D;
  const create = scope.URL.createObjectURL;
  assert.equal(premultiplySvgTextureUploads(scope), true);
  assert.equal(scope.WebGL2RenderingContext.prototype.texSubImage2D, upload);
  assert.equal(scope.URL.createObjectURL, create);
  const url = scope.URL.createObjectURL({ type: 'image/svg+xml' });
  scope.URL.revokeObjectURL(url);
  const gl = new scope.WebGL2RenderingContext();
  gl.texSubImage2D(0x0de1, 0, 0, 0, 0x1908, 0x1401, new scope.HTMLImageElement(url));
  assert.equal(scope.uploads.at(-1).premultiplied, false);
  assert.equal(premultiplySvgTextureUploads({ URL: scope.URL, HTMLImageElement: scope.HTMLImageElement }), false);
  assert.equal(premultiplySvgTextureUploads({}), false);
});

test('the dashboard installs the fix before Slint creates its first image', () => {
  const bridge = read('public', 'bridge.js');
  const boot = bridge.slice(bridge.indexOf('// ---------------------------------------------------------------- boot'));
  const install = boot.indexOf('premultiplySvgTextureUploads();');
  assert.ok(install > 0, 'bridge.js boot calls premultiplySvgTextureUploads()');
  assert.ok(install < boot.indexOf('await init();'), 'before the wasm module initialises');
  assert.ok(install < boot.indexOf('start_dashboard('), 'before the dashboard instantiates its images');
});

test('provider marks take the plain vector path: no pixelated, cached-raster or tinted draw, one opacity per variant', () => {
  const mark = read('ui', 'components', 'mark.slint');
  const block = mark.slice(mark.indexOf('export component ProviderMark'), mark.indexOf('export component PlatformGlyph'));
  assert.ok(block.length > 100);
  for (const banned of ['image-rendering', 'pixelated', 'cache-rendering-hint', 'colorize', 'Layer {']) {
    assert.equal(block.includes(banned), false, `ProviderMark must not use ${banned}`);
  }
  const images = [...block.matchAll(/Image \{([\s\S]*?)\n {4}\}/g)].map(m => m[1]);
  assert.equal(images.length, 2, 'one image for the dark-surface art and one for the light-surface art');
  const opacity = images.map(body => /opacity: ([^;]+);/.exec(body)?.[1].trim());
  assert.deepEqual(opacity, ['root.twin ? Theme.mix : 1', '1 - Theme.mix'], 'the two variants cross-fade with complementary opacities');
  assert.match(images[0], /source: Marks\.dark-art\(root\.provider\);/);
  assert.match(images[1], /source: Marks\.light-art\(root\.provider\);/);
  for (const body of images) assert.match(body, /image-fit: contain;/);
});

test('every provider mark but the plated Kimi icon is SVG, and Slint stays on the version the fix was measured on', () => {
  const mark = read('ui', 'components', 'mark.slint');
  const files = [...mark.matchAll(/out property <image> ([a-z0-9-]+): @image-url\("\.\.\/marks\/([^"]+)"\);/g)].map(m => m[2]);
  assert.ok(files.length >= 17);
  assert.deepEqual(files.filter(f => !f.endsWith('.svg')), ['kimi-code.png'], 'raster marks upload straight and are premultiplied by the shader');
  // The fix matches Slint 1.18.1's premultiplied flag for SVG <img> textures. When Slint changes, re-measure
  // (see renderer.mjs) before keeping premultiplySvgTextureUploads, or SVG marks would be premultiplied twice.
  assert.match(read('Cargo.toml'), /slint = \{ version = "=1\.18\.1"/);
});
