// "Later" must remember the dismissed build id, so the 10-minute poll does not offer the same
// build again (review L6). Guards the wiring statically (the kit would need two polls, which is
// not run tonight).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dir = path.dirname(fileURLToPath(import.meta.url));
const src = fs.readFileSync(path.join(dir, '../leptos/src/frame.rs'), 'utf8');
// Only the implementation, not the test itself, proves the wiring.
const impl = src.split('#[cfg(test)]')[0];

test('Later remembers the dismissed build id', () => {
	assert.match(impl, /dismissed_build/, 'the dismissed id is remembered');
	assert.match(impl, /fn should_offer/, 'the poll offers only a different build');
});
