// A pin's refusal is drawn on every branch of its card and sheet that draws the pin's body, so a standalone
// refusal on a fresh pin is never invisible on a wide desktop (wave-2 landing review M1).
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const src = fs.readFileSync(path.resolve(here, '../leptos/src/pins.rs'), 'utf8').split('#[cfg(test)]')[0];

// Concept C v2: the wide pane and the phone sheet both draw one sheet_body, which draws the refusal notice before
// any branch (quiet, cached or fresh), so no branch can draw the thread without it.
test('the thread sheet draws its refusal notice before every branch of its body', () => {
	const start = src.indexOf('fn sheet_body(');
	assert.ok(start >= 0, 'fn sheet_body exists');
	const fn = src.slice(start, src.indexOf('\n// ----', start));
	const notice = fn.indexOf('{refusal_notice(&p.u)}');
	const body = fn.lastIndexOf('{body}');
	assert.ok(notice >= 0, 'sheet_body draws refusal_notice');
	assert.ok(body > notice, 'the notice comes before the body it belongs to');
	assert.match(fn, /cached_notice\(&p\.u\)/, 'a cached copy says so');
});

test('the pane and the phone sheet both draw sheet_body', () => {
	const uses = (src.match(/sheet_body\(&p,/g) || []).length;
	assert.equal(uses, 2, 'one use for the wide pane, one for the phone sheet');
});
