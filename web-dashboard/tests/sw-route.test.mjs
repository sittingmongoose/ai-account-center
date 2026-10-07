import test from 'node:test';
import assert from 'node:assert/strict';
import routeModule from '../public/sw-route.js';

const {
  NAVIGATION_TIMEOUT_MS,
  cacheName,
  isNeverCached,
  route,
  navigationTimeoutMs,
  cacheable,
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
  assert.equal(route({ ...same, pathname: '/pkg/0123456789ab/ccs_account_dashboard_bg.wasm' }), 'cache-first');
  assert.equal(route({ ...same, pathname: '/pkg/0123456789ab/ccs_account_dashboard.js' }), 'cache-first');
});

test('an unversioned pkg path is never treated as immutable', () => {
  const same = { method: 'GET', sameOrigin: true };
  assert.equal(route({ ...same, pathname: '/pkg/ccs_account_dashboard.js' }), 'network-first');
  assert.equal(route({ ...same, pathname: '/pkg/latest/ccs_account_dashboard.js' }), 'network-first');
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
  assert.equal(route({ method: 'POST', sameOrigin: true, pathname: '/api/auth/login' }), 'passthrough');
  assert.equal(route({ method: 'GET', sameOrigin: false, pathname: '/api/auth/check' }), 'passthrough');
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

test('only plain same-origin 200s without auth data are stored', () => {
  const headers = (entries = {}) => ({ has: (name) => name.toLowerCase() in entries });
  assert.equal(cacheable({ status: 200, type: 'basic', headers: headers() }), true);
  assert.equal(cacheable({ status: 200, type: 'basic', headers: headers({ 'content-type': 'x' }) }), true);
  assert.equal(cacheable({ status: 200, type: 'basic', headers: headers({ 'set-cookie': 'a=b' }) }), false);
  assert.equal(cacheable({ status: 200, type: 'opaque', headers: headers() }), false);
  assert.equal(cacheable({ status: 200, type: 'opaqueredirect', headers: headers() }), false);
  assert.equal(cacheable({ status: 200, type: 'error', headers: headers() }), false);
  assert.equal(cacheable({ status: 200, type: 'cors', headers: headers() }), false);
  assert.equal(cacheable({ status: 404, type: 'basic', headers: headers() }), false);
  assert.equal(cacheable({ status: 0, type: 'basic', headers: headers() }), false);
  assert.equal(cacheable(null), false);
  assert.equal(cacheable({ status: 200, type: 'basic', headers: { has() { throw new Error('gone'); } } }), false);
});
