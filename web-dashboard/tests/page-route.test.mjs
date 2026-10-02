import test from 'node:test';
import assert from 'node:assert/strict';
import { PAGES, pageFromUrl, pagePath } from '../public/page-route.mjs';

test('the server page routes open their pages, and ?view= stays an alias', () => {
  assert.deepEqual(PAGES, ['home', 'analytics', 'accounts']);
  assert.equal(pageFromUrl('/', ''), 'home');
  assert.equal(pageFromUrl('/login', ''), 'home');
  assert.equal(pageFromUrl('/analytics', ''), 'analytics');
  // /settings is a 301 to /accounts on the server; /accounts/<provider> serves the page too
  assert.equal(pageFromUrl('/accounts', ''), 'accounts');
  assert.equal(pageFromUrl('/accounts/kimi-code', ''), 'accounts');
  assert.equal(pageFromUrl('/accounts/../x', ''), 'home');
  assert.equal(pageFromUrl('/', '?view=accounts'), 'accounts');
  assert.equal(pageFromUrl('/', '?view=analytics&motion'), 'analytics');
  assert.equal(pageFromUrl('/analytics', '?view=nope'), 'analytics');
  assert.equal(pageFromUrl('/nope', '?view=nope'), 'home');
  assert.equal(pageFromUrl(undefined, undefined), 'home');
});

test('navigation pushes the path form', () => {
  assert.equal(pagePath('home'), '/');
  assert.equal(pagePath('analytics'), '/analytics');
  assert.equal(pagePath('accounts'), '/accounts');
  assert.equal(pagePath('other'), '/');
  for (const page of PAGES) assert.equal(pageFromUrl(pagePath(page), ''), page);
});
