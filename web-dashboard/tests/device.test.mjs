import test from 'node:test';
import assert from 'node:assert/strict';
import {
  px,
  readSafeArea,
  keyboardHeight,
  isAppleMobile,
  installRow,
  themeColor,
  themeScreen,
  viewportSize,
} from '../public/device.mjs';

test('px parses CSS lengths', () => {
  assert.equal(px('47px'), 47);
  assert.equal(px('0px'), 0);
  assert.equal(px('12.5px'), 12.5);
  assert.equal(px(''), 0);
  assert.equal(px('auto'), 0);
  assert.equal(px('-4px'), 0);
  assert.equal(px(null), 0);
});

test('readSafeArea reads the probe paddings', () => {
  const styleOf = () => ({ paddingTop: '47px', paddingRight: '0px', paddingBottom: '34px', paddingLeft: '0px' });
  assert.deepEqual(readSafeArea({}, styleOf), { top: 47, right: 0, bottom: 34, left: 0 });
});

test('readSafeArea tolerates a missing probe or style', () => {
  assert.deepEqual(readSafeArea(null, () => ({})), { top: 0, right: 0, bottom: 0, left: 0 });
  assert.deepEqual(readSafeArea({}, null), { top: 0, right: 0, bottom: 0, left: 0 });
  assert.deepEqual(readSafeArea({}, () => { throw new Error('gone'); }), { top: 0, right: 0, bottom: 0, left: 0 });
  assert.deepEqual(readSafeArea({}, () => ({})), { top: 0, right: 0, bottom: 0, left: 0 });
});

test('keyboardHeight is the covered band only while focused', () => {
  assert.equal(keyboardHeight(844, { height: 464, offsetTop: 0 }, true), 380);
  assert.equal(keyboardHeight(844, { height: 464, offsetTop: 10 }, true), 370);
  assert.equal(keyboardHeight(844, { height: 844, offsetTop: 0 }, true), 0);
  assert.equal(keyboardHeight(844, { height: 464, offsetTop: 0 }, false), 0);
  assert.equal(keyboardHeight(844, null, true), 0);
});

test('isAppleMobile matches iPhone, iPad and touch Macs', () => {
  assert.equal(isAppleMobile('Mozilla/5.0 (iPhone; CPU iPhone OS 26_4 like Mac OS X)'), true);
  assert.equal(isAppleMobile('Mozilla/5.0 (iPad; CPU OS 26_4 like Mac OS X)'), true);
  assert.equal(isAppleMobile('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)', { touchPoints: 5 }), true);
  assert.equal(isAppleMobile('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)', { touchPoints: 0 }), false);
  assert.equal(isAppleMobile('Mozilla/5.0 (Linux; Android 16; Pixel 10)'), false);
});

test('installRow picks the install hint', () => {
  assert.equal(installRow({ standalone: true, deferredPrompt: true }), 'hidden');
  assert.equal(installRow({ deferredPrompt: true }), 'chromium');
  assert.equal(installRow({ appleMobile: true }), 'ios');
  assert.equal(installRow({}), 'hidden');
});

test('themeColor follows the resolved theme and the screen', () => {
  assert.equal(themeColor({ mode: 'light', screen: 'dashboard' }), '#FBFCFD');
  assert.equal(themeColor({ mode: 'dark', screen: 'dashboard' }), '#131B23');
  assert.equal(themeColor({ mode: 'auto', systemDark: true, screen: 'dashboard' }), '#131B23');
  assert.equal(themeColor({ mode: 'auto', systemDark: false, screen: 'dashboard' }), '#FBFCFD');
  assert.equal(themeColor({ mode: 'light', screen: 'cover' }), '#ECF0F3');
  assert.equal(themeColor({ mode: 'dark', screen: 'cover' }), '#0C1217');
  // Forcing dark while the OS is light still resolves dark.
  assert.equal(themeColor({ mode: 'dark', systemDark: false, screen: 'cover' }), '#0C1217');
});

test('themeScreen is the dashboard only once signed in', () => {
  assert.equal(themeScreen(true), 'dashboard');
  assert.equal(themeScreen(false), 'cover');
});

test('viewportSize returns inner dimensions on desktop or non-standalone', () => {
  // Desktop
  assert.deepEqual(
    viewportSize({ innerWidth: 1440, innerHeight: 900, standalone: false, appleMobile: false }),
    { width: 1440, height: 900 }
  );
  // Android standalone
  assert.deepEqual(
    viewportSize({ innerWidth: 412, innerHeight: 915, screenWidth: 412, screenHeight: 915, standalone: true, appleMobile: false }),
    { width: 412, height: 915 }
  );
  // iOS Safari (not standalone)
  assert.deepEqual(
    viewportSize({ innerWidth: 402, innerHeight: 714, screenWidth: 402, screenHeight: 874, standalone: false, appleMobile: true }),
    { width: 402, height: 714 }
  );
});

test('viewportSize expands layout height to full screen in standalone iOS (portrait & landscape)', () => {
  // iPhone 17/18 Pro portrait standalone: innerHeight is 812 pt, but full screen is 874 pt
  assert.deepEqual(
    viewportSize({ innerWidth: 402, innerHeight: 812, screenWidth: 402, screenHeight: 874, standalone: true, appleMobile: true }),
    { width: 402, height: 874 }
  );
  // iPhone landscape standalone: innerWidth is 874, innerHeight is 360 pt
  assert.deepEqual(
    viewportSize({ innerWidth: 874, innerHeight: 360, screenWidth: 402, screenHeight: 874, standalone: true, appleMobile: true }),
    { width: 874, height: 402 }
  );
});

test('viewportSize handles fallback values gracefully', () => {
  assert.deepEqual(viewportSize({}), { width: 0, height: 0 });
  assert.deepEqual(viewportSize({ innerWidth: null, innerHeight: undefined }), { width: 0, height: 0 });
  assert.deepEqual(viewportSize({ innerWidth: 300, innerHeight: 600 }), { width: 300, height: 600 });
});
