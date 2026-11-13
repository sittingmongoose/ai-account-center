/**
 * Model aliases end to end: every spelling kind from the 2026-11-13 tally resolves to its
 * canonical `models.json` id, effort suffixes move, unknowns stay unknown (reported, never
 * guessed), and the view counts what was not scored.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import {
  CATALOG_FILES,
  CatalogReader,
  packagedCatalogDir,
  parseCatalogFile,
  seedCatalog,
} from '../../../../src/web-server/scoreboard/catalog';
import { RunsReader } from '../../../../src/web-server/scoreboard/runs-reader';
import { normalizeModel } from '../../../../src/web-server/scoreboard/model-identity';
import { buildScoreboardView } from '../../../../src/web-server/scoreboard/scoreboard-view';
import { NOW, tempDirs } from '../status/status-test-helpers';
import { runDoc, writeRun } from './scoreboard-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const { models } = (await import(
  '../../../../docs/agent-reports/catalog/models.json'
)) as unknown as { models: Array<{ model: string; efforts?: string[] }> };
const aliasJson = (await import(
  '../../../../docs/agent-reports/catalog/model-aliases.json'
)) as unknown as {
  schema: string;
  aliases: Record<string, string>;
  effortSuffixes: string[];
};
const table = { aliases: aliasJson.aliases, effortSuffixes: aliasJson.effortSuffixes };
const known = new Set(models.map((model) => model.model));
const resolve = (id: string, effort: string | null = null) => normalizeModel(id, effort, models, table);

/** Every distinct raw model id in model-id-tally.txt (2026-11-13), in tally order. */
const TALLY_IDS = [
  'gpt-6.1-sol',
  'openai/gpt-6.1-sol',
  'anthropic/claude-opus-5-5',
  'claude-opus-5-5',
  'openai/gpt-6-astra',
  'codex/gpt-6.1-sol',
  'gpt-6-astra',
  'openai/gpt-6-luna',
  'xai/grok-4.7',
  'muse/muse-wonder-full-1.0',
  'muse/muse-ember-1.0',
  'muse-ember-1.0',
  'opencode-go/deepseek-v4.1-flash',
  'grok-4.7',
  'codex/gpt-6-astra',
  'claude-fable-5-2',
  'anthropic/claude-fable-5-2',
  'muse-wonder-full-1.0',
  'glm-5.3',
  'zai/glm-5.3',
  'builtin:zai-coding-plan\\GLM-5.3',
  'google/gemini-4-argon-high',
  'zcode/glm-5.3',
  'anthropic/claude-fable-5-1',
  'muse-spark-1.3',
  'deepseek-v4.1-flash',
  'builtin:bigmodel-coding-plan\\GLM-5.3',
  'alibaba-token-plan/qwen3.8-max',
  'anthropic/claude-haiku-5-5',
  'cursor/grok-4.7',
  'anthropic/claude-sonnet-5-5',
];

describe('alias table', () => {
  it('is the shared table with the documented shape', () => {
    expect(aliasJson.schema).toBe('agent-reports/model-aliases@1');
    expect(Array.isArray(aliasJson.effortSuffixes)).toBe(true);
    expect(aliasJson.effortSuffixes).toContain('-high');
    for (const suffix of aliasJson.effortSuffixes) expect(suffix.startsWith('-')).toBe(true);
    const parsed = parseCatalogFile(
      'model-aliases.json',
      fs.readFileSync(path.join(packagedCatalogDir(), 'model-aliases.json'))
    );
    expect(parsed.ok).toBe(true);
    if (parsed.ok) {
      expect(parsed.value.kind).toBe('aliases');
      expect(parsed.value.aliases).toEqual(aliasJson.aliases);
      expect(parsed.value.effortSuffixes).toEqual(aliasJson.effortSuffixes);
    }
  });

  it('refuses a bad alias file with fixed words that never echo the file', () => {
    const parse = (value: unknown) =>
      parseCatalogFile('model-aliases.json', Buffer.from(JSON.stringify(value)));
    expect(parse({ schema: 'wrong', aliases: {}, effortSuffixes: [] })).toEqual({
      ok: false,
      reason: 'schema is missing or not valid',
    });
    expect(parse({ schema: 'agent-reports/model-aliases@1', effortSuffixes: [] })).toEqual({
      ok: false,
      reason: 'aliases is missing or not valid',
    });
    expect(parse({ schema: 'agent-reports/model-aliases@1', aliases: [] })).toEqual({
      ok: false,
      reason: 'aliases is missing or not valid',
    });
    expect(
      parse({
        schema: 'agent-reports/model-aliases@1',
        aliases: { 'a/b': 'c/d' },
        effortSuffixes: ['high'],
      })
    ).toEqual({ ok: false, reason: 'effortSuffixes[0] is missing or not valid' });
  });

  it('publishes model-aliases.json the same way as models.json (seed, never over the share)', async () => {
    expect(CATALOG_FILES).toContain('model-aliases.json');
    const root = dirs.make();
    const catalog = path.join(root, 'v1', 'catalog');
    fs.mkdirSync(catalog, { recursive: true });
    const owned = path.join(catalog, 'model-aliases.json');
    fs.writeFileSync(owned, '{"owner":"edited"}');
    const written = await seedCatalog(root);
    expect(written).toContain('models.json');
    expect(written).not.toContain('model-aliases.json');
    expect(fs.readFileSync(owned, 'utf8')).toBe('{"owner":"edited"}');
    const fresh = dirs.make();
    fs.mkdirSync(path.join(fresh, 'v1', 'catalog'), { recursive: true });
    const writtenFresh = await seedCatalog(fresh);
    expect(writtenFresh).toContain('model-aliases.json');
  });

  it('resolves every bare id to its canonical catalog id', () => {
    const bare: Array<[string, string]> = [
      ['gpt-6.1-sol', 'openai/gpt-6.1-sol'],
      ['claude-opus-5-5', 'anthropic/claude-opus-5-5'],
      ['gpt-6-astra', 'openai/gpt-6-astra'],
      ['muse-ember-1.0', 'muse/muse-ember-1.0'],
      ['grok-4.7', 'xai/grok-4.7'],
      ['claude-fable-5-2', 'anthropic/claude-fable-5-2'],
      ['muse-wonder-full-1.0', 'muse/muse-wonder-full-1.0'],
      ['glm-5.3', 'zai/glm-5.3'],
      ['deepseek-v4.1-flash', 'deepseek/deepseek-v4.1-flash'],
      ['muse-spark-1.3', 'muse/muse-spark-1.3-contributor'],
    ];
    for (const [written, canonical] of bare)
      expect(resolve(written)).toEqual({ id: canonical, effort: null });
  });

  it('resolves provider-instance prefixes only by their full written string', () => {
    const prefixed: Array<[string, string]> = [
      ['codex/gpt-6.1-sol', 'openai/gpt-6.1-sol'],
      ['codex/gpt-6-astra', 'openai/gpt-6-astra'],
      ['zcode/glm-5.3', 'zai/glm-5.3'],
      ['cursor/grok-4.7', 'xai/grok-4.7'],
      ['opencode-go/deepseek-v4.1-flash', 'deepseek/deepseek-v4.1-flash'],
      ['alibaba-token-plan/qwen3.8-max', 'alibaba/qwen3.8-max'],
      ['omp/alibaba-token-plan/qwen4-max-preview', 'alibaba/qwen4-max-preview'],
    ];
    for (const [written, canonical] of prefixed)
      expect(resolve(written)).toEqual({ id: canonical, effort: null });
    // A new prefix of a known bare id is not stripped: prefixes never match by rule.
    expect(resolve('codex/future-model')).toEqual({ id: 'codex/future-model', effort: null });
    expect(resolve('zcode/gpt-6.1-sol')).toEqual({ id: 'zcode/gpt-6.1-sol', effort: null });
  });

  it('resolves harness spellings by their full written string, exactly and case-sensitively', () => {
    expect(resolve('builtin:zai-coding-plan\\GLM-5.3')).toEqual({
      id: 'zai/glm-5.3',
      effort: null,
    });
    expect(resolve('builtin:bigmodel-coding-plan\\GLM-5.3')).toEqual({
      id: 'zai/glm-5.3',
      effort: null,
    });
    expect(resolve('builtin:other-plan\\GLM-5.3')).toEqual({
      id: 'builtin:other-plan\\GLM-5.3',
      effort: null,
    });
    expect(resolve('builtin:zai-coding-plan\\glm-5.3')).toEqual({
      id: 'builtin:zai-coding-plan\\glm-5.3',
      effort: null,
    });
  });

  it('moves an effort suffix to model.effort, keeping an effort the run already names', () => {
    expect(resolve('google/gemini-4-argon-high')).toEqual({
      id: 'google/gemini-4-argon',
      effort: 'high',
    });
    expect(resolve('google/gemini-4-argon-high', 'max')).toEqual({
      id: 'google/gemini-4-argon',
      effort: 'max',
    });
    expect(resolve('gemini-4-argon-high')).toEqual({
      id: 'google/gemini-4-argon',
      effort: 'high',
    });
    expect(normalizeModel('gemini-4-argon-high', 'max', models)).toEqual({
      id: 'google/gemini-4-argon',
      effort: 'max',
    });
  });

  it('leaves every unknown id unknown: reported, never guessed', () => {
    const unknowns = [
      'future-model-9.9',
      'GPT-6.1-SOL',
      'codex/future-model',
      'openai/gpt-6.1-sol-2',
      'google/gemini-4-argon-thinking',
    ];
    for (const unknown of unknowns) {
      const resolved = resolve(unknown);
      expect(resolved).toEqual({ id: unknown, effort: null });
      expect(known.has(resolved.id)).toBe(false);
    }
    // An alias whose target is not in the catalog resolves nothing, even via the table.
    expect(
      normalizeModel('future-x', null, models, {
        aliases: { 'future-x': 'example/not-in-catalog' },
        effortSuffixes: [],
      })
    ).toEqual({ id: 'future-x', effort: null });
  });

  it('resolves every id in the real tally to a catalog id', () => {
    expect(TALLY_IDS).toHaveLength(31);
    for (const written of TALLY_IDS) {
      const { id } = resolve(written);
      if (!known.has(id)) throw new Error(`tally id ${written} resolved to unknown ${id}`);
      expect(known.has(id)).toBe(true);
    }
  });
});

describe('the view with aliases', () => {
  async function aliasView() {
    const root = dirs.make();
    const catalogDir = path.join(root, 'v1', 'catalog');
    fs.mkdirSync(catalogDir, { recursive: true });
    for (const name of fs.readdirSync(packagedCatalogDir()))
      fs.copyFileSync(path.join(packagedCatalogDir(), name), path.join(catalogDir, name));
    const writer = 'vm.alias-lead';
    const model = (provider: string | null, id: string | null) => ({
      provider,
      id,
      harness: 'muse',
      effort: null,
      account: null,
    });
    // Three spellings of Sol 6.1 must land in one row.
    writeRun(
      root,
      runDoc(writer, 'sol-canonical', { model: model('openai', 'openai/gpt-6.1-sol') })
    );
    writeRun(root, runDoc(writer, 'sol-bare', { model: model(null, 'gpt-6.1-sol') }));
    writeRun(root, runDoc(writer, 'sol-prefix', { model: model(null, 'codex/gpt-6.1-sol') }));
    // An effort in the id and a harness spelling, each merged into their catalog row.
    writeRun(
      root,
      runDoc(writer, 'gemini-high', {
        model: model('google', 'google/gemini-4-argon-high'),
        endedAt: new Date(NOW - 5 * 3_600_000).toISOString(),
      })
    );
    writeRun(
      root,
      runDoc(writer, 'glm-harness', {
        model: model(null, 'builtin:zai-coding-plan\\GLM-5.3'),
        endedAt: new Date(NOW - 4 * 3_600_000).toISOString(),
      })
    );
    // What stays unscored: unknown model, no model, no task type.
    writeRun(root, runDoc(writer, 'future', { model: model(null, 'future-model-9.9') }));
    writeRun(
      root,
      runDoc(writer, 'no-model', {
        model: { provider: null, id: null, harness: 'muse', effort: null, account: null },
      })
    );
    writeRun(
      root,
      runDoc(writer, 'no-type', {
        model: model('zai', 'glm-5.3'),
        task: { id: 'x', type: null, title: 'Unknown work x', size: { tasks: 1 } },
      })
    );
    const options = { root: () => root, now: () => NOW };
    const catalog = new CatalogReader(options);
    const runs = new RunsReader(options);
    await Promise.all([catalog.scan(), runs.scan()]);
    return buildScoreboardView({
      now: NOW,
      timeZone: 'UTC',
      root,
      catalog: catalog.snapshot(),
      runs: runs.snapshot(),
    });
  }

  function rows(view: ReturnType<typeof buildScoreboardView>, use: string, type: string) {
    const found = view.uses.find((row) => row.id === use)?.taskTypes.find((t) => t.id === type);
    if (!found) throw new Error(`no ${use}/${type}`);
    return found.rows;
  }

  it('counts aliased spellings in their canonical rows', async () => {
    const view = await aliasView();
    const build = rows(view, 'building', 'build');
    const sol = build.find((row) => row.model === 'openai/gpt-6.1-sol');
    expect(sol?.n.observed).toBe(3);
    expect(sol?.inCatalog).toBe(true);
    expect(sol?.label).toBe('Sol 6.1');
    const gemini = build.find((row) => row.model === 'google/gemini-4-argon');
    expect(gemini?.effort).toBe('high');
    expect(gemini?.inCatalog).toBe(true);
    const glm = build.find((row) => row.model === 'zai/glm-5.3');
    expect(glm?.inCatalog).toBe(true);
    // The unknown spelling keeps its own row, labelled by its id, never guessed.
    const future = build.find((row) => row.model === 'future-model-9.9');
    expect(future).toMatchObject({ label: 'future-model-9.9', inCatalog: false });
  });

  it('counts what was not scored by reason, with examples', async () => {
    const view = await aliasView();
    expect(view.unscored).toMatchObject({
      total: 3,
      noModel: 1,
      noTaskType: 1,
      unknownModel: 1,
      unknownModels: [{ id: 'future-model-9.9', runs: 1 }],
    });
    expect(view.unscored.examples.map((example) => example.reason).sort()).toEqual([
      'no-model',
      'no-task-type',
      'unknown-model',
    ]);
    const unknown = view.unscored.examples.find((example) => example.reason === 'unknown-model');
    expect(unknown).toMatchObject({ model: 'future-model-9.9', taskType: 'build' });
    const noModel = view.unscored.examples.find((example) => example.reason === 'no-model');
    expect(noModel?.model).toBeNull();
    const noType = view.unscored.examples.find((example) => example.reason === 'no-task-type');
    expect(noType?.taskType).toBeNull();
    // No account ever reaches the view, examples included.
    expect(JSON.stringify(view.unscored)).not.toContain('account');
  });
});
