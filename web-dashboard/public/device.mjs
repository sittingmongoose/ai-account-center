// Device environment for the responsive dashboard (DESIGN-MOBILE.md 1.1, 6.2-6.3, 6.6).
//
// Pure helpers; bridge.js wires them to the browser and pushes the results into the Device
// global through the set-safe-area, set-input-profile, set-keyboard and set-online wasm exports.
// Every number, label and visibility decision stays here (tested); Slint only lays out.

/** Parse a CSS px length ("47px") into a finite number, else 0. */
export function px(value) {
  if (typeof value !== 'string') return 0;
  const match = /^(-?\d+(?:\.\d+)?)px$/.exec(value.trim());
  const parsed = match ? Number(match[1]) : NaN;
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
}

/**
 * Read the live safe-area insets from a probe element whose paddings are the env() values
 * (padding-top: env(safe-area-inset-top), and so on). `styleOf` is getComputedStyle.
 */
export function readSafeArea(probe, styleOf) {
  const zero = { top: 0, right: 0, bottom: 0, left: 0 };
  if (!probe || typeof styleOf !== 'function') return zero;
  let style;
  try {
    style = styleOf(probe);
  } catch {
    return zero;
  }
  if (!style) return zero;
  return {
    top: px(style.paddingTop),
    right: px(style.paddingRight),
    bottom: px(style.paddingBottom),
    left: px(style.paddingLeft),
  };
}

/**
 * The keyboard height covering the canvas (DESIGN-MOBILE.md 1.1): the layout viewport minus the
 * visual viewport, only while a text input has focus. Android Chrome resizes the layout viewport
 * (interactive-widget=resizes-content), so this is about 0 there; iOS keeps the layout viewport
 * and this is the keyboard plus any AutoFill bar.
 */
export function keyboardHeight(innerHeight, viewport, focused) {
  if (focused !== true) return 0;
  const height = viewport?.height;
  const offsetTop = viewport?.offsetTop ?? 0;
  if (!Number.isFinite(innerHeight) || !Number.isFinite(height)) return 0;
  return Math.max(0, innerHeight - height - (Number.isFinite(offsetTop) ? offsetTop : 0));
}

/** iOS or iPadOS (including iPadOS reporting as desktop Safari), never trusted for more than UI copy. */
export function isAppleMobile(ua, { standalone = false, touchPoints = 0 } = {}) {
  const agent = typeof ua === 'string' ? ua : '';
  if (/iPhone|iPad|iPod/i.test(agent)) return true;
  // iPadOS 13+ reports "Macintosh" but has touch points; iPhones never report Macintosh.
  if (/Macintosh/i.test(agent) && touchPoints > 1) return true;
  void standalone;
  return false;
}

/**
 * Which "Home screen app" row Settings shows (DESIGN-MOBILE.md 6.6): 'chromium' (Install button),
 * 'ios' (Share-menu guidance) or 'hidden' (standalone already, or no install path).
 */
export function installRow({ standalone = false, deferredPrompt = false, appleMobile = false } = {}) {
  if (standalone) return 'hidden';
  if (deferredPrompt) return 'chromium';
  if (appleMobile) return 'ios';
  return 'hidden';
}

const THEME_COLORS = {
  dashboard: { light: '#FBFCFD', dark: '#131B23' },
  cover: { light: '#ECF0F3', dark: '#0C1217' },
};

/**
 * Whether the resolved app theme is dark (DESIGN-MOBILE.md 6.3): an explicit mode wins,
 * otherwise the OS setting decides. The inline first-paint script in index.html mirrors this.
 */
export function isDarkTheme({ mode = 'auto', systemDark = false } = {}) {
  return mode === 'dark' || (mode !== 'light' && systemDark === true);
}

/**
 * The theme-color for the resolved app theme and the current screen (DESIGN-MOBILE.md 6.3):
 * dashboard screens use paper so the Android status bar matches the top bar; sign-in, offline
 * and loading use the background. `screen` is 'dashboard' or anything else.
 */
export function themeColor({ mode = 'auto', systemDark = false, screen = 'dashboard' } = {}) {
  const table = screen === 'dashboard' ? THEME_COLORS.dashboard : THEME_COLORS.cover;
  return isDarkTheme({ mode, systemDark }) ? table.dark : table.light;
}

/** The app-screen bucket for theme-color: 'dashboard' once signed in, else the cover screens. */
export function themeScreen(authenticated) {
  return authenticated === true ? 'dashboard' : 'cover';
}

/**
 * Layout viewport dimensions ({ width, height }) for the Slint canvas.
 *
 * Resolves the dimensions from the actual canvas element after CSS layout
 * (e.g. canvas.clientWidth / clientHeight or getBoundingClientRect),
 * falling back to the window inner dimensions.
 *
 * Never substitutes physical screen dimensions, preserving windowed environments
 * (iPad Split View, Stage Manager / windowed PWA, external displays) and preventing
 * false keyboard heights and offscreen controls.
 */
export function viewportSize({
  canvasWidth,
  canvasHeight,
  innerWidth = 0,
  innerHeight = 0,
} = {}) {
  const cw = Number.isFinite(canvasWidth) && canvasWidth > 0 ? Math.round(canvasWidth) : 0;
  const ch = Number.isFinite(canvasHeight) && canvasHeight > 0 ? Math.round(canvasHeight) : 0;
  const iw = Number.isFinite(innerWidth) && innerWidth > 0 ? Math.round(innerWidth) : 0;
  const ih = Number.isFinite(innerHeight) && innerHeight > 0 ? Math.round(innerHeight) : 0;

  return {
    width: cw > 0 ? cw : iw,
    height: ch > 0 ? ch : ih,
  };
}
