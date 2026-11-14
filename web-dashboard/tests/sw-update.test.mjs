// A new build's service worker replaces the installed app only with a whole one (review M1, Sol): it downloads every
// address of its precache list while installing, fails the install if any part fails, and drops the older build's
// cache at activation only once its own copy is complete. The real sw.js and sw-route.js run against fake
// fetch/caches (the browser kit blocks service workers); the stamp is build-ui.js's own.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import buildUi from '../../scripts/build-ui.js';
import precacheModule from '../../scripts/ui-precache.js';

const { stampedSwSource } = buildUi;
const { PAGE_ADDRESSES } = precacheModule;
const publicDir = new URL('../public/', import.meta.url);
const routeSource = fs.readFileSync(new URL('sw-route.js', publicDir), 'utf8');
const ORIGIN = 'http://test';
const OLD = 'aac-shell-000000000001';
const NEW_ID = '00000000000a';
const NEW = `aac-shell-${NEW_ID}`;
const ASSETS = ['/frame-session.mjs', '/app/app.css?v=0123456789ab', '/pkg/0123456789ab/aac_web.js', '/pkg/0123456789ab/aac_web_bg.wasm', '/bridge.js'];
const PRECACHE = [...PAGE_ADDRESSES, ...ASSETS];

const absolute = (url) => (url.startsWith('/') ? ORIGIN + url : url);
const key = (request) => absolute(typeof request === 'string' ? request : request.url);

class FakeCache {
	constructor() {
		this.store = new Map();
	}
	async put(request, response) {
		this.store.set(key(request), response);
	}
	async match(request, opts = {}) {
		const wanted = key(request);
		if (this.store.has(wanted)) return this.store.get(wanted);
		if (opts.ignoreSearch) {
			const u = new URL(wanted);
			for (const [k, v] of this.store) {
				const ku = new URL(k);
				if (ku.origin === u.origin && ku.pathname === u.pathname) return v;
			}
		}
		return undefined;
	}
	async keys() {
		return [...this.store.keys()].map((url) => ({ url, method: 'GET' }));
	}
}

/** Cache storage with named caches; match reads every cache, as the browser's does. */
function fakeCaches() {
	const named = new Map();
	return {
		named,
		async open(name) {
			if (!named.has(name)) named.set(name, new FakeCache());
			return named.get(name);
		},
		async keys() {
			return [...named.keys()];
		},
		async delete(name) {
			return named.delete(name);
		},
		async match(request, opts) {
			for (const cache of named.values()) {
				const found = await cache.match(request, opts);
				if (found) return found;
			}
			return undefined;
		},
	};
}

function response(status, contentType, { redirected = false } = {}) {
	const headers = new Headers({ 'content-type': contentType });
	return { status, type: 'basic', redirected, headers, clone() { return this; } };
}

/** What the server answers: the frame page at its addresses, each asset as itself. */
const served = (address) =>
	PAGE_ADDRESSES.includes(address.split('?')[0])
		? response(200, 'text/html; charset=utf-8')
		: response(200, 'text/javascript');

class FakeRequest {
	constructor(url, init = {}) {
		this.url = absolute(url);
		this.method = 'GET';
		this.mode = init.mode ?? 'cors';
		this.cache = init.cache;
	}
}

/** The packaged worker (stamped with `precache`) in a fake worker scope over `storage`. */
function loadWorker(fetchImpl, storage, precache = PRECACHE) {
	const handlers = {};
	const claimed = { count: 0 };
	const sandbox = {
		URL, URLSearchParams, Headers, Promise, Error, setTimeout, clearTimeout,
		Request: FakeRequest,
		Response: class { constructor(body, init) { this.body = body; this.status = init?.status ?? 200; this.headers = new Headers(init?.headers); } },
		fetch: (request) => fetchImpl(request.url.slice(ORIGIN.length) || '/', request),
		caches: storage,
		clients: { claim: async () => { claimed.count++; } },
	};
	sandbox.self = sandbox;
	sandbox.location = { origin: ORIGIN };
	sandbox.addEventListener = (type, cb) => {
		handlers[type] = cb;
	};
	const ctx = vm.createContext(sandbox);
	sandbox.importScripts = () => vm.runInContext(routeSource, ctx);
	const source = fs.readFileSync(new URL('sw.js', publicDir), 'utf8');
	vm.runInContext(precache === null ? source.replace('__AAC_BUILD_ID__', NEW_ID) : stampedSwSource(source, NEW_ID, precache), ctx);
	return { handlers, claimed };
}

/** Runs a lifecycle event the way the browser does: it settles with everything passed to waitUntil. */
async function lifecycle(handlers, type) {
	const waits = [];
	handlers[type]({ waitUntil: (p) => waits.push(p) });
	return Promise.all(waits);
}

async function navigate(handlers, path) {
	const event = { request: new FakeRequest(path, { mode: 'navigate' }), waitUntil() {}, respondWith(p) { this.promise = p; } };
	handlers.fetch(event);
	return event.promise;
}

/** An installed copy of the previous build: the start page and a tab, as its worker stored them. */
async function oldCopy(storage) {
	const old = await storage.open(OLD);
	await old.put('/', response(200, 'text/html'));
	await old.put('/app/accounts', response(200, 'text/html'));
	return old;
}

test('installing downloads the whole app into the new cache, each address fresh from the server', async () => {
	const storage = fakeCaches();
	const asked = [];
	const { handlers } = loadWorker(async (address, request) => {
		asked.push([address, request.cache]);
		return served(address);
	}, storage);
	await lifecycle(handlers, 'install');
	assert.deepEqual(asked.map(([a]) => a).sort(), [...PRECACHE].sort());
	assert.ok(asked.every(([, mode]) => mode === 'no-cache'), 'the HTTP cache answers only what the server confirms');
	const mine = storage.named.get(NEW);
	for (const address of PRECACHE) assert.ok(await mine.match(address), `${address} is in the new copy`);
});

test('one failed, redirected or wrong-kind answer fails the install and leaves the installed copy alone', async () => {
	for (const [label, fail] of [
		['offline mid-install', () => Promise.reject(new Error('network down'))],
		['a server error', () => Promise.resolve(response(503, 'text/plain'))],
		['a missing asset redirected to the page', () => Promise.resolve(response(200, 'text/html', { redirected: true }))],
		['an asset answered with the page', () => Promise.resolve(response(200, 'text/html'))],
	]) {
		const storage = fakeCaches();
		await oldCopy(storage);
		const { handlers } = loadWorker(async (address) => (address === '/bridge.js' ? fail() : served(address)), storage);
		await assert.rejects(lifecycle(handlers, 'install'), undefined, label);
		assert.ok(storage.named.has(OLD), `${label}: the installed copy stays`);
		assert.equal(storage.named.has(NEW), false, `${label}: the partial new copy goes`);
		assert.ok(await storage.named.get(OLD).match('/'), `${label}: its start page stays`);
	}
});

test('a page address answered with something other than the page fails the install too', async () => {
	const storage = fakeCaches();
	const { handlers } = loadWorker(async (address) => (address === '/app/status' ? response(200, 'text/javascript') : served(address)), storage);
	await assert.rejects(lifecycle(handlers, 'install'), /incomplete: \/app\/status/);
});

test('update, close the last client, then an offline launch: the new copy answers (review M1, Sol)', async () => {
	const storage = fakeCaches();
	await oldCopy(storage);
	// online: the new build's worker installs (the old one still serves this launch)
	const { handlers, claimed } = loadWorker(async (address) => served(address), storage);
	await lifecycle(handlers, 'install');
	// the last client closes; the next launch activates the new worker
	await lifecycle(handlers, 'activate');
	assert.deepEqual([...storage.named.keys()], [NEW], 'the older copy goes once the new one is whole');
	assert.equal(claimed.count, 1);
	// ... and that launch is offline
	const offline = loadWorker(async () => {
		throw new Error('offline');
	}, storage).handlers;
	for (const path of ['/', '/app/accounts', '/app/scoreboard', '/analytics']) {
		const page = await navigate(offline, path);
		assert.equal(page.status, 200, `${path} opens the app offline`);
		assert.match(page.headers.get('content-type'), /text\/html/);
	}
});

test('activation keeps the older copy while its own copy is incomplete', async () => {
	const storage = fakeCaches();
	await oldCopy(storage);
	const { handlers, claimed } = loadWorker(async (address) => served(address), storage);
	await lifecycle(handlers, 'install');
	// the browser dropped part of the new copy between install and activation
	storage.named.get(NEW).store.delete(`${ORIGIN}/pkg/0123456789ab/aac_web_bg.wasm`);
	await lifecycle(handlers, 'activate');
	assert.ok(storage.named.has(OLD), 'the last usable copy stays');
	assert.equal(claimed.count, 1, 'the new worker still takes charge');
	const offline = loadWorker(async () => {
		throw new Error('offline');
	}, storage).handlers;
	assert.equal((await navigate(offline, '/')).status, 200, 'an offline launch still finds a start page');
});

test('an unstamped worker (a fixture or dev server) keeps the previous behaviour', async () => {
	const storage = fakeCaches();
	await oldCopy(storage);
	const asked = [];
	const { handlers } = loadWorker(async (address) => {
		asked.push(address);
		return served(address);
	}, storage, null);
	await lifecycle(handlers, 'install');
	assert.deepEqual(asked, [], 'nothing to download');
	await lifecycle(handlers, 'activate');
	assert.equal(storage.named.has(OLD), false);
});
