// The frame kit must enforce the behaviours it names (review L4): the U1 sheet check must fail
// when there is no pinned thread, not pass silently, and the L1 Details-beside-a-left-rail rule
// needs a rect check. Guards the kits statically (browsers are not run tonight).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dir = path.dirname(fileURLToPath(import.meta.url));
const frame = fs.readFileSync(path.join(dir, 'browser/checks/frame.check.mjs'), 'utf8');
const accounts = fs.readFileSync(path.join(dir, 'browser/checks/accounts-frame.check.mjs'), 'utf8');

test('the U1 sheet check fails without a pinned thread', () => {
	assert.equal(frame.includes("if (await t.page.$('.pin-row'))"), false, 'the sheet check must not skip silently');
	assert.match(frame, /t\.check\([^)]*pinned thread to open/, 'absence of a pinned row fails the check');
});

test('Details beside a left rail has a rect check', () => {
	assert.match(accounts, /Details reaches the right edge beside a left rail/, 'accounts-frame checks Details on the strip-left pose');
	assert.match(accounts, /\.details/, 'the check measures the Details panel');
	assert.match(accounts, /0,0,34,84/, 'the strip-left insets');
});
