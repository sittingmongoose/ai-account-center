// A pin's refusal is drawn on every branch of its card and sheet that draws the pin's body, so a standalone
// refusal on a fresh pin is never invisible on a wide desktop (wave-2 landing review M1).
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const src = fs.readFileSync(path.resolve(here, '../leptos/src/status.rs'), 'utf8');

function body(name) {
	const start = src.indexOf(`fn ${name}(`);
	assert.ok(start >= 0, `fn ${name} exists`);
	const next = src.indexOf('\n#[component]', start + 1);
	return src.slice(start, next < 0 ? undefined : next);
}

for (const name of ['PinCard', 'PinSheet']) {
	test(`${name}: every branch that draws the pin body also draws its refusal notice`, () => {
		const fn = body(name);
		const bodies = (fn.match(/pin_body\(&update/g) || []).length;
		const notices = (fn.match(/refusal_notice\(&update\)/g) || []).length;
		assert.ok(bodies >= 2, `${name} draws the pin body on its cached and fresh branches`);
		assert.equal(notices, bodies, `${name}: ${notices} refusal notices for ${bodies} pin bodies`);
	});
}
