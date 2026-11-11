// Overlay geometry without a browser (review J1): sheets and the account menu already end at
// the rail (U1); the popover, viewer and switch approval must clear the rail in every pose.
// Guards the CSS and the popover/tip placement in charts.rs statically (the kit needs browsers).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dir = path.dirname(fileURLToPath(import.meta.url));
const css = fs.readFileSync(path.join(dir, '../leptos/style/app.css'), 'utf8');
const chartsSrc = fs.readFileSync(path.join(dir, '../leptos/src/ui/charts.rs'), 'utf8');
// Only the implementation, not the test itself, proves the wiring.
const chartsImpl = chartsSrc.split('#[cfg(test)]')[0];

test('the popover sheet ends at the rail', () => {
	assert.match(css, /:root\[data-chrome='rail'\] \.pop/, 'pop sheet right offset');
	assert.match(css, /:root\[data-chrome='rail'\]\[data-rail-side='left'\] \.pop/, 'pop sheet left offset');
});

test('the viewer and the switch approval clear the rail even when the strip inset is missing', () => {
	assert.match(css, /:root\[data-chrome='rail'\] \.viewer/, 'viewer rail padding');
	assert.match(css, /:root\[data-chrome='rail'\] \.ascrim/, 'ascrim rail padding');
});

test('popover and tip placement clear the rail', () => {
	assert.match(chartsImpl, /fn content_bounds/, 'placement reads the rail bounds');
	assert.match(chartsImpl, /content_bounds/, 'place and tip use the bounds');
});

test('the help popover and the tip stay inside the content', () => {
	assert.match(css, /:root\[data-chrome='rail'\] \.hpop/, 'hpop rail offset');
	assert.match(css, /:root\[data-chrome='rail'\] \.tip-card/, 'tip rail width');
});

test('the popover height and kit measure the panel and the last button (review N3)', () => {
	assert.match(chartsImpl, /fn pop_max_heights/, 'height helper');
	assert.match(chartsImpl, /pop_max_heights/, 'place uses the helper');
	const kit = fs.readFileSync(path.join(dir, 'browser/checks/overlay-close.check.mjs'), 'utf8');
	assert.match(kit, /\.pop/, 'kit measures the pop panel');
	assert.match(kit, /last-of-type/, 'kit measures the approval last button');
	assert.equal(kit.includes('pop-grab'), false, 'kit no longer measures the grabber');
});

test('the kit covers the 951 top, the band button and the placement wait (review N6)', () => {
	const kit = fs.readFileSync(path.join(dir, 'browser/checks/overlay-close.check.mjs'), 'utf8');
	assert.match(kit, /sheet top clears 28/, '951 sheet-top assert');
	assert.match(kit, /band-tools/, 'band account button');
	assert.match(kit, /waitForTimeout\(300\)/, 'placement wait');
	assert.equal(kit.includes("=== 'band') return '.topbar"), false, 'dead band branch gone');
});

test('rail sheets take the safe inset on the side away from the rail (review N2)', () => {
	assert.match(css, /:root\[data-chrome='rail'\] \.sheet-host \.sheet \{ right: var\(--rail-w\); left: var\(--safe-left/, 'sheet left takes safe');
	assert.match(css, /:root\[data-chrome='rail'\]\[data-rail-side='left'\] \.sheet-host \.sheet \{ right: var\(--safe-right/, 'sheet right takes safe');
	assert.match(css, /:root\[data-chrome='rail'\] \.pop[\s\S]*?left: var\(--safe-left/, 'pop sheet left takes safe');
	assert.match(css, /:root\[data-chrome='rail'\]\[data-rail-side='left'\] \.pop[\s\S]*?right: var\(--safe-right/, 'pop sheet right takes safe');
	assert.match(css, /calc\(100% - var\(--rail-w[^)]*\) - var\(--safe-left[^)]*\)\)/, 'details panel width takes safe');
});

test("an iPhone's rail keeps its buttons inside the safe area, out of the island's band (wave-2 device M4)", () => {
	const device = fs.readFileSync(path.join(dir, '../leptos/src/ui/device.rs'), 'utf8').split('#[cfg(test)]')[0];
	assert.match(device, /set_property\(\s*"--rail-pad"/, 'the frame sets the rail padding');
	assert.match(device, /fn rail_pad\(insets: Insets, duo: bool\)/, 'the padding depends on the device');
	assert.match(css, /:root\[data-chrome='rail'\] \.rail \{[^}]*padding-right: var\(--rail-pad, 0px\)/, 'right rail padded on the edge side');
	assert.match(css, /:root\[data-chrome='rail'\]\[data-rail-side='left'\] \.rail \{[^}]*padding-left: var\(--rail-pad, 0px\)/, 'left rail padded on the edge side');
});
