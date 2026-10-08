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

test('viewportSize sizes from layout probe dimensions when resolved', () => {
  // Desktop layout probe
  assert.deepEqual(
    viewportSize({ layoutWidth: 1440, layoutHeight: 900, innerWidth: 1440, innerHeight: 900 }),
    { width: 1440, height: 900 }
  );
  // Mobile Safari layout probe
  assert.deepEqual(
    viewportSize({ layoutWidth: 402, layoutHeight: 714, innerWidth: 402, innerHeight: 714 }),
    { width: 402, height: 714 }
  );
  // Standalone iOS PWA layout probe with default status bar (62pt status bar -> 812pt content)
  assert.deepEqual(
    viewportSize({ layoutWidth: 402, layoutHeight: 812, innerWidth: 402, innerHeight: 812 }),
    { width: 402, height: 812 }
  );
  // Backward compatibility: canvasWidth / canvasHeight still honored if supplied
  assert.deepEqual(
    viewportSize({ canvasWidth: 402, canvasHeight: 812, innerWidth: 402, innerHeight: 812 }),
    { width: 402, height: 812 }
  );
});

test('viewportSize tracks runtime window size and orientation transitions', () => {
  // Sequence 1: 1280x800 -> 600x800 -> 800x600 -> 1280x800
  const step1 = viewportSize({ layoutWidth: 1280, layoutHeight: 800, innerWidth: 1280, innerHeight: 800 });
  assert.deepEqual(step1, { width: 1280, height: 800 });

  const step2 = viewportSize({ layoutWidth: 600, layoutHeight: 800, innerWidth: 600, innerHeight: 800 });
  assert.deepEqual(step2, { width: 600, height: 800 });

  const step3 = viewportSize({ layoutWidth: 800, layoutHeight: 600, innerWidth: 800, innerHeight: 600 });
  assert.deepEqual(step3, { width: 800, height: 600 });

  const step4 = viewportSize({ layoutWidth: 1280, layoutHeight: 800, innerWidth: 1280, innerHeight: 800 });
  assert.deepEqual(step4, { width: 1280, height: 800 });

  // Sequence 2: Phone portrait -> landscape -> portrait
  const portrait = viewportSize({ layoutWidth: 402, layoutHeight: 812, innerWidth: 402, innerHeight: 812 });
  assert.deepEqual(portrait, { width: 402, height: 812 });

  const landscape = viewportSize({ layoutWidth: 812, layoutHeight: 402, innerWidth: 812, innerHeight: 402 });
  assert.deepEqual(landscape, { width: 812, height: 402 });

  const backToPortrait = viewportSize({ layoutWidth: 402, layoutHeight: 812, innerWidth: 402, innerHeight: 812 });
  assert.deepEqual(backToPortrait, { width: 402, height: 812 });
});

test('viewportSize layout probe breaks renderer canvas feedback loop', () => {
  // If renderer canvas has stale inline style (e.g. 1280x800 from prior render),
  // layoutWidth/layoutHeight probe takes precedence over stale canvas dimensions.
  const resized = viewportSize({
    layoutWidth: 600,
    layoutHeight: 800,
    canvasWidth: 1280, // stale inline renderer canvas
    canvasHeight: 800,
    innerWidth: 600,
    innerHeight: 800,
  });
  assert.deepEqual(resized, { width: 600, height: 800 });
  assert.notEqual(resized.width, 1280);
});

test('viewportSize falls back to inner dimensions when canvas is not yet sized', () => {
  assert.deepEqual(
    viewportSize({ innerWidth: 1440, innerHeight: 900 }),
    { width: 1440, height: 900 }
  );
  assert.deepEqual(
    viewportSize({ innerWidth: 402, innerHeight: 714 }),
    { width: 402, height: 714 }
  );
  assert.deepEqual(
    viewportSize({ innerWidth: 412, innerHeight: 915 }),
    { width: 412, height: 915 }
  );
});

test('viewportSize preserves iPad Split View, Stage Manager and rotation without screen substitution', () => {
  // iPad Split View (e.g. 600x800 on 820x1180 screen): must preserve 600x800, NOT physical screen 820x1180
  const splitView = viewportSize({ canvasWidth: 600, canvasHeight: 800, innerWidth: 600, innerHeight: 800 });
  assert.deepEqual(splitView, { width: 600, height: 800 });
  assert.notEqual(splitView.width, 820);
  assert.notEqual(splitView.height, 1180);

  // Stage Manager windowed PWA (e.g. 700x600)
  assert.deepEqual(
    viewportSize({ canvasWidth: 700, canvasHeight: 600, innerWidth: 700, innerHeight: 600 }),
    { width: 700, height: 600 }
  );

  // External display window (e.g. 1920x1080)
  assert.deepEqual(
    viewportSize({ canvasWidth: 1920, canvasHeight: 1080, innerWidth: 1920, innerHeight: 1080 }),
    { width: 1920, height: 1080 }
  );

  // Rotation: portrait (600x800) vs landscape (800x600)
  assert.deepEqual(
    viewportSize({ canvasWidth: 600, canvasHeight: 800 }),
    { width: 600, height: 800 }
  );
  assert.deepEqual(
    viewportSize({ canvasWidth: 800, canvasHeight: 600 }),
    { width: 800, height: 600 }
  );
});

test('keyboardHeight computes covered band without false keyboard at idle', () => {
  // Idle (not focused): always 0 regardless of dimensions
  assert.equal(keyboardHeight(800, { height: 800, offsetTop: 0 }, false), 0);
  assert.equal(keyboardHeight(800, { height: 500, offsetTop: 0 }, false), 0);
  assert.equal(keyboardHeight(800, null, false), 0);

  // Idle when focused is true but visual viewport matches layout height: 0 (no false keyboard)
  assert.equal(keyboardHeight(800, { height: 800, offsetTop: 0 }, true), 0);
  assert.equal(keyboardHeight(600, { height: 600, offsetTop: 0 }, true), 0);

  // Active keyboard: layout height 800, visualViewport height 520 -> 280
  assert.equal(keyboardHeight(800, { height: 520, offsetTop: 0 }, true), 280);

  // Active keyboard on windowed iPad: layout height 600, visualViewport height 350 -> 250
  assert.equal(keyboardHeight(600, { height: 350, offsetTop: 0 }, true), 250);
});

test('viewportSize handles fallback values gracefully', () => {
  assert.deepEqual(viewportSize({}), { width: 0, height: 0 });
  assert.deepEqual(viewportSize({ innerWidth: null, innerHeight: undefined }), { width: 0, height: 0 });
  assert.deepEqual(viewportSize({ innerWidth: 300, innerHeight: 600 }), { width: 300, height: 600 });
  assert.deepEqual(viewportSize({ canvasWidth: 300.4, canvasHeight: 600.6 }), { width: 300, height: 601 });
});
