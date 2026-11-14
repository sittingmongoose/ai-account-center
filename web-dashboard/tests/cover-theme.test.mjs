import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { isDarkTheme, themeColor } from '../public/device.mjs';

const publicDir = new URL('../public/', import.meta.url);
const read = (name) => readFileSync(new URL(name, publicDir), 'utf8');
const frame = () => readFileSync(new URL('../leptos/index.html', import.meta.url), 'utf8');

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

test('the frame page paints the cover colour of each scheme first; the frame follows the chosen theme', () => {
  const html = frame();
  // One meta per scheme, with the cover colours of device.mjs; lib.rs sync_theme_color points both at the chosen
  // theme's colour once it is known (an explicit Light or Dark wins over the scheme).
  assert.match(html, /<meta name="theme-color" content="#ECF0F3" media="\(prefers-color-scheme: light\)" \/>/);
  assert.match(html, /<meta name="theme-color" content="#0C1217" media="\(prefers-color-scheme: dark\)" \/>/);
  assert.equal(themeColor({ mode: 'light', screen: 'cover' }), '#ECF0F3');
  assert.equal(themeColor({ mode: 'dark', screen: 'cover' }), '#0C1217');
  const lib = readFileSync(new URL('../leptos/src/lib.rs', import.meta.url), 'utf8');
  assert.match(lib, /const THEME_COLOR_LIGHT: &str = "#ECF0F3";/);
  assert.match(lib, /const THEME_COLOR_DARK: &str = "#0C1217";/);
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

test('the frame page opts into the measured standalone status-strip viewport', () => {
  const html = frame();
  assert.match(
    html,
    /<meta name="apple-mobile-web-app-status-bar-style" content="black-translucent" \/>/
  );
  assert.doesNotMatch(html, /name="apple-mobile-web-app-status-bar-style" content="default"/);
});
