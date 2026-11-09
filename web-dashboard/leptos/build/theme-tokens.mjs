// The Leptos pages' colour tokens, generated from the Slint theme so the two stay the same.
// Writes the block between the GENERATED TOKENS markers in style/app.css. Run after theme.slint changes:
//   node web-dashboard/leptos/build/theme-tokens.mjs
// web-dashboard/tests/leptos-style.test.mjs fails if the block has drifted from theme.slint.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
export const THEME_SLINT = path.resolve(here, '../../ui/theme.slint');
export const APP_CSS = path.resolve(here, '../style/app.css');
export const BEGIN = '/* BEGIN GENERATED TOKENS (build/theme-tokens.mjs, from web-dashboard/ui/theme.slint) */';
export const END = '/* END GENERATED TOKENS */';

/** CSS token name -> theme.slint property. Every one is a `pick(light, dark)` pair. */
export const TOKENS = Object.freeze({
	bg: 'bg',
	'bg-band': 'bg-band',
	paper: 'paper',
	'paper-2': 'paper-2',
	'paper-3': 'paper-3',
	ink: 'ink',
	'ink-2': 'ink-2',
	'ink-3': 'ink-3',
	'ink-4': 'ink-4',
	rule: 'rule',
	'rule-2': 'rule-2',
	'rule-strong': 'rule-strong',
	'pg-minor': 'paper-grid-minor',
	'pg-major': 'paper-grid-major',
	track: 'track',
	'track-tick': 'track-tick',
	'fill-wash': 'fill-wash',
	'row-hover': 'row-hover',
	'row-sel': 'row-sel',
	accent: 'accent',
	'accent-ink': 'accent-ink',
	'accent-text': 'accent-text',
	'accent-soft': 'accent-soft',
	'nav-bg': 'nav-bg',
	'nav-active': 'nav-active',
	'nav-active-fg': 'nav-active-fg',
	calm: 'calm',
	warn: 'warn',
	crit: 'crit',
	over: 'over',
	'warn-text': 'warn-text',
	'crit-text': 'crit-text',
	'good-text': 'good-text',
	'sheet-scrim': 'sheet-scrim',
	grabber: 'grabber',
	'h-claude': 'h-claude',
	'h-codex': 'h-codex',
});

/** Tokens whose value is geometry around a theme colour. */
const SHADOWS = Object.freeze({ 'shadow-1': ['shadow-1', '0 1px 3px'], 'shadow-pop': ['shadow-pop', '0 16px 40px'] });

/** Every `pick(#light, #dark)` in theme.slint, by property name. */
export function readTheme(source) {
	const pairs = new Map();
	for (const m of source.matchAll(/out property <color> ([\w-]+): pick\((#[0-9A-Fa-f]{6,8}),\s*(#[0-9A-Fa-f]{6,8})\);/g)) {
		pairs.set(m[1], { light: m[2], dark: m[3] });
	}
	return pairs;
}

function pair(pairs, name) {
	const value = pairs.get(name);
	if (!value) throw new Error(`theme.slint has no pick() colour named ${name}`);
	return value;
}

function declarations(pairs, mode) {
	const lines = Object.entries(TOKENS).map(([token, name]) => `--${token}: ${pair(pairs, name)[mode]};`);
	for (const [token, [name, geometry]] of Object.entries(SHADOWS)) lines.push(`--${token}: ${geometry} ${pair(pairs, name)[mode]};`);
	return lines.map((line) => `  ${line}`).join('\n');
}

/** The generated block, markers included. */
export function renderTokens(source) {
	const pairs = readTheme(source);
	const dark = declarations(pairs, 'dark');
	return [
		BEGIN,
		`:root {\n  color-scheme: light;\n${declarations(pairs, 'light')}\n}`,
		`@media (prefers-color-scheme: dark) {\n  :root:not([data-theme='light']) {\n  color-scheme: dark;\n${dark}\n  }\n}`,
		`:root[data-theme='dark'] {\n  color-scheme: dark;\n${dark}\n}`,
		END,
	].join('\n');
}

/** css with its generated block replaced by `block`. */
export function withTokens(css, block) {
	const from = css.indexOf(BEGIN);
	const to = css.indexOf(END);
	if (from < 0 || to < from) throw new Error('style/app.css has no GENERATED TOKENS markers');
	return css.slice(0, from) + block + css.slice(to + END.length);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
	fs.writeFileSync(APP_CSS, withTokens(fs.readFileSync(APP_CSS, 'utf8'), renderTokens(fs.readFileSync(THEME_SLINT, 'utf8'))));
	console.log('style/app.css tokens updated from theme.slint');
}
