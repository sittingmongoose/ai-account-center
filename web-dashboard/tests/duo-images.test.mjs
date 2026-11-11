// The image kits must report the Duo's screen for Duo poses (review L3): without it the frame
// draws phone chrome for a Duo shot. Guards the kits statically (the browser kit needs browsers,
// which are not run tonight): every `duo-*` format below is marked `duo: true` and the context
// spreads the Duo screen only for those.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dir = path.dirname(fileURLToPath(import.meta.url));
const KITS = ['home-images/images.check.mjs', 'accounts-images/images.check.mjs', 'analytics-images/images.check.mjs', 'frame-images/images.check.mjs'];

test('every Duo image pose reports the Duo screen', () => {
	for (const kit of KITS) {
		const src = fs.readFileSync(path.join(dir, 'browser', kit), 'utf8');
		assert.match(src, /DUO_SCREEN\s*=\s*\{\s*width:\s*466,\s*height:\s*678\s*\}/, `${kit}: DUO_SCREEN 466x678`);
		assert.match(src, /\.\.\.\(f\.duo\s*\?\s*\{\s*screen:\s*DUO_SCREEN\s*\}\s*:\s*\{\}\)/, `${kit}: screen spreads for duo formats only`);
		// Every duo-* format is marked duo: true (count via names).
		const duoNames = [...src.matchAll(/name:\s*'(duo-[a-z0-9-]+)'/g)].map((m) => m[1]);
		assert.ok(duoNames.length >= 5, `${kit}: duo formats ${duoNames.join(', ')}`);
		const marked = [...src.matchAll(/name:\s*'duo-[a-z0-9-]+'[^}]*duo:\s*true/g)];
		assert.equal(marked.length, duoNames.length, `${kit}: every duo format marked duo: true`);
	}
});

test('the non-REAL pose is gone', () => {
	for (const kit of KITS) {
		const src = fs.readFileSync(path.join(dir, 'browser', kit), 'utf8');
		assert.equal(src.includes('duo-folded-rotated'), false, `${kit}: duo-folded-rotated dropped`);
		assert.equal(src.includes('0,84,21,0'), false, `${kit}: 0,84,21,0 pose dropped`);
	}
});
