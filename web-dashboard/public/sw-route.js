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

  return {
    BUILD_ID_PATTERN: BUILD_ID_PATTERN,
    NAVIGATION_TIMEOUT_MS: NAVIGATION_TIMEOUT_MS,
    OFFLINE_STATUS: OFFLINE_STATUS,
    cacheName: cacheName,
    isNeverCached: isNeverCached,
    route: route,
    navigationTimeoutMs: navigationTimeoutMs,
    cacheable: cacheable,
  };
});
