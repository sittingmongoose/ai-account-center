// /bridge.js after the classic app's retirement (review M1, Astra): a cached classic page that loads it moves to the app
// frame, at the tab its address named, with the rest of its query, and forgets the classic choice. The file runs as
// written (only `export` dropped) in a fake page, so its own top-level step is what is tested.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { addressCases } from './fixtures/address-cases.mjs';

const SOURCE = fs.readFileSync(new URL('../public/bridge.js', import.meta.url), 'utf8');

/** Runs the module in a page at `href` with `stored` localStorage; returns what it did and its functions. */
function runIn(href, stored = { 'aac-frame': 'classic' }, { storageThrows = false } = {}) {
	const url = new URL(href, 'http://dashboard.test');
	const replaced = [];
	const store = new Map(Object.entries(stored));
	const localStorage = {
		removeItem(k) {
			if (storageThrows) throw new Error('storage refused');
			store.delete(k);
		},
	};
	const window = { location: { pathname: url.pathname, search: url.search, replace: (to) => replaced.push(to) }, localStorage };
	const ctx = vm.createContext({ window, URLSearchParams, decodeURIComponent });
	vm.runInContext(`${SOURCE.replace(/^export /gm, '')}\n;({ frameAddress, leaveClassic })`, ctx);
	const api = vm.runInContext('({ frameAddress, leaveClassic })', ctx);
	return { replaced, store, api };
}

test('a cached classic page moves to the frame and forgets the classic choice', () => {
	const { replaced, store } = runIn('/');
	assert.deepEqual(replaced, ['/app/home']);
	assert.equal(store.has('aac-frame'), false);
	// the classic page's own redirect then reads the default ("new") if it ever runs again
	const other = runIn('/accounts/claude?e2e&view=accounts&safe=82,0,34,0', { 'aac-frame': 'classic', 'aac-theme': 'dark' });
	assert.deepEqual(other.replaced, ['/app/accounts?e2e&safe=82,0,34,0']);
	assert.equal(other.store.get('aac-theme'), 'dark', 'nothing else is touched');
});

test('it moves on even when storage refuses', () => {
	const { replaced } = runIn('/analytics', {}, { storageThrows: true });
	assert.deepEqual(replaced, ['/app/analytics']);
});

test('every address opens the tab the frame opens for it (lib.rs, case for case, encoded aliases included)', () => {
	const { api } = runIn('/');
	for (const [path, search, tab] of addressCases())
		assert.equal(api.frameAddress(path, search).split('?')[0], `/app/${tab}`, `${path}${search}`);
});

test('the query keeps everything but the classic keys, however those are encoded, as written', () => {
	const { api } = runIn('/');
	for (const [path, search, want] of [
		['/', '', '/app/home'],
		['/', '?view=analytics', '/app/analytics'],
		['/', '?%76iew=analytics&x=1', '/app/analytics?x=1'],
		['/', '?view=%73tatus&cl%61ssic=1&content=1', '/app/status'],
		['/', '?e2e&safe=82%2C0', '/app/home?e2e&safe=82%2C0'],
		['/', '?viewer=1&&', '/app/home?viewer=1'],
		['/', '?%zz=1', '/app/home?%zz=1'],
		['/accounts/codex', '?embed=1', '/app/accounts?embed=1'],
	])
		assert.equal(api.frameAddress(path, search), want, `${path}${search}`);
});

test('it loads nothing: no import of any kind, so the old cache cannot break it', () => {
	assert.doesNotMatch(SOURCE, /^\s*import[\s{*('"]/m);
	assert.doesNotMatch(SOURCE, /\bimport\s*\(/);
	assert.ok(SOURCE.length < 8 * 1024);
});
