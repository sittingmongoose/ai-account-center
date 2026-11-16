'use strict';

// What the service worker downloads before it may replace the installed copy of the app (web-dashboard/public/sw.js
// PRECACHE): the frame page's addresses and every packaged asset the app can ask for. build-ui.js stamps the list into
// the packaged worker; verify-bundle.js derives it again from dist/ui and refuses a worker whose list differs, so a
// worker can never activate with less than a whole app (review M1).

const crypto = require('crypto');

/** The placeholder in the source sw.js that build-ui.js replaces with the list (a JSON array). */
const SW_PRECACHE = '[/*__AAC_PRECACHE__*/]';
const SW_PRECACHE_ERROR = 'sw.js must name the [/*__AAC_PRECACHE__*/] placeholder exactly once.';
/** Canonical frame addresses. sw-route.js maps the server's supported single trailing slash to these entries. */
const PAGE_ADDRESSES = Object.freeze([
  '/',
  '/app/home',
  '/app/analytics',
  '/app/accounts',
  '/app/status',
  '/app/scoreboard',
]);
/** The frame page file itself, which the server answers at PAGE_ADDRESSES, never at its own path. */
const APP_ENTRY = 'app/index.html';
/** Packaged files no page asks for: the worker itself, the build record, type declarations, licences and notes. */
const NOT_REQUESTED = /^(sw\.js|sw-route\.js|ui-build-manifest\.json)$|\.(d\.ts|txt|md|br|gz)$/;

/** The stylesheet's version, as the frame page names it (build-ui.js stagedAppFiles: `app.css?v=<12 hex>`). */
function cssVersion(css) {
  return crypto.createHash('sha256').update(css).digest('hex').slice(0, 12);
}

/**
 * The precache list for a packaged UI: PAGE_ADDRESSES, then every packaged file (relative POSIX paths) a page can
 * request, sorted, with the stylesheet at the versioned address the page links. `css` is app/app.css's bytes.
 */
function precacheList(files, css) {
  const assets = [...files]
    .filter((file) => file !== APP_ENTRY && !NOT_REQUESTED.test(file))
    .sort()
    .map((file) => (file === 'app/app.css' ? `/app/app.css?v=${cssVersion(css)}` : `/${file}`));
  return [...PAGE_ADDRESSES, ...assets];
}

/** The list as it is written into sw.js. */
function precacheLiteral(list) {
  return JSON.stringify(list);
}

/** The list a packaged sw.js carries, or null when it carries none (an unstamped or foreign worker). */
function stampedPrecache(swSource) {
  const match = /var PRECACHE = (\[[^\]]*\]);/.exec(swSource);
  if (!match) return null;
  try {
    const list = JSON.parse(match[1]);
    return Array.isArray(list) && list.every((entry) => typeof entry === 'string') ? list : null;
  } catch {
    return null;
  }
}

module.exports = {
  SW_PRECACHE,
  SW_PRECACHE_ERROR,
  PAGE_ADDRESSES,
  precacheList,
  precacheLiteral,
  stampedPrecache,
  cssVersion,
};
