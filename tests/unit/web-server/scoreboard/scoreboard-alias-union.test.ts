/**
 * The one alias table (pm7-build's ruling; agent-report 1.6): AAC publishes
 * `catalog/model-aliases.json`, which agent-report lays over its built-in copy. The packaged table
 * is the union of that copy (fixtures/agent-report-1.6-builtin-model-aliases.json, copied unchanged
 * from tools/agent-report/selftest-fixtures/catalog/model-aliases.json at cf74b0f) and AAC's own
 * entries, so AAC's reader resolves every spelling the built-in copy accepts to the same catalog id
 * and refuses none of them. It also maps the 35 tally runs the copy cannot, and keeps Qwen's `-max`.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import {
  CatalogReader,
  packagedCatalogDir,
  seedCatalog,
} from '../../../../src/web-server/scoreboard/catalog';
import { normalizeModel } from '../../../../src/web-server/scoreboard/model-identity';
import { NOW, tempDirs } from '../status/status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

interface AliasFile {
  schema: string;
  aliases: Record<string, string>;
  effortSuffixes: string[];
}
const read = <T>(file: string) => JSON.parse(fs.readFileSync(file, 'utf8')) as T;
const builtin = read<AliasFile>(
  path.join(__dirname, 'fixtures', 'agent-report-1.6-builtin-model-aliases.json')
);
const packaged = read<AliasFile>(path.join(packagedCatalogDir(), 'model-aliases.json'));
const { models } = read<{ models: Array<{ model: string; efforts?: string[] }> }>(
  path.join(packagedCatalogDir(), 'models.json')
);

/** The catalog and alias table as AAC's reader loads them from a share it has just seeded. */
async function publishedTable() {
  const root = dirs.make();
  fs.mkdirSync(path.join(root, 'v1', 'catalog'), { recursive: true });
  await seedCatalog(root);
  const reader = new CatalogReader({ root: () => root, now: () => NOW });
  await reader.scan();
  const state = reader.snapshot();
  expect(state.problems).toEqual([]);
  expect(state.missingFiles).toEqual([]);
  return {
    models: state.models,
    table: { aliases: state.aliases, effortSuffixes: state.effortSuffixes },
  };
}

/** The tally ids (model-id-tally.txt, 2026-11-13) that agent-report's built-in copy cannot map. */
const UNMAPPED_BY_BUILTIN: Array<[string, number]> = [
  ['opencode-go/deepseek-v4.1-flash', 32],
  ['deepseek-v4.1-flash', 2],
  ['alibaba-token-plan/qwen3.8-max', 1],
];

describe('the published alias table', () => {
  it('holds every built-in entry with the same canonical id, and every built-in suffix', () => {
    expect(builtin.schema).toBe(packaged.schema);
    expect(Object.keys(builtin.aliases)).toHaveLength(104);
    for (const [written, canonical] of Object.entries(builtin.aliases))
      expect([written, packaged.aliases[written]]).toEqual([written, canonical]);
    for (const suffix of builtin.effortSuffixes) expect(packaged.effortSuffixes).toContain(suffix);
    // The union (108), plus the explicit Qwen spellings (4).
    expect(Object.keys(packaged.aliases)).toHaveLength(112);
    const known = new Set(models.map((model) => model.model));
    for (const canonical of Object.values(packaged.aliases))
      expect(known.has(canonical)).toBe(true);
  });

  it("resolves every built-in spelling through AAC's reader to the built-in's catalog id", async () => {
    const published = await publishedTable();
    for (const [written, canonical] of Object.entries(builtin.aliases))
      expect([written, normalizeModel(written, null, published.models, published.table)]).toEqual([
        written,
        { id: canonical, effort: null },
      ]);
  });

  it('refuses no built-in spelling with an effort suffix the built-in splits', async () => {
    const published = await publishedTable();
    for (const [written, canonical] of Object.entries(builtin.aliases))
      for (const suffix of builtin.effortSuffixes)
        expect([
          `${written}${suffix}`,
          normalizeModel(`${written}${suffix}`, null, published.models, published.table),
        ]).toEqual([`${written}${suffix}`, { id: canonical, effort: suffix.slice(1) }]);
  });

  it('maps the 35 tally runs the built-in copy left unmapped', async () => {
    const published = await publishedTable();
    const builtinTable = { aliases: builtin.aliases, effortSuffixes: builtin.effortSuffixes };
    // agent-report's copy came with the catalog's 16 ids of Nov 11, before these three were added.
    const NEW_MODELS = [
      'deepseek/deepseek-v4.1-flash',
      'alibaba/qwen3.8-max',
      'alibaba/qwen4-max-preview',
    ];
    const nov11 = published.models.filter((model) => !NEW_MODELS.includes(model.model));
    expect(nov11).toHaveLength(16);
    const known = new Set(published.models.map((model) => model.model));
    let runs = 0;
    for (const [written, count] of UNMAPPED_BY_BUILTIN) {
      // agent-report's copy alone does not know it (its catalog of 16 lacks these models)...
      expect(builtin.aliases[written]).toBeUndefined();
      expect(normalizeModel(written, null, nov11, builtinTable).id).toBe(written);
      // ...the published table does.
      const resolved = normalizeModel(written, null, published.models, published.table);
      expect(known.has(resolved.id)).toBe(true);
      expect(resolved.effort).toBeNull();
      runs += count;
    }
    expect(runs).toBe(35);
  });

  // Review 1, L5: a catalog model whose own name ends in an effort word must have its spellings as
  // explicit entries, so no reader (AAC or agent-report) can split the word off as an effort.
  it('gives every catalog model whose name ends in an effort word explicit entries', () => {
    const suffixes = [...new Set([...packaged.effortSuffixes, ...builtin.effortSuffixes])];
    const named = models.filter((model) => {
      const name = model.model.slice(model.model.indexOf('/') + 1);
      return suffixes.some((suffix) => name.endsWith(suffix));
    });
    // Today only Qwen 3.8 Max; a new one fails here until its entries are added.
    expect(named.map((model) => model.model)).toEqual(['alibaba/qwen3.8-max']);
    for (const { model } of named) {
      const name = model.slice(model.indexOf('/') + 1);
      expect([name, packaged.aliases[name]]).toEqual([name, model]);
      const spelled = Object.entries(packaged.aliases).filter(([, target]) => target === model);
      expect(spelled.length).toBeGreaterThanOrEqual(3);
    }
  });

  it("keeps '-max' in Qwen's names: it is part of the name, never an effort", async () => {
    const published = await publishedTable();
    const resolve = (id: string, effort: string | null = null) =>
      normalizeModel(id, effort, published.models, published.table);
    for (const written of [
      'alibaba/qwen3.8-max',
      'qwen3.8-max',
      'alibaba-token-plan/qwen3.8-max',
      'omp/alibaba-token-plan/qwen3.8-max',
    ])
      expect([written, resolve(written)]).toEqual([
        written,
        { id: 'alibaba/qwen3.8-max', effort: null },
      ]);
    for (const written of [
      'alibaba/qwen4-max-preview',
      'qwen4-max-preview',
      'alibaba-token-plan/qwen4-max-preview',
      'omp/alibaba-token-plan/qwen4-max-preview',
    ])
      expect([written, resolve(written)]).toEqual([
        written,
        { id: 'alibaba/qwen4-max-preview', effort: null },
      ]);
    // Every -max spelling is an explicit entry, so a reader that splits suffixes before a bare
    // lookup still keeps it whole, even if a `qwen3.8` model is ever listed.
    for (const written of ['qwen3.8-max', 'alibaba-token-plan/qwen3.8-max'])
      expect(packaged.aliases[written]).toBe('alibaba/qwen3.8-max');
    const withBase = [...published.models, { model: 'alibaba/qwen3.8', efforts: ['max'] }];
    expect(normalizeModel('qwen3.8-max', null, withBase, published.table)).toEqual({
      id: 'alibaba/qwen3.8-max',
      effort: null,
    });
    // A real effort after the name still splits off, and the run's own effort wins.
    expect(resolve('qwen3.8-max-high')).toEqual({ id: 'alibaba/qwen3.8-max', effort: 'high' });
    expect(resolve('alibaba-token-plan/qwen3.8-max', 'low')).toEqual({
      id: 'alibaba/qwen3.8-max',
      effort: 'low',
    });
  });
});
