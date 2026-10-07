// The app shell's service worker (DESIGN-MOBILE.md 6.5): offline and fast start.
// scripts/build-ui.js stamps BUILD_ID into the packaged copy; each deploy then
// installs a new worker, which activates on the next launch (no skipWaiting, no
// update prompt) and drops the previous build's caches. Routing rules live in
// the pure sw-route.js (tested); this file only runs them against fetch/cache.
importScripts('./sw-route.js');

var BUILD_ID = '__AAC_BUILD_ID__';
var CACHE = AacSwRoute.cacheName(BUILD_ID);

// First visit while offline: neither the network nor the cache has the shell.
// Once the app has loaded, the in-app Offline screen (phase 5) takes over.
var OFFLINE_PAGE =
  '<!doctype html><html lang="en"><head><meta charset="utf-8">' +
  '<meta name="viewport" content="width=device-width,initial-scale=1">' +
  '<meta name="theme-color" media="(prefers-color-scheme: light)" content="#ECF0F3">' +
  '<meta name="theme-color" media="(prefers-color-scheme: dark)" content="#0C1217">' +
  '<title>AI Account Center</title>' +
  '<style>html,body{margin:0;height:100%}body{display:flex;align-items:center;justify-content:center;' +
  'background:#ECF0F3;color:#15202B;font:15px/1.5 system-ui,sans-serif;text-align:center}' +
  '@media (prefers-color-scheme:dark){body{background:#0C1217;color:#E6EBF0}}' +
  'main{padding:24px}a{color:#2552CC}</style></head><body><main>' +
  '<h1>Can&rsquo;t reach the dashboard</h1>' +
  '<p>The app shell has not been downloaded yet, and there is no connection.</p>' +
  '<p><a href="/">Try again</a></p></main></body></html>';

function fromNetwork(request, timeoutMs) {
  if (!(timeoutMs > 0)) return fetch(request);
  var controller = new AbortController();
  var timer = setTimeout(function () {
    controller.abort();
  }, timeoutMs);
  return fetch(request, { signal: controller.signal }).then(
    function (response) {
      clearTimeout(timer);
      return response;
    },
    function (error) {
      clearTimeout(timer);
      throw error;
    }
  );
}

function store(request, response) {
  if (!AacSwRoute.cacheable(response)) return Promise.resolve();
  var copy = response.clone();
  return caches
    .open(CACHE)
    .then(function (cache) {
      return cache.put(request, copy);
    })
    .catch(function () {});
}

// The shell (index.html, bridge.js, *.mjs, fonts, icons, page routes): the
// network first, so the shell stays in step with the server, then the cache.
function networkFirst(request, timeoutMs) {
  return fromNetwork(request, timeoutMs).then(
    function (response) {
      store(request, response);
      return response;
    },
    function () {
      return caches.match(request).then(function (cached) {
        if (cached) return cached;
        if (request.mode === 'navigate') {
          return new Response(OFFLINE_PAGE, {
            status: AacSwRoute.OFFLINE_STATUS,
            headers: { 'Content-Type': 'text/html; charset=utf-8' },
          });
        }
        throw new Error('The app shell is not cached yet.');
      });
    }
  );
}

// The content-addressed runtime: immutable, so the cache answers first.
function cacheFirst(request) {
  return caches.match(request).then(function (cached) {
    if (cached) return cached;
    return fetch(request).then(function (response) {
      store(request, response);
      return response;
    });
  });
}

self.addEventListener('fetch', function (event) {
  var request = event.request;
  var url;
  try {
    url = new URL(request.url);
  } catch {
    return;
  }
  var decision = AacSwRoute.route({
    method: request.method,
    sameOrigin: url.origin === self.location.origin,
    pathname: url.pathname,
  });
  if (decision === 'cache-first') {
    event.respondWith(cacheFirst(request));
  } else if (decision === 'network-first') {
    event.respondWith(
      networkFirst(request, AacSwRoute.navigationTimeoutMs(request.mode === 'navigate'))
    );
  }
  // 'passthrough': /api, /v0, /ws, non-GET and cross-origin requests are never
  // intercepted, so no respondWith here.
});

self.addEventListener('activate', function (event) {
  event.waitUntil(
    caches
      .keys()
      .then(function (names) {
        return Promise.all(
          names
            .filter(function (name) {
              return name !== CACHE && name.indexOf('aac-shell-') === 0;
            })
            .map(function (name) {
              return caches.delete(name);
            })
        );
      })
      .then(function () {
        return self.clients.claim();
      })
  );
});
