// The build check must not reload over a focused iframe or an open confirmation (review L5):
// typing() counts each as busy. Guards the wiring statically (the kit would need a real iframe
// to focus, which is not run tonight).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dir = path.dirname(fileURLToPath(import.meta.url));
const src = fs.readFileSync(path.join(dir, '../leptos/src/ui/device.rs'), 'utf8');

test('typing counts a focused iframe and open confirmations as busy', () => {
	assert.match(src, /busy_for_reload/, 'typing delegates to busy_for_reload');
	assert.doesNotMatch(src.split('#[cfg(test)]')[0], /slint-host/, 'nothing waits on the retired Slint dashboard');
	assert.match(src, /\.adialog/, 'the frame switch dialog counts');
	assert.match(src, /iframe/, 'a focused iframe counts');
});
