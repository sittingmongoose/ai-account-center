/**
 * The shared normalizeModel case table (DESIGN-M3-COMPLETE 1.4): the real TypeScript
 * lookup over the packaged catalog and alias table, pinned for the Rust port
 * (crates/aac-status/src/model_identity.rs, checked by tests/model_identity.rs).
 *
 *   node tests/spec/model-identity-vectors.mjs [--check]
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { normalizeModel } from '../../src/web-server/scoreboard/model-identity.ts';

const root = fileURLToPath(new URL('../..', import.meta.url));
const out = path.join(root, 'tests/spec/model-identity.json');
const catalog = JSON.parse(
  fs.readFileSync(path.join(root, 'docs/agent-reports/catalog/models.json'), 'utf8')
);
const aliasFile = JSON.parse(
  fs.readFileSync(path.join(root, 'docs/agent-reports/catalog/model-aliases.json'), 'utf8')
);
const models = catalog.models.map(({ model, efforts }) => ({ model, efforts: efforts ?? [] }));
const table = { aliases: aliasFile.aliases, effortSuffixes: aliasFile.effortSuffixes };

const cases = [];
const add = (id, effort, useTable) => {
  const want = normalizeModel(id, effort, models, useTable ? table : null);
  cases.push({ id, effort, useTable, want });
};

const suffixes = [...new Set([...table.effortSuffixes, ...models.flatMap((m) => m.efforts.map((e) => `-${e}`))])];
for (const { model } of models) {
  const bare = model.includes('/') ? model.slice(model.indexOf('/') + 1) : model;
  for (const effort of [null, 'max', 'high', '']) {
    add(model, effort, true);
    add(model, effort, false);
    add(bare, effort, true);
    add(bare, effort, false);
    for (const suffix of suffixes) {
      add(`${bare}${suffix}`, effort, true);
      add(`${model}${suffix}`, effort, true);
    }
  }
  add(`${model}-unknownsuffix`, null, true);
  add(`${bare}-unknownsuffix`, 'max', true);
}
for (const written of Object.keys(table.aliases)) {
  add(written, null, true);
  add(written, 'max', true);
  add(written, null, false);
  add(`${written}-high`, null, true);
  // An alias whose target is not listed resolves nothing.
  cases.push({
    id: written,
    effort: null,
    useTable: true,
    want: normalizeModel(written, null, models, { aliases: { [written]: 'unlisted/model' }, effortSuffixes: [] }),
    overrideAliases: { [written]: 'unlisted/model' },
  });
}
for (const id of [
  'future-model',
  'future-model-high',
  'gpt-6.1-sol-max',
  'openai/gpt-6.1-sol-max',
  'provider\\model',
  'builtin:plan\\Model',
  '',
  'a',
  'gemini-4-argon-high-max',
  'claude-opus-5-5-high',
]) {
  add(id, null, true);
  add(id, 'max', true);
  add(id, null, false);
}

const corpus = { version: 1, models, aliases: table.aliases, effortSuffixes: table.effortSuffixes, cases };
if (process.argv.includes('--check')) {
  assert.deepEqual(JSON.parse(fs.readFileSync(out, 'utf8')), JSON.parse(JSON.stringify(corpus)));
  console.log(`model-identity vectors: ${cases.length} cases match`);
} else {
  fs.writeFileSync(out, `${JSON.stringify(corpus, null, 2)}\n`);
  console.log(`wrote ${out} (${cases.length} cases)`);
}
