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

// Stores a copy of a fetched response. The copies are cloned right away, before
// the original goes back to the page and its body is read; cloning later, after
// the cache opened, failed silently and left the cache empty.
function store(request, response) {
  if (!AacSwRoute.cacheable(response)) return Promise.resolve();
  var copy = response.clone();
  var mirror = null;
  try {
    var url = new URL(request.url);
    if (
      AacSwRoute.shouldMirrorStartUrl(
        {
          method: request.method,
          sameOrigin: url.origin === self.location.origin,
          pathname: url.pathname,
          isNavigation: request.mode === 'navigate',
        },
        response
      )
    ) {
      // Every page route serves the same index.html: mirror it under the
      // start URL, so the installed app's first offline launch finds a shell.
      mirror = new URL(AacSwRoute.START_URL_PATHNAME, url.origin).toString();
    }
  } catch {
    mirror = null;
  }
  // A separate copy per entry; the original stays with the page.
  var mirrorCopy = mirror ? response.clone() : null;
  return caches
    .open(CACHE)
    .then(function (cache) {
      var puts = [cache.put(request, copy)];
      if (mirror) puts.push(cache.put(mirror, mirrorCopy));
      return Promise.all(puts);
    })
    .catch(function () {});
}

// Keeps the worker alive until the copy is stored.
function storeFor(event, request, response) {
  var done = store(request, response);
  try {
    event.waitUntil(done);
  } catch {
    // The event already finished; the store still runs while the worker lives.
  }
}

// A navigation's cached answer: its own entry, then the pure fallback lookups in
// order (same URL ignoring the query, the start-URL shell, any cached navigation
// shell). Undefined when the cache has no shell at all.
function cachedNavigation(request, url) {
  var lookups = AacSwRoute.navigationFallbackLookups({
    method: request.method,
    sameOrigin: url.origin === self.location.origin,
    pathname: url.pathname,
  });
  var chain = caches.match(request).catch(function () {
    return undefined;
  });
  lookups.forEach(function (lookup) {
    chain = chain.then(function (cached) {
      if (cached) return cached;
      return matchFallback(request, url, lookup).catch(function () {
        return undefined;
      });
    });
  });
  return chain;
}

// A navigation the network could not answer: a cached shell, and only then the
// bare offline page.
function offlineNavigation(request, url) {
  return cachedNavigation(request, url).then(function (cached) {
    if (cached) return cached;
    return new Response(OFFLINE_PAGE, {
      status: AacSwRoute.OFFLINE_STATUS,
      headers: { 'Content-Type': 'text/html; charset=utf-8' },
    });
  });
}

function matchFallback(request, url, lookup) {
  if (!lookup) return Promise.resolve(undefined);
  if (lookup.lookup === 'same-url-ignore-search') {
    return caches.match(request, { ignoreSearch: true });
  }
  if (lookup.lookup === 'start-url') {
    return caches.match(new URL(lookup.pathname || '/', url.origin).toString());
  }
  if (lookup.lookup === 'any-navigation-shell') {
    return anyCachedShell();
  }
  return Promise.resolve(undefined);
}

// Any cached navigation shell: the first same-origin cached entry holding an
// HTML document. Every page route serves the same index.html, so any one of
// them shows the in-app Offline card; subresources never qualify.
function anyCachedShell() {
  return caches
    .open(CACHE)
    .then(function (cache) {
      return cache.keys().then(function (keys) {
        var documents = keys.filter(function (key) {
          if (!key || key.method !== 'GET') return false;
          var keyUrl;
          try {
            keyUrl = new URL(key.url);
          } catch {
            return false;
          }
          return keyUrl.origin === self.location.origin;
        });
        var chain = Promise.resolve(undefined);
        documents.forEach(function (key) {
          chain = chain.then(function (found) {
            if (found) return found;
            return cache.match(key).then(function (response) {
              if (response && AacSwRoute.isHtmlDocument(response)) return response;
              return undefined;
            });
          });
        });
        return chain;
      });
    })
    .catch(function () {
      return undefined;
    });
}

// What the cache holds for a shell request: the entry itself, or for a
// navigation any cached shell. Undefined when nothing can answer.
function cachedShell(request) {
  if (request.mode === 'navigate') return cachedNavigation(request, new URL(request.url));
  return caches.match(request).catch(function () {
    return undefined;
  });
}

// The shell (index.html, bridge.js, *.mjs, fonts, icons, page routes): the
// network first, so the shell stays in step with the server, then the cache.
// A navigation gives up on a slow network only when a cached shell can answer
// instead; with nothing cached it waits, since the bare offline page would be
// worse than a slow start. A server error (a proxy's 502 while the dashboard
// restarts) is answered from the cache too when the cache has the shell.
function networkFirst(event, request, timeoutMs) {
  var waited =
    timeoutMs > 0
      ? cachedShell(request).then(function (cached) {
          return cached ? timeoutMs : 0;
        })
      : Promise.resolve(0);
  return waited
    .then(function (ms) {
      return fromNetwork(request, ms);
    })
    .then(
      function (response) {
        if (response.status >= 500 && response.status <= 599) {
          return cachedShell(request).then(function (cached) {
            return cached || response;
          });
        }
        storeFor(event, request, response);
        return response;
      },
      function () {
        return caches.match(request).then(function (cached) {
          if (cached) return cached;
          if (request.mode === 'navigate') {
            return offlineNavigation(request, new URL(request.url));
          }
          throw new Error('The app shell is not cached yet.');
        });
      }
    );
}

// The content-addressed runtime: immutable, so the cache answers first.
function cacheFirst(event, request) {
  return caches.match(request).then(function (cached) {
    if (cached) return cached;
    return fetch(request).then(function (response) {
      storeFor(event, request, response);
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
    event.respondWith(cacheFirst(event, request));
  } else if (decision === 'network-first') {
    event.respondWith(
      networkFirst(event, request, AacSwRoute.navigationTimeoutMs(request.mode === 'navigate'))
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
