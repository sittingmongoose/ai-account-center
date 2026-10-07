// The service worker's routing rules (DESIGN-MOBILE.md 6.5 and the orchestrator's
// 6.5 correction): which requests the worker may touch, and how.
//
// Pure: no fetch, cache, DOM or worker API is used here. sw.js loads this file
// through importScripts (classic worker script, no modules); node:test loads it
// through require. Every number, label and caching decision stays here (tested).
(function (root, factory) {
  'use strict';
  var api = factory();
  if (typeof module === 'object' && module !== null && typeof module.exports === 'object') {
    module.exports = api;
  } else {
    root.AacSwRoute = api;
  }
})(typeof self !== 'undefined' ? self : globalThis, function () {
  'use strict';

  /** The wasm content hash scripts/build-ui.js stamps into the packaged sw.js. */
  var BUILD_ID_PATTERN = /^[a-f0-9]{12}$/;
  /** Content-addressed runtime: only pkg/<buildId>/** is immutable (6.5 correction). */
  var VERSIONED_PKG = /^\/pkg\/[a-f0-9]{12}\//;
  /** Never cached or intercepted: APIs, the usage hub, the socket, in any letter case. */
  var NEVER_CACHED = [/^\/api(\/|$)/, /^\/v0(\/|$)/, /^\/ws$/];
  /** A navigation waits this long for the network before reading the cache. */
  var NAVIGATION_TIMEOUT_MS = 3000;
  /** The offline fallback answers navigations only when neither network nor cache has the shell. */
  var OFFLINE_STATUS = 503;
  /** The installed app launches here, so every stored page shell is mirrored to it. */
  var START_URL_PATHNAME = '/';
  /** Offline fallback lookups, in order: same URL, start URL, any shell. */
  var SAME_URL_IGNORE_SEARCH = 'same-url-ignore-search';
  var START_URL = 'start-url';
  var ANY_NAVIGATION_SHELL = 'any-navigation-shell';

  /** The one cache name for a build; every deploy installs a new worker and drops the old caches. */
  function cacheName(buildId) {
    if (!BUILD_ID_PATTERN.test(buildId || '')) throw new Error('Invalid service worker build id.');
    return 'aac-shell-' + buildId;
  }

  /** Whether this pathname must pass through untouched (compared in lower case). */
  function isNeverCached(pathname) {
    var lower = String(pathname || '/').toLowerCase();
    return NEVER_CACHED.some(function (pattern) {
      return pattern.test(lower);
    });
  }

  /**
   * How sw.js answers a request: 'passthrough' (the worker does not call
   * respondWith), 'network-first' (the shell: index.html, bridge.js, *.mjs,
   * fonts, icons, page routes) or 'cache-first' (the immutable pkg runtime).
   * `request` is { method, sameOrigin, pathname }; anything cross-origin or not
   * a GET passes through, as do the never-cached paths whatever the method.
   */
  function route(request) {
    if (!request || request.method !== 'GET' || request.sameOrigin !== true) return 'passthrough';
    var pathname = request.pathname || '/';
    if (isNeverCached(pathname)) return 'passthrough';
    if (VERSIONED_PKG.test(pathname)) return 'cache-first';
    return 'network-first';
  }

  /** Navigations race the network against the timeout; subresources wait for their answer. */
  function navigationTimeoutMs(isNavigation) {
    return isNavigation === true ? NAVIGATION_TIMEOUT_MS : 0;
  }

  /**
   * Whether a fetched response may be stored: a plain same-origin 200 with no
   * visible auth data. Anything else (an opaque cross-origin answer, an error,
   * a response carrying a cookie) is served but never written to the cache.
   */
  function cacheable(response) {
    if (!response || response.status !== 200) return false;
    if (response.type !== 'basic' && response.type !== 'default') return false;
    try {
      if (
        response.headers &&
        typeof response.headers.has === 'function' &&
        response.headers.has('set-cookie')
      )
        return false;
    } catch {
      return false;
    }
    return true;
  }

  /**
   * Ordered cache lookups for an offline navigation whose own entry missed:
   * the same URL ignoring the query (so /?e2e finds / and /accounts?x=1 finds
   * /accounts), the cached start-URL shell, then any cached navigation shell.
   * Only network-first paths get a shell fallback: /api, /v0, /ws and
   * anything else route() passes through (or serves cache-first) returns no
   * lookups. `request` is { method, sameOrigin, pathname }, as route() takes.
   */
  function navigationFallbackLookups(request) {
    if (route(request) !== 'network-first') return [];
    return [
      { lookup: SAME_URL_IGNORE_SEARCH },
      { lookup: START_URL, pathname: START_URL_PATHNAME },
      { lookup: ANY_NAVIGATION_SHELL },
    ];
  }

  /**
   * Whether a stored navigation is also stored under the start URL, so the
   * installed app's first offline launch finds a shell. Only HTML page-route
   * navigations qualify: /api, /v0, /ws and anything else route() passes
   * through never mirrors, and neither do subresources (never HTML), the
   * start URL itself or non-navigation requests. `request` is
   * { method, sameOrigin, pathname, isNavigation }; `response` is the fetched
   * response (only its content type is read).
   */
  function shouldMirrorStartUrl(request, response) {
    if (!request || request.isNavigation !== true) return false;
    if (!isHtmlDocument(response)) return false;
    if (route(request) !== 'network-first') return false;
    return String(request.pathname || '/') !== START_URL_PATHNAME;
  }

  /**
   * Whether a response is a navigation shell: an HTML document, which is what
   * every page route serves. Subresources (scripts, fonts, icons, the wasm
   * runtime) never qualify, so the any-shell scan cannot answer a document
   * with one of them.
   */
  function isHtmlDocument(response) {
    try {
      var type = response && response.headers ? response.headers.get('content-type') : null;
      return typeof type === 'string' && type.toLowerCase().indexOf('text/html') !== -1;
    } catch {
      return false;
    }
  }

  return {
    BUILD_ID_PATTERN: BUILD_ID_PATTERN,
    NAVIGATION_TIMEOUT_MS: NAVIGATION_TIMEOUT_MS,
    OFFLINE_STATUS: OFFLINE_STATUS,
    START_URL_PATHNAME: START_URL_PATHNAME,
    cacheName: cacheName,
    isNeverCached: isNeverCached,
    route: route,
    navigationTimeoutMs: navigationTimeoutMs,
    cacheable: cacheable,
    navigationFallbackLookups: navigationFallbackLookups,
    shouldMirrorStartUrl: shouldMirrorStartUrl,
    isHtmlDocument: isHtmlDocument,
  };
});
