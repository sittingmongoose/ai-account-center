import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const publicDir = new URL('../public/', import.meta.url);
const read = (url) => readFileSync(url, 'utf8');
const sw = read(new URL('sw.js', publicDir));
const offline = vm.runInNewContext(sw.match(/var OFFLINE_PAGE =([\s\S]*?);\r?\n/)[1]);
// The Home Screen start (/) and every tab answer with the frame page since the classic page was retired.
const documents = [
  ['Home Screen start and frame page', read(new URL('../leptos/index.html', publicDir))],
  ['service-worker offline start', offline],
];
function meta(html, name) {
  for (const tag of html.match(/<meta\b[^>]*>/gi) || []) {
    const a = Object.fromEntries(
      [...tag.matchAll(/([\w-]+)=["']([^"']*)["']/g)].map((m) => [m[1], m[2]])
    );
    if (a.name === name) return a.content;
  }
}
for (const [name, html] of documents) {
  test(`${name} installs an edge-to-edge standalone viewport`, () => {
    assert.equal(meta(html, 'apple-mobile-web-app-capable'), 'yes');
    assert.equal(meta(html, 'apple-mobile-web-app-status-bar-style'), 'black-translucent');
    assert.match(meta(html, 'viewport'), /viewport-fit=cover/);
  });
}
test('the entry page loads the system-status protection before the app', () => {
  const [, html] = documents[0];
  assert.match(html, /<link[^>]+href="\/status-bar\.css"/);
  assert.match(html, /data-status-embedded/);
});
