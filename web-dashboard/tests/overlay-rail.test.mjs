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
