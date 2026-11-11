// A phone sheet's close button stays reachable while the sheet scrolls: it shares the grabber's
// sticky strip instead of being positioned inside the scrolling sheet (pkg P recheck N2; Jared,
// 2026-11-11: a Status sheet could not be closed on the Duo).
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const leptos = path.resolve(here, '../leptos');
const css = fs.readFileSync(path.join(leptos, 'style/app.css'), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');

function rule(selector) {
	const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	const m = css.match(new RegExp(`(^|\\n)${escaped}\\s*\\{([^}]*)\\}`));
	return m ? m[2] : null;
}

for (const file of ['src/status.rs', 'src/scoreboard.rs']) {
	test(`${file}: the sheet's close button sits in the sticky top strip with the grabber`, () => {
		const src = fs.readFileSync(path.join(leptos, file), 'utf8');
		const strip = src.match(/<div class="sheet-top">([\s\S]*?)<\/div>/);
		assert.ok(strip, 'the sheet has a .sheet-top strip');
		assert.match(strip[1], /<SheetGrab\b/, 'the grabber is in the strip');
		assert.match(strip[1], /class="[^"]*\bsheet-close\b/, 'the close button is in the strip');
		const outside = src.replace(strip[0], '');
		assert.doesNotMatch(outside, /<SheetGrab\b/, 'no second grabber outside the strip');
	});
}

test('the top strip sticks to the top of the scrolling sheet and is opaque', () => {
	const strip = rule('.sheet-top');
	assert.ok(strip, '.sheet-top has a rule');
	assert.match(strip, /position:\s*sticky/);
	assert.match(strip, /top:\s*-?\d/);
	assert.match(strip, /background:\s*var\(--paper\)/);
	const grab = rule('.grabber-zone');
	assert.ok(grab, '.grabber-zone has a rule');
	assert.doesNotMatch(grab, /position:\s*sticky/, 'only the strip sticks, so the button and grabber move together');
});
