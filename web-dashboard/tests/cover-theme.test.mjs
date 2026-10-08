import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { isDarkTheme, themeColor } from '../public/device.mjs';

const publicDir = new URL('../public/', import.meta.url);
const read = (name) => readFileSync(new URL(name, publicDir), 'utf8');

test('isDarkTheme resolves the app theme (DESIGN-MOBILE.md 6.3)', () => {
  assert.equal(isDarkTheme({ mode: 'dark', systemDark: false }), true);
  assert.equal(isDarkTheme({ mode: 'light', systemDark: true }), false);
  assert.equal(isDarkTheme({ mode: 'auto', systemDark: true }), true);
  assert.equal(isDarkTheme({ mode: 'auto', systemDark: false }), false);
  assert.equal(isDarkTheme({}), false);
  assert.equal(isDarkTheme(), false);
});

test('cover screens follow the resolved theme, including forced dark (F41)', () => {
  assert.equal(themeColor({ mode: 'light', screen: 'cover' }), '#ECF0F3');
  assert.equal(themeColor({ mode: 'dark', screen: 'cover' }), '#0C1217');
  assert.equal(themeColor({ mode: 'auto', systemDark: true, screen: 'cover' }), '#0C1217');
  // Forcing dark while the OS is light still resolves dark on cover screens.
  assert.equal(themeColor({ mode: 'dark', systemDark: false, screen: 'cover' }), '#0C1217');
  assert.equal(themeColor({ mode: 'dark', systemDark: false, screen: 'dashboard' }), '#131B23');
});

test('index.html first paint bets on the cover screen in both themes', () => {
  const html = read('index.html');
  // The static meta matches the loading screen (a cover screen); bridge.js refines it
  // to the dashboard colors once the session is known.
  assert.match(html, /<meta name="theme-color" content="#ECF0F3">/);
  // The inline first-paint script mirrors isDarkTheme (stored theme, else the OS)
  // and points the meta at the resolved cover color before the wasm boots.
  assert.match(html, /localStorage\.getItem\('aac-theme'\)/);
  assert.match(html, /querySelector\('meta\[name="theme-color"\]'\)/);
  assert.match(html, /'#0C1217':'#ECF0F3'/);
});

test('the service-worker offline page follows the OS theme', () => {
  const sw = read('sw.js');
  assert.match(
    sw,
    /<meta name="theme-color" media="\(prefers-color-scheme: light\)" content="#ECF0F3">/
  );
  assert.match(
    sw,
    /<meta name="theme-color" media="\(prefers-color-scheme: dark\)" content="#0C1217">/
  );
  assert.doesNotMatch(sw, /<meta name="theme-color" content="/);
});

test('index.html uses default status bar style to avoid standalone blur overlay', () => {
  const html = read('index.html');
  assert.match(html, /<meta name="apple-mobile-web-app-status-bar-style" content="default">/);
  assert.doesNotMatch(html, /content="black-translucent"/);
});
