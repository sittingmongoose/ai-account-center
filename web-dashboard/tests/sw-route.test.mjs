import test from 'node:test';
import assert from 'node:assert/strict';
import routeModule from '../public/sw-route.js';

const {
  NAVIGATION_TIMEOUT_MS,
  START_URL_PATHNAME,
  cacheName,
  isNeverCached,
  route,
  navigationTimeoutMs,
  cacheable,
  navigationFallbackLookups,
  shouldMirrorStartUrl,
  isHtmlDocument,
} = routeModule;

test('the shell routes network-first and the immutable runtime cache-first', () => {
  const same = { method: 'GET', sameOrigin: true };
  assert.equal(route({ ...same, pathname: '/' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/index.html' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/bridge.js' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/view-model.mjs' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/manifest.webmanifest' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/sw.js' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/sw-route.js' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/assets/InstrumentSans.ttf' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/icons/aac-512.png' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/analytics' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/accounts/codex' }), 'network-first');
  assert.equal(
    route({ ...same, pathname: '/pkg/0123456789ab/ccs_account_dashboard_bg.wasm' }),
    'cache-first'
  );
  assert.equal(
    route({ ...same, pathname: '/pkg/0123456789ab/ccs_account_dashboard.js' }),
    'cache-first'
  );
});

test('an unversioned pkg path is never treated as immutable', () => {
  const same = { method: 'GET', sameOrigin: true };
  assert.equal(route({ ...same, pathname: '/pkg/ccs_account_dashboard.js' }), 'network-first');
  assert.equal(
    route({ ...same, pathname: '/pkg/latest/ccs_account_dashboard.js' }),
    'network-first'
  );
});

test('/api, /v0, /ws and non-GET requests always pass through untouched', () => {
  const same = { method: 'GET', sameOrigin: true };
  for (const pathname of [
    '/api/auth/check',
    '/api',
    '/api/',
    '/API/accounts/registry',
    '/v0/management/usage',
    '/v0',
    '/V0/MANAGEMENT',
    '/ws',
    '/WS',
  ]) {
    assert.equal(route({ ...same, pathname }), 'passthrough');
    assert.equal(isNeverCached(pathname), true);
  }
  // Lookalikes are shell paths, not APIs.
  assert.equal(route({ ...same, pathname: '/apiary' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/v0lt' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/ws-join' }), 'network-first');
  assert.equal(isNeverCached('/apiary'), false);
  // Whatever the method or origin, the worker never intercepts these.
  assert.equal(
    route({ method: 'POST', sameOrigin: true, pathname: '/api/auth/login' }),
    'passthrough'
  );
  assert.equal(
    route({ method: 'GET', sameOrigin: false, pathname: '/api/auth/check' }),
    'passthrough'
  );
  assert.equal(route({ method: 'POST', sameOrigin: true, pathname: '/' }), 'passthrough');
  assert.equal(route({ method: 'GET', sameOrigin: false, pathname: '/' }), 'passthrough');
  assert.equal(route(null), 'passthrough');
});

test('only navigations race the network against the 3 s timeout', () => {
  assert.equal(NAVIGATION_TIMEOUT_MS, 3000);
  assert.equal(navigationTimeoutMs(true), 3000);
  assert.equal(navigationTimeoutMs(false), 0);
  assert.equal(navigationTimeoutMs(undefined), 0);
});

test('each build gets its own cache name; anything else fails closed', () => {
  assert.equal(cacheName('0123456789ab'), 'aac-shell-0123456789ab');
  for (const bad of ['', '__AAC_BUILD_ID__', '0123456789ab\n', 'xyz', null, undefined]) {
    assert.throws(() => cacheName(bad), /Invalid service worker build id/);
  }
});

const nav = (pathname) => ({ method: 'GET', sameOrigin: true, pathname, isNavigation: true });
const subresource = (pathname) => ({
  method: 'GET',
  sameOrigin: true,
  pathname,
  isNavigation: false,
});
const typed = (contentType) => ({
  headers: { get: (name) => (name === 'content-type' ? contentType : null) },
});
const html = (contentType = 'text/html') => typed(contentType);
const script = () => typed('text/javascript; charset=utf-8');

test('offline navigations fall back through the query-stripped URL, / and any shell', () => {
  const same = { method: 'GET', sameOrigin: true };
  const expected = [
    { lookup: 'same-url-ignore-search' },
    { lookup: 'start-url', pathname: '/' },
    { lookup: 'any-navigation-shell' },
  ];
  assert.equal(START_URL_PATHNAME, '/');
  // A page never visited online (/?e2e was cached, / was not) still finds a shell.
  for (const pathname of ['/', '/login', '/analytics', '/accounts', '/accounts/codex']) {
    assert.deepEqual(navigationFallbackLookups({ ...same, pathname }), expected);
  }
});

test('/api, /v0, /ws and other non-shell paths never get a shell fallback', () => {
  const same = { method: 'GET', sameOrigin: true };
  for (const pathname of [
    '/api/auth/check',
    '/api',
    '/API/accounts/registry',
    '/v0/management/usage',
    '/v0',
    '/ws',
    '/WS',
  ]) {
    assert.deepEqual(navigationFallbackLookups({ ...same, pathname }), []);
    assert.equal(shouldMirrorStartUrl(nav(pathname), html()), false);
  }
  // The immutable runtime is not a document; non-GET and cross-origin never qualify.
  assert.deepEqual(
    navigationFallbackLookups({ ...same, pathname: '/pkg/0123456789ab/ccs_account_dashboard.js' }),
    []
  );
  assert.deepEqual(
    navigationFallbackLookups({ method: 'POST', sameOrigin: true, pathname: '/' }),
    []
  );
  assert.deepEqual(
    navigationFallbackLookups({ method: 'GET', sameOrigin: false, pathname: '/' }),
    []
  );
  assert.deepEqual(navigationFallbackLookups(null), []);
});

test('only HTML page-route navigations mirror to the start URL', () => {
  assert.equal(shouldMirrorStartUrl(nav('/accounts'), html()), true);
  assert.equal(shouldMirrorStartUrl(nav('/accounts/codex'), html()), true);
  assert.equal(shouldMirrorStartUrl(nav('/analytics'), html()), true);
  assert.equal(shouldMirrorStartUrl(nav('/login'), html()), true);
  // The start URL itself needs no mirror; subresources are never HTML shells.
  assert.equal(shouldMirrorStartUrl(nav('/'), html()), false);
  assert.equal(shouldMirrorStartUrl(nav('/accounts'), script()), false);
  assert.equal(shouldMirrorStartUrl(nav('/bridge.js'), script()), false);
  // Subresource fetches and missing inputs never mirror either.
  assert.equal(shouldMirrorStartUrl(subresource('/accounts'), html()), false);
  assert.equal(shouldMirrorStartUrl(nav('/accounts'), null), false);
  assert.equal(shouldMirrorStartUrl(null, html()), false);
});

test('only HTML documents count as navigation shells', () => {
  assert.equal(isHtmlDocument(html()), true);
  assert.equal(isHtmlDocument(html('text/html; charset=utf-8')), true);
  assert.equal(isHtmlDocument(html('TEXT/HTML')), true);
  assert.equal(isHtmlDocument(script()), false);
  assert.equal(isHtmlDocument({ headers: { get: () => null } }), false);
  assert.equal(isHtmlDocument(null), false);
  assert.equal(
    isHtmlDocument({
      headers: {
        get() {
          throw new Error('gone');
        },
      },
    }),
    false
  );
});

test('only plain same-origin 200s without auth data are stored', () => {
  const headers = (entries = {}) => ({ has: (name) => name.toLowerCase() in entries });
  assert.equal(cacheable({ status: 200, type: 'basic', headers: headers() }), true);
  assert.equal(
    cacheable({ status: 200, type: 'basic', headers: headers({ 'content-type': 'x' }) }),
    true
  );
  assert.equal(
    cacheable({ status: 200, type: 'basic', headers: headers({ 'set-cookie': 'a=b' }) }),
    false
  );
  assert.equal(cacheable({ status: 200, type: 'opaque', headers: headers() }), false);
  assert.equal(cacheable({ status: 200, type: 'opaqueredirect', headers: headers() }), false);
  assert.equal(cacheable({ status: 200, type: 'error', headers: headers() }), false);
  assert.equal(cacheable({ status: 200, type: 'cors', headers: headers() }), false);
  assert.equal(cacheable({ status: 404, type: 'basic', headers: headers() }), false);
  assert.equal(cacheable({ status: 0, type: 'basic', headers: headers() }), false);
  assert.equal(cacheable(null), false);
  assert.equal(
    cacheable({
      status: 200,
      type: 'basic',
      headers: {
        has() {
          throw new Error('gone');
        },
      },
    }),
    false
  );
});
