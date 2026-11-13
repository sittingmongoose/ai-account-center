/**
 * Publishing the packaged catalog (catalog.ts `seedCatalog`): a missing file is seeded whole; an
 * existing `models.json` or `model-aliases.json` gains only the packaged ids it lacks, written by a
 * temporary file and a rename, with every byte already on the share kept; a share file that is not
 * valid is left alone and named in the catalog Source.
 */
import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import fs from 'fs';
import path from 'path';
import {
  CATALOG_FILES,
  CatalogReader,
  memberSpan,
  packagedCatalogDir,
  parseCatalogFile,
  seedCatalog,
  type SeedFailure,
} from '../../../../src/web-server/scoreboard/catalog';
import { ScoreboardService } from '../../../../src/web-server/scoreboard/scoreboard-service';
import { NOW, tempDirs } from '../status/status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const packaged = (name: string) => fs.readFileSync(path.join(packagedCatalogDir(), name), 'utf8');
const packagedModels = JSON.parse(packaged('models.json')) as {
  schema: string;
  models: Array<Record<string, unknown> & { model: string }>;
};
const packagedAliases = JSON.parse(packaged('model-aliases.json')) as {
  schema: string;
  aliases: Record<string, string>;
  effortSuffixes: string[];
};
/** The three models added on 2026-11-13, which the share's Nov 11 models.json lacks. */
const NEW_MODELS = [
  'deepseek/deepseek-v4.1-flash',
  'alibaba/qwen3.8-max',
  'alibaba/qwen4-max-preview',
];
const pretty = (doc: unknown) => `${JSON.stringify(doc, null, 2)}\n`;

function catalogDir(): { root: string; dir: string } {
  const root = dirs.make();
  const dir = path.join(root, 'v1', 'catalog');
  fs.mkdirSync(dir, { recursive: true });
  return { root, dir };
}

async function seed(root: string, sourceDir?: string) {
  const failures: SeedFailure[] = [];
  const written = await seedCatalog(root, sourceDir, (failure) => failures.push(failure));
  return { written, failures };
}

const dotFiles = (dir: string) => fs.readdirSync(dir).filter((name) => name.startsWith('.'));

describe('seedCatalog publishing', () => {
  it('seeds a missing file whole, byte for byte', async () => {
    const { root, dir } = catalogDir();
    const { written, failures } = await seed(root);
    expect(written.sort()).toEqual([...CATALOG_FILES].sort());
    expect(failures).toEqual([]);
    for (const name of CATALOG_FILES)
      expect(fs.readFileSync(path.join(dir, name), 'utf8')).toBe(packaged(name));
    expect(dotFiles(dir)).toEqual([]);
  });

  it('adds only the packaged models a share models.json lacks, after its own, in its layout', async () => {
    const { root, dir } = catalogDir();
    // The share as of Nov 11: the packaged models without the three new ones, plus one the owner added.
    const owner = {
      model: 'example/owner-model',
      label: 'Owner Model',
      family: null,
      runtimes: [],
      efforts: [],
      prices: null,
      notes: 'added on the share',
    };
    const nov11 = {
      schema: packagedModels.schema,
      models: [...packagedModels.models.filter((row) => !NEW_MODELS.includes(row.model)), owner],
    };
    const file = path.join(dir, 'models.json');
    fs.writeFileSync(file, pretty(nov11));
    fs.chmodSync(file, 0o664);
    const { written, failures } = await seed(root);
    expect(failures).toEqual([]);
    expect(written).toContain('models.json');
    const added = packagedModels.models.filter((row) => NEW_MODELS.includes(row.model));
    expect(added.map((row) => row.model)).toEqual(NEW_MODELS);
    // The same file a person would write: the share's entries first and unchanged, the new ones after.
    expect(fs.readFileSync(file, 'utf8')).toBe(
      pretty({ schema: nov11.schema, models: [...nov11.models, ...added] })
    );
    expect(fs.statSync(file).mode & 0o777).toBe(0o664);
    expect(dotFiles(dir)).toEqual([]);
    // Nothing left to add: the next start writes nothing and leaves the file as it is.
    const mtime = fs.statSync(file).mtimeMs;
    const again = await seed(root);
    expect(again.written).toEqual([]);
    expect(again.failures).toEqual([]);
    expect(fs.statSync(file).mtimeMs).toBe(mtime);
  });

  it('never changes an edited entry, its unknown fields or its layout; share edits win', async () => {
    const { root, dir } = catalogDir();
    // Hand-written on one line, with an owner's edit of a packaged model and a field AAC does not know.
    const text =
      '{"schema":"catalog-models@1","models":[{"model":"anthropic/claude-opus-5-5","label":"Opus (owner)",' +
      '"family":"Claude","runtimes":[],"efforts":["max"],"prices":{"input":1.50,"output":7.0},"notes":"","ownerField":1}]}';
    const file = path.join(dir, 'models.json');
    fs.writeFileSync(file, text);
    const { failures } = await seed(root);
    expect(failures).toEqual([]);
    const next = fs.readFileSync(file, 'utf8');
    const close = text.lastIndexOf(']');
    // Every byte of the share's text is kept; only new entries are inserted before the array closes.
    expect(next.startsWith(text.slice(0, close))).toBe(true);
    expect(next.endsWith(text.slice(close))).toBe(true);
    const doc = JSON.parse(next) as typeof packagedModels;
    expect(doc.models[0]).toEqual(JSON.parse(text).models[0]);
    expect(doc.models.filter((row) => row.model === 'anthropic/claude-opus-5-5')).toHaveLength(1);
    expect(doc.models.map((row) => row.model)).toEqual(
      packagedModels.models.map((row) => row.model)
    );
  });

  it('adds only the alias keys and effort suffixes the share lacks, keeping its own values', async () => {
    const { root, dir } = catalogDir();
    const share = {
      schema: packagedAliases.schema,
      aliases: { 'gpt-6.1-sol': 'example/owner-choice', 'owner-written': 'zai/glm-5.3' },
      effortSuffixes: ['-high'],
    };
    const file = path.join(dir, 'model-aliases.json');
    fs.writeFileSync(file, pretty(share));
    const { written, failures } = await seed(root);
    expect(failures).toEqual([]);
    expect(written).toContain('model-aliases.json');
    const doc = JSON.parse(fs.readFileSync(file, 'utf8')) as typeof packagedAliases;
    expect(doc.aliases['gpt-6.1-sol']).toBe('example/owner-choice');
    expect(doc.aliases['owner-written']).toBe('zai/glm-5.3');
    // The share's own suffix stays first; the packaged ones it lacks follow in packaged order.
    expect(doc.effortSuffixes).toEqual([
      '-high',
      ...packagedAliases.effortSuffixes.filter((suffix) => suffix !== '-high'),
    ]);
    const missing = Object.keys(packagedAliases.aliases).filter((key) => key !== 'gpt-6.1-sol');
    expect(Object.keys(doc.aliases)).toEqual(['gpt-6.1-sol', 'owner-written', ...missing]);
    for (const key of missing) expect(doc.aliases[key]).toBe(packagedAliases.aliases[key]);
    expect(fs.readFileSync(file, 'utf8')).toBe(pretty(doc));
    const parsed = parseCatalogFile('model-aliases.json', fs.readFileSync(file));
    expect(parsed.ok).toBe(true);
  });

  it('fills an empty list in the layout of the line that names it', async () => {
    const { root, dir } = catalogDir();
    const file = path.join(dir, 'models.json');
    fs.writeFileSync(file, '{\r\n  "schema": "catalog-models@1",\r\n  "models": []\r\n}\r\n');
    await seed(root);
    const next = fs.readFileSync(file, 'utf8');
    expect(
      next.startsWith('{\r\n  "schema": "catalog-models@1",\r\n  "models": [\r\n    {\r\n')
    ).toBe(true);
    expect(next.endsWith('\r\n    }\r\n  ]\r\n}\r\n')).toBe(true);
    expect(next.replace(/\r\n/g, '\n')).toBe(pretty(packagedModels));
  });

  it('leaves a share file that is not valid alone and names it in the catalog Source', async () => {
    const { root, dir } = catalogDir();
    const models = path.join(dir, 'models.json');
    const aliases = path.join(dir, 'model-aliases.json');
    fs.writeFileSync(models, '{ damaged');
    fs.writeFileSync(aliases, '{"schema":"agent-reports/model-aliases@1","aliases":[]}');
    const { written, failures } = await seed(root);
    expect(written).not.toContain('models.json');
    expect(written).not.toContain('model-aliases.json');
    expect(failures).toEqual([
      { file: 'models.json', code: 'invalid' },
      { file: 'model-aliases.json', code: 'invalid' },
    ]);
    expect(fs.readFileSync(models, 'utf8')).toBe('{ damaged');
    expect(fs.readFileSync(aliases, 'utf8')).toBe(
      '{"schema":"agent-reports/model-aliases@1","aliases":[]}'
    );
    expect(dotFiles(dir)).toEqual([]);

    const service = new ScoreboardService({
      root: () => root,
      now: () => NOW,
      timeZone: () => 'UTC',
    });
    await service.seed();
    const { detail } = (await service.view()).sources[1];
    expect(detail).toContain('catalog/models.json (not valid JSON)');
    expect(detail).toContain('Could not add new packaged entries to models.json: invalid.');
    expect(detail).toContain('Could not add new packaged entries to model-aliases.json: invalid.');
    expect(detail).not.toContain('damaged');
  });

  it('leaves alone a file that names its list twice', async () => {
    const { root, dir } = catalogDir();
    const file = path.join(dir, 'models.json');
    // JSON.parse keeps the second list, so the file reads; where to add is not clear, so nothing is added.
    const text = '{"schema":"catalog-models@1","models":[{"model":"a/b","label":"B"}],"models":[]}';
    fs.writeFileSync(file, text);
    const { failures } = await seed(root);
    expect(failures).toEqual([{ file: 'models.json', code: 'unmergeable' }]);
    expect(fs.readFileSync(file, 'utf8')).toBe(text);
  });

  it('reads again when the share file changes during the write, keeping that edit', async () => {
    const { root, dir } = catalogDir();
    const file = path.join(dir, 'models.json');
    const base = {
      schema: packagedModels.schema,
      models: packagedModels.models.filter((row) => !NEW_MODELS.includes(row.model)),
    };
    fs.writeFileSync(file, pretty(base));
    const edited = structuredClone(base);
    edited.models[0].notes = 'edited while AAC started';
    const realWrite = fs.promises.writeFile;
    let edits = 0;
    const write = spyOn(fs.promises, 'writeFile').mockImplementation((async (
      ...args: Parameters<typeof fs.promises.writeFile>
    ) => {
      await realWrite(...args);
      // Another writer saves the share file after AAC read it and before the rename, once.
      if (edits++ === 0) fs.writeFileSync(file, pretty(edited));
    }) as typeof fs.promises.writeFile);
    let result: Awaited<ReturnType<typeof seed>>;
    try {
      result = await seed(root);
    } finally {
      write.mockRestore();
    }
    expect(result.failures).toEqual([]);
    expect(result.written).toContain('models.json');
    const added = packagedModels.models.filter((row) => NEW_MODELS.includes(row.model));
    expect(fs.readFileSync(file, 'utf8')).toBe(
      pretty({ schema: edited.schema, models: [...edited.models, ...added] })
    );
    expect(dotFiles(dir)).toEqual([]);
  });

  it('names a share file that keeps changing, and leaves it as the last writer left it', async () => {
    const { root, dir } = catalogDir();
    const file = path.join(dir, 'models.json');
    const base = { schema: packagedModels.schema, models: [packagedModels.models[0]] };
    fs.writeFileSync(file, pretty(base));
    const realWrite = fs.promises.writeFile;
    let edits = 0;
    const write = spyOn(fs.promises, 'writeFile').mockImplementation((async (
      ...args: Parameters<typeof fs.promises.writeFile>
    ) => {
      await realWrite(...args);
      const doc = structuredClone(base);
      doc.models[0].notes = `edit ${++edits}`;
      fs.writeFileSync(file, pretty(doc));
    }) as typeof fs.promises.writeFile);
    let result: Awaited<ReturnType<typeof seed>>;
    try {
      result = await seed(root);
    } finally {
      write.mockRestore();
    }
    expect(result.failures).toEqual([{ file: 'models.json', code: 'busy' }]);
    expect(result.written).not.toContain('models.json');
    const doc = JSON.parse(fs.readFileSync(file, 'utf8')) as typeof packagedModels;
    expect(doc.models).toHaveLength(1);
    expect(doc.models[0].notes).toBe('edit 3');
    expect(dotFiles(dir)).toEqual([]);
  });

  it('publishes the merged table and the new models to a share as of Nov 11 on the next start', async () => {
    const { root, dir } = catalogDir();
    // The live share then: models.json with the 16 ids, the other kinds, and no alias table yet.
    const nov11 = {
      schema: packagedModels.schema,
      models: packagedModels.models.filter((row) => !NEW_MODELS.includes(row.model)),
    };
    expect(nov11.models).toHaveLength(16);
    fs.writeFileSync(path.join(dir, 'models.json'), pretty(nov11));
    for (const name of ['task-types.json', 'measures.json', 'uses.json', 'benchmarks.json'])
      fs.writeFileSync(path.join(dir, name), packaged(name));
    const { written, failures } = await seed(root);
    expect(failures).toEqual([]);
    expect(written.sort()).toEqual(['model-aliases.json', 'models.json']);
    expect(fs.readFileSync(path.join(dir, 'model-aliases.json'), 'utf8')).toBe(
      packaged('model-aliases.json')
    );
    const reader = new CatalogReader({ root: () => root, now: () => NOW });
    await reader.scan();
    const state = reader.snapshot();
    expect(state.problems).toEqual([]);
    expect(state.missingFiles).toEqual([]);
    expect(state.models.map((row) => row.model)).toEqual([
      ...nov11.models.map((row) => row.model),
      ...NEW_MODELS,
    ]);
    expect(Object.keys(state.aliases)).toHaveLength(Object.keys(packagedAliases.aliases).length);
    expect(state.effortSuffixes).toEqual(packagedAliases.effortSuffixes);
  });

  it('leaves the other catalog files as they are on the share', async () => {
    const { root, dir } = catalogDir();
    const uses = path.join(dir, 'uses.json');
    fs.writeFileSync(uses, pretty({ schema: 'catalog-uses@1', uses: [] }));
    const { written } = await seed(root);
    expect(written).not.toContain('uses.json');
    expect(fs.readFileSync(uses, 'utf8')).toBe(pretty({ schema: 'catalog-uses@1', uses: [] }));
  });
});

describe('memberSpan', () => {
  it('finds a top-level member past strings holding brackets, quotes and escapes', () => {
    const text =
      '{"notes":"a ] } [ { \\" , : x","nested":{"models":[1]},"models" : [ {"model":"a"} ] ,"z":"models"}';
    const span = memberSpan(text, 'models');
    expect(span).not.toBeNull();
    if (!span) return;
    expect(text.slice(span.open, span.close + 1)).toBe('[ {"model":"a"} ]');
  });

  it('finds nothing for a missing, repeated or plain member', () => {
    expect(memberSpan('{"nested":{"models":[]}}', 'models')).toBeNull();
    expect(memberSpan('{"models":[],"models":[]}', 'models')).toBeNull();
    expect(memberSpan('{"models":"[]"}', 'models')).toBeNull();
    expect(memberSpan('{"aliases":{"a\\\\b":"c"}}', 'aliases')).toEqual({ open: 11, close: 22 });
  });
});
