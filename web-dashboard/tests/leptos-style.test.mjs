// The Leptos pages' stylesheet: its colours match the Slint theme, and the shapes the redesign removed stay gone
// (DESIGN-SYSTEM.md section 1: no pills, chips, dots, side bars or tinted boxes).
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { TOKENS, THEME_SLINT, APP_CSS, BEGIN, END, readTheme, renderTokens } from '../leptos/build/theme-tokens.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const cssSource = fs.readFileSync(APP_CSS, 'utf8');
const theme = readTheme(fs.readFileSync(THEME_SLINT, 'utf8'));
const leptosSrc = path.resolve(here, '../leptos/src');

function stripComments(css) {
	return css.replace(/\/\*[\s\S]*?\*\//g, '');
}
const css = stripComments(cssSource);

function rustFiles(dir) {
	return fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
		const p = path.join(dir, e.name);
		return e.isDirectory() ? rustFiles(p) : e.name.endsWith('.rs') ? [p] : [];
	});
}

test('every token has a theme.slint colour', () => {
	for (const [token, name] of Object.entries(TOKENS)) assert.ok(theme.has(name), `--${token} maps to ${name}, which is not in theme.slint`);
});

test('the generated token block is what theme.slint produces', () => {
	const expected = renderTokens(fs.readFileSync(THEME_SLINT, 'utf8'));
	const from = cssSource.indexOf(BEGIN);
	const to = cssSource.indexOf(END);
	assert.ok(from >= 0 && to > from, 'style/app.css has no GENERATED TOKENS markers');
	const actual = cssSource.slice(from, to + END.length);
	assert.equal(actual, expected, 'style/app.css has drifted from theme.slint: run node web-dashboard/leptos/build/theme-tokens.mjs');
});

test('each theme colour is set for light and for dark', () => {
	const block = cssSource.slice(cssSource.indexOf(BEGIN), cssSource.indexOf(END));
	const [light, ...darkParts] = block.split(/@media|:root\[data-theme='dark'\]/);
	for (const [token, name] of Object.entries(TOKENS)) {
		const { light: l, dark: d } = theme.get(name);
		assert.ok(light.includes(`--${token}: ${l};`), `light --${token} is not ${l}`);
		for (const part of darkParts) assert.ok(part.includes(`--${token}: ${d};`), `dark --${token} is not ${d}`);
	}
});

// ---- Banned shapes (DESIGN-SYSTEM.md section 1) ----------------------------------------------------------------

test('no pill radii', () => {
	assert.equal(/999px|9999px/.test(css), false, 'a 999px radius makes a pill');
});

test('no chip or dot classes', () => {
	const banned = /(^|[^\w-])\.(chip|chip-btn|dot|state-dot|src-dot|badge|pill)(?![\w-])/gm;
	const hits = css.match(banned) ?? [];
	assert.deepEqual(hits, [], 'chips and status dots came back');
});

test('no chip or dot markup in the pages', () => {
	for (const file of rustFiles(leptosSrc)) {
		const text = fs.readFileSync(file, 'utf8');
		for (const m of text.matchAll(/class="([^"]*)"/g)) {
			for (const cls of m[1].split(/\s+/)) {
				assert.equal(['chip', 'chip-btn', 'dot', 'state-dot', 'badge', 'pill'].includes(cls), false, `${path.basename(file)} uses class "${cls}"`);
			}
		}
	}
});

test('no side bars in a state colour', () => {
	const rules = [...css.matchAll(/border-left[^:]*:([^;}]+)/g)].map((m) => m[1]);
	for (const value of rules) {
		assert.equal(/--(accent|calm|warn|crit|over|good|h-)/.test(value), false, `border-left in a state colour: ${value.trim()}`);
	}
});

// Every background that uses a state colour outside a meter fill, each judged by the rule it is written in.
function tintedBackgrounds(source) {
	const hits = [];
	for (const m of source.matchAll(/background(?:-color)?:([^;}]+)/g)) {
		if (!/--(warn|crit|calm|good)/.test(m[1])) continue;
		const start = source.lastIndexOf('}', m.index);
		const selector = source.slice(start + 1, source.indexOf('{', start)).trim();
		if (!/\.fill/.test(selector)) hits.push(selector);
	}
	return hits;
}

test('no tinted state backgrounds', () => {
	assert.equal(/--(warn|crit|good|calm)-soft/.test(css), false, 'a --warn-soft style token came back');
	assert.deepEqual(tintedBackgrounds(css), [], 'state colour used as a background outside a meter fill');
});

test('a tinted background is caught even when a meter fill uses the same colour', () => {
	assert.deepEqual(tintedBackgrounds(`${css}\n.review-alert { background: var(--warn); }`), ['.review-alert']);
	assert.deepEqual(tintedBackgrounds(`${css}\n.review-alert { background-color: var(--crit); }`), ['.review-alert']);
});

// ---- Rules that keep the type right ------------------------------------------------------------------------------

test('every font-weight also sets the wght axis (iOS WebKit ignores font-weight on these fonts)', () => {
	for (const m of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
		const body = m[2];
		for (const w of body.matchAll(/font-weight:\s*(\d+)/g)) {
			if (body.includes('@font-face')) continue;
			if (/@font-face/.test(m[1])) continue;
			assert.ok(new RegExp(`font-variation-settings:\\s*'wght' ${w[1]}\\b`).test(body), `rule "${m[1].trim().slice(0, 60)}" sets font-weight ${w[1]} without its wght`);
		}
	}
});

test('the compact sizes below desktop are the approved ones', () => {
	assert.match(css, /--fs-body: 12\.5px; --fs-small: 11\.5px; --fs-title: 14\.5px; --fs-page: 19px; --fs-tiny: 11px/);
	assert.match(css, /--fs-body: 13\.5px; --fs-small: 12px; --fs-title: 17px; --fs-page: 28px/);
});

test('the iframe gets no rail or band space', () => {
	// data-chrome="none" reserves nothing: the shell's own chrome already sits outside the content.
	assert.match(css, /data-chrome='none'\] \.shell \{ grid-template-rows: minmax\(0, 1fr\); background: transparent; \}/);
	const rail = css.indexOf("[data-chrome='rail'] .content");
	assert.ok(rail > 0, 'the bottom inset applies only to the rail and band layouts');
});
