// The build check (frame.rs watch_build) fetches its own page with cache: 'no-store' and
// compares the served build id. The service worker must never answer that check from the
// cache, or an offline / 5xx server reads as a new version (review M1). Loading the real
// sw.js against fake fetch/caches: the kit blocks service workers, so nothing else runs it.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const publicDir = new URL('../public/', import.meta.url);
const routeSource = fs.readFileSync(new URL('sw-route.js', publicDir), 'utf8');
const swSource = fs
	.readFileSync(new URL('sw.js', publicDir), 'utf8')
	.replace('__AAC_BUILD_ID__', '0123456789ab');

class FakeCache {
	constructor() {
		this.store = new Map();
	}

	async put(req, res) {
		this.store.set(req.url || req, res);
	}

	async match(req, opts = {}) {
		const key = req.url || req;
		if (opts.ignoreSearch) {
			const u = new URL(key);
			for (const [k, v] of this.store) {
				const ku = new URL(k);
				if (ku.origin === u.origin && ku.pathname === u.pathname) return v;
			}
			return undefined;
		}
		if (this.store.has(key)) return this.store.get(key);
		return undefined;
	}

	async keys() {
		return [...this.store.keys()].map((url) => ({ url, method: 'GET' }));
	}
}

function fakeResponse(status, { type = 'basic', contentType = 'text/html', cacheControl = null } = {}) {
	const headers = new Headers();
	if (contentType) headers.set('content-type', contentType);
	if (cacheControl) headers.set('cache-control', cacheControl);
	return { status, type, headers, clone() { return this; } };
}

function loadWorker(fetchImpl, cache = new FakeCache()) {
	const handlers = {};
	const sandbox = {
		URL,
		URLSearchParams,
		Headers,
		Response,
		Promise,
		Error,
		setTimeout,
		clearTimeout,
		fetch: fetchImpl,
		caches: {
			open: async () => cache,
			match: async (req, opts) => cache.match(req, opts),
			keys: async () => ['aac-shell-0123456789ab'],
			delete: async () => true,
		},
		clients: { claim: async () => {} },
	};
	sandbox.self = sandbox;
	sandbox.location = { origin: 'http://test' };
	sandbox.addEventListener = (type, cb) => {
		handlers[type] = cb;
	};
	const ctx = vm.createContext(sandbox);
	sandbox.importScripts = () => {
		vm.runInContext(routeSource, ctx);
	};
	vm.runInContext(swSource, ctx);
	return { handlers, cache };
}

const get = (path, { mode = 'cors', cache } = {}) => {
	const r = { url: `http://test${path}`, method: 'GET', mode };
	if (cache !== undefined) r.cache = cache;
	return r;
};

async function respond(handlers, request) {
	const event = { request, waitUntil() {}, promise: null, respondWith(p) { this.promise = p; } };
	handlers.fetch(event);
	return event.promise;
}

test('a no-store build check never gets a cached answer, on a 5xx or offline', async () => {
	const cached = fakeResponse(200);
	// 5xx with a cached page: the check sees the 502, not the cache.
	{
		const { handlers, cache } = loadWorker(async () => fakeResponse(502));
		await cache.put(get('/app/home'), cached);
		const got = await respond(handlers, get('/app/home', { cache: 'no-store' }));
		assert.equal(got.status, 502, 'no-store on a 5xx must not answer from the cache');
		assert.notEqual(got, cached, 'the cached page must not read as the server answer');
	}
	// Offline with a cached page: the check fails, not the cache.
	{
		const { handlers, cache } = loadWorker(async () => {
			throw new Error('network down');
		});
		await cache.put(get('/app/home'), cached);
		await assert.rejects(() => respond(handlers, get('/app/home', { cache: 'no-store' })), /network down|could not answer|not cached/);
	}
});

test('a plain request still falls back to the cache (the contrast)', async () => {
	const cached = fakeResponse(200);
	{
		const { handlers, cache } = loadWorker(async () => fakeResponse(500));
		await cache.put(get('/app/home'), cached);
		const got = await respond(handlers, get('/app/home'));
		assert.equal(got, cached, 'without no-store the cache still answers');
	}
	{
		const { handlers, cache } = loadWorker(async () => {
			throw new Error('network down');
		});
		await cache.put(get('/app/home'), cached);
		const got = await respond(handlers, get('/app/home'));
		assert.equal(got, cached, 'without no-store the cache still answers offline');
	}
});

test('the warmed runtime caches cache-first, so a cached page keeps its runtime', async () => {
	// watch_build warms /pkg/<served>/aac_web.js and aac_web_bg.wasm with fetch; cacheFirst
	// stores them, so the cached HTML can start offline (review M1-b).
	const { handlers, cache } = loadWorker(async () => fakeResponse(200, { contentType: 'text/javascript' }));
	const req = get('/pkg/deadbeefcafe/aac_web.js');
	const first = await respond(handlers, req);
	assert.equal(first.status, 200);
	assert.equal(cache.store.size, 1, 'the runtime fetch is stored');
	const cached = await cache.match(req);
	assert.equal(cached, first, 'a later load reads the warmed runtime from the cache');
});
