// Account switching was removed (2026-11-13), so no product text may still send the user to
// "sign in to another account first" or "switch first", and the live login signs in again in place.
// Scans the shipped sources, help text and current docs (not tests, not the CHANGELOG history).
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('../../', import.meta.url)));
const TEXT = /\.(ts|tsx|js|mjs|cjs|rs|py|md|swift|cs|xaml|html|css|json|txt)$/;
const SKIP_DIR = new Set(['node_modules', 'target', 'dist', '.git', 'tests', 'Tests', 'fixtures', '__pycache__', 'bin', 'obj']);
const isTest = (name) => /(\.test\.|_test\.py$|\.spec\.)/.test(name);

function walk(dir, out = []) {
  let entries;
  try { entries = readdirSync(dir, { withFileTypes: true }); } catch { return out; }
  for (const entry of entries) {
    if (entry.name.startsWith('.')) continue;
    const path = join(dir, entry.name);
    if (entry.isDirectory()) { if (!SKIP_DIR.has(entry.name)) walk(path, out); }
    else if (TEXT.test(entry.name) && !isTest(entry.name)) out.push(path);
  }
  return out;
}
function files(...paths) {
  return paths.flatMap((p) => {
    const full = join(root, p);
    try { return statSync(full).isDirectory() ? walk(full) : [full]; } catch { return []; }
  });
}
/** A file's path under the repo with forward slashes, the shape the checks below name (Windows returns `\`). */
const rel = (file) => relative(root, file).split('\\').join('/');
function hits(list, pattern) {
  const found = [];
  for (const file of list) {
    const lines = readFileSync(file, 'utf8').split('\n');
    lines.forEach((line, index) => { if (pattern.test(line)) found.push(`${rel(file)}:${index + 1}: ${line.trim().slice(0, 140)}`); });
  }
  return found;
}

const PRODUCT = files('src', 'scripts', 'crates', 'native', 'web-dashboard/public', 'web-dashboard/leptos/src',
  'macos-bar/Sources', 'windows-bar', 'browser-bridge', 'docs', 'README.md', 'CONTRIBUTING.md');
const PAGE = files('web-dashboard/public', 'web-dashboard/leptos/src');

test('the scan reads the product sources it guards', () => {
  const names = PRODUCT.map(rel);
  for (const expected of ['src/antigravity/terminal-signin.ts', 'web-dashboard/public/accounts-view.mjs',
    'web-dashboard/leptos/src/dash.rs', 'docs/antigravity-runtime.md', 'src/commands/antigravity-command.ts'])
    assert.ok(names.includes(expected), expected);
});

test('no "sign in to another account first" or "switch first" advice is left', () => {
  const pattern = new RegExp([
    String.raw`\bsign\b[^.\n]{0,40}?\bin to another (account|profile)\b[^.\n]*\bfirst\b`,
    String.raw`\bswitch(ing)? to another (account|profile)\b`,
    String.raw`\banother (account|profile) first\b`,
    String.raw`\bswitch(ing)? first\b`,
    String.raw`\bcannot sign in again\b`,
    String.raw`\brefused for the live login\b`,
    String.raw`\baccount_active_signin\b`,
    String.raw`Automatic switching status`,
  ].join('|'), 'i');
  assert.deepEqual(hits(PRODUCT, pattern), []);
});

test('the page and the Leptos frame describe no switch approval, dialog or review', () => {
  assert.deepEqual(hits(PAGE, /switch approval|pending-switch|switch dialog|switch confirmation|switching review|account switched/i), []);
});
