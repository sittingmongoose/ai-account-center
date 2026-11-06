import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import {
  nodeReaderFs,
  type ReaderFs,
} from '../../../../src/web-server/status/agent-reports-reader';
import { RunsReader } from '../../../../src/web-server/scoreboard/runs-reader';
import {
  CATALOG_FILES,
  CatalogReader,
  packagedCatalogDir,
  parseCatalogFile,
  seedCatalog,
} from '../../../../src/web-server/scoreboard/catalog';
import { NOW, tempDirs } from '../status/status-test-helpers';
import { runDoc, writeRun } from './scoreboard-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const REAL_RUNS = path.resolve(import.meta.dir, '../../../fixtures/scoreboard');

function reader(root: string, overrides: Partial<{ now: () => number; fs: ReaderFs }> = {}) {
  return new RunsReader({ root: () => root, now: () => NOW, ...overrides });
}

function errno(code: string) {
  return Object.assign(new Error(code), { code });
}

describe('runs reader', () => {
  it('reads real benchmark records, keeping their metrics, process and size', async () => {
    const root = dirs.make();
    const writer = 'windows.opus-typed-decisions';
    const target = path.join(root, 'v1', 'writers', writer, 'runs', '2026-10-31');
    fs.mkdirSync(target, { recursive: true });
    const names = fs.readdirSync(REAL_RUNS);
    expect(names).toHaveLength(3);
    for (const name of names) fs.copyFileSync(path.join(REAL_RUNS, name), path.join(target, name));
    const runs = reader(root);
    await runs.scan();
    const state = runs.snapshot();
    expect(state.problems).toEqual([]);
    expect(state.runs).toHaveLength(3);
    const doc = state.runs.find((run) => run.doc.id.endsWith('a-bmad-board-architecture'))?.doc;
    expect(doc?.model.id).toBe('openai/gpt-6.1-sol');
    expect(doc?.task.size?.words).toBe(1429);
    expect(doc?.metrics.answerKeyFinal).toBe(0);
    expect(doc?.process?.id).toBe('a');
    expect(doc?.imported?.from).toContain('pilot2');
  });

  it('keeps the highest seq per run id across date folders', async () => {
    const root = dirs.make();
    const doc = runDoc('vm.lead', 'A', { seq: 5, outcome: 'done' });
    writeRun(root, doc, '2026-11-06');
    writeRun(root, { ...doc, seq: 2, outcome: 'failed' }, '2026-11-05');
    const runs = reader(root);
    await runs.scan();
    const state = runs.snapshot();
    expect(state.runs.map((run) => [run.doc.id, run.doc.seq, run.doc.outcome])).toEqual([
      ['vm.lead--A', 5, 'done'],
    ]);
  });

  it('ignores a lower seq landing late in the same file', async () => {
    const root = dirs.make();
    const doc = runDoc('vm.lead', 'A', { seq: 5, outcome: 'done' });
    const file = writeRun(root, doc);
    const runs = reader(root);
    await runs.scan();
    fs.writeFileSync(file, JSON.stringify({ ...doc, seq: 1, outcome: 'failed' }));
    fs.utimesSync(file, new Date(NOW), new Date(NOW));
    await runs.scan();
    expect(runs.snapshot().runs[0].doc.outcome).toBe('done');
  });

  it('counts and names invalid files, and ignores temp files and other names', async () => {
    const root = dirs.make();
    const good = writeRun(root, runDoc('vm.lead', 'A'));
    const dir = path.dirname(good);
    fs.writeFileSync(path.join(dir, 'vm.lead--nul.json'), Buffer.from('{"a":\0}'));
    fs.writeFileSync(path.join(dir, 'vm.lead--broken.json'), '{ not json');
    fs.writeFileSync(
      path.join(dir, 'vm.lead--other.json'),
      JSON.stringify(runDoc('vm.other', 'B'))
    );
    fs.writeFileSync(path.join(dir, 'vm.lead--big.json'), Buffer.alloc(40 * 1024, 32));
    fs.writeFileSync(path.join(dir, '.vm.lead--A.json.tmp-1'), '{');
    fs.writeFileSync(path.join(dir, 'notes.txt'), 'x');
    fs.mkdirSync(path.join(root, 'v1', 'writers', 'vm.lead', 'runs', 'latest'));
    const runs = reader(root);
    await runs.scan();
    const state = runs.snapshot();
    expect(state.runs.map((run) => run.doc.id)).toEqual(['vm.lead--A']);
    const prefix = 'writers/vm.lead/runs/';
    expect(state.problems.map((problem) => [problem.file, problem.reason])).toEqual([
      [`${prefix}2026-11-06/vm.lead--big.json`, 'larger than the 32 KB cap'],
      [`${prefix}2026-11-06/vm.lead--broken.json`, 'not valid JSON'],
      [`${prefix}2026-11-06/vm.lead--nul.json`, 'contains NUL bytes from a damaged write'],
      [`${prefix}2026-11-06/vm.lead--other.json`, 'writer does not match its folder'],
      [`${prefix}latest`, 'folder name is not a date'],
    ]);
  });

  it('keeps the last good copy through damage, ENOENT and ESTALE', async () => {
    const root = dirs.make();
    const file = writeRun(root, runDoc('vm.lead', 'A', { seq: 1 }));
    let fail: Error | null = null;
    let failListing: Error | null = null;
    const flaky: ReaderFs = {
      ...nodeReaderFs,
      readdir: async (dir) => {
        if (failListing && dir.endsWith('2026-11-06')) throw failListing;
        return nodeReaderFs.readdir(dir);
      },
      stat: async (target) => {
        if (fail && target === file) throw fail;
        return nodeReaderFs.stat(target);
      },
    };
    const runs = reader(root, { fs: flaky });
    await runs.scan();
    expect(runs.snapshot().runs).toHaveLength(1);

    for (const error of [
      errno('ENOENT'),
      errno('ESTALE'),
      new Error('Unknown system error -116'),
    ]) {
      fail = error;
      await runs.scan();
      expect(runs.snapshot().runs).toHaveLength(1);
      expect(runs.snapshot().problems).toEqual([]);
    }
    fail = null;
    failListing = errno('ESTALE');
    await runs.scan();
    expect(runs.snapshot().runs).toHaveLength(1);
    expect(runs.snapshot().problems).toEqual([]);
    failListing = null;

    fs.writeFileSync(file, '{ damaged');
    fs.utimesSync(file, new Date(NOW), new Date(NOW));
    await runs.scan();
    const state = runs.snapshot();
    expect(state.runs).toHaveLength(1);
    expect(state.problems.map((problem) => problem.reason)).toEqual(['not valid JSON']);
  });

  it('keeps a file absent from its listing for the grace period, then forgets it', async () => {
    const root = dirs.make();
    const file = writeRun(root, runDoc('vm.lead', 'A'));
    let now = NOW;
    const runs = reader(root, { now: () => now });
    await runs.scan();
    fs.rmSync(file);
    await runs.scan();
    expect(runs.snapshot().runs).toHaveLength(1);
    now += 121_000;
    await runs.scan();
    expect(runs.snapshot().runs).toHaveLength(0);
  });

  it('lists a folder before reading it and reads only changed files', async () => {
    const root = dirs.make();
    const file = writeRun(root, runDoc('vm.lead', 'A'));
    const calls: string[] = [];
    const counting: ReaderFs = {
      readdir: (dir) => {
        calls.push(`list:${path.basename(dir)}`);
        return nodeReaderFs.readdir(dir);
      },
      stat: nodeReaderFs.stat,
      readFile: (target) => {
        calls.push(`read:${path.basename(target)}`);
        return nodeReaderFs.readFile(target);
      },
    };
    const runs = reader(root, { fs: counting });
    await runs.scan();
    expect(calls).toEqual([
      'list:writers',
      'list:vm.lead',
      'list:runs',
      'list:2026-11-06',
      `read:${path.basename(file)}`,
    ]);
    calls.length = 0;
    await runs.scan();
    expect(calls.some((call) => call.startsWith('read:'))).toBe(false);
  });

  it('says missing with no writers folder and an unlistable one is an error', async () => {
    const root = dirs.make();
    const missing = reader(path.join(root, 'none'));
    await missing.scan();
    expect(missing.snapshot().state).toBe('missing');
    const broken = reader(root, {
      fs: {
        ...nodeReaderFs,
        readdir: async () => {
          throw errno('EACCES');
        },
      },
    });
    await broken.scan();
    expect(broken.snapshot().state).toBe('error');
  });
});

describe('catalog', () => {
  it('accepts every packaged seed', () => {
    for (const name of CATALOG_FILES) {
      const parsed = parseCatalogFile(name, fs.readFileSync(path.join(packagedCatalogDir(), name)));
      expect(parsed.ok).toBe(true);
    }
  });

  it('refuses bad files with fixed words that never echo the file', () => {
    const parse = (value: unknown) =>
      parseCatalogFile('task-types.json', Buffer.from(JSON.stringify(value)));
    expect(parse({ schema: 'wrong', taskTypes: [] })).toEqual({
      ok: false,
      reason: 'schema is missing or not valid',
    });
    expect(parse({ schema: 'catalog-task-types@1' })).toEqual({
      ok: false,
      reason: 'taskTypes is missing or not valid',
    });
    expect(
      parse({
        schema: 'catalog-task-types@1',
        taskTypes: [{ id: 'a', label: 'A', unit: 'SECRET-WORD', measures: [] }],
      })
    ).toEqual({ ok: false, reason: 'taskTypes[0].unit is missing or not valid' });
    const row = { id: 'a', label: 'A', unit: 'task', measures: [] };
    expect(parse({ schema: 'catalog-task-types@1', taskTypes: [row, row] })).toEqual({
      ok: false,
      reason: 'taskTypes repeat an id',
    });
    expect(parseCatalogFile('models.json', Buffer.from('{\0')).ok).toBe(false);
    expect(
      parseCatalogFile(
        'measures.json',
        Buffer.from(JSON.stringify({ schema: 'catalog-measures@1', minRuns: 0, measures: [] }))
      )
    ).toEqual({ ok: false, reason: 'minRuns is missing or not valid' });
  });

  it('never overwrites a file on the share and writes only what is missing', async () => {
    const root = dirs.make();
    const catalog = path.join(root, 'v1', 'catalog');
    fs.mkdirSync(catalog, { recursive: true });
    const owned = path.join(catalog, 'models.json');
    fs.writeFileSync(owned, '{"owner":"edited"}');
    const written = await seedCatalog(root);
    expect(written.sort()).toEqual(CATALOG_FILES.filter((n) => n !== 'models.json').sort());
    expect(fs.readFileSync(owned, 'utf8')).toBe('{"owner":"edited"}');
    expect(fs.readdirSync(catalog).filter((name) => name.startsWith('.'))).toEqual([]);
    const first = fs.statSync(path.join(catalog, 'uses.json')).mtimeMs;
    expect(await seedCatalog(root)).toEqual([]);
    expect(fs.statSync(path.join(catalog, 'uses.json')).mtimeMs).toBe(first);
  });

  it('writes nothing without a catalog folder and never creates one', async () => {
    const root = dirs.make();
    expect(await seedCatalog(root)).toEqual([]);
    expect(fs.existsSync(path.join(root, 'v1'))).toBe(false);
  });

  it('shows a missing folder, a missing file and a damaged file, keeping the last good copy', async () => {
    const root = dirs.make();
    const catalog = new CatalogReader({ root: () => root, now: () => NOW });
    await catalog.scan();
    expect(catalog.snapshot().state).toBe('missing');
    expect(catalog.snapshot().minRuns).toBe(5);
    const dir = path.join(root, 'v1', 'catalog');
    fs.mkdirSync(dir, { recursive: true });
    await seedCatalog(root);
    fs.rmSync(path.join(dir, 'benchmarks.json'));
    await catalog.scan();
    let state = catalog.snapshot();
    expect(state.state).toBe('ok');
    expect(state.missingFiles).toEqual(['benchmarks.json']);
    expect(state.benchmarks).toEqual([]);
    expect(state.taskTypes.length).toBeGreaterThan(5);
    const count = state.taskTypes.length;
    const taskTypes = path.join(dir, 'task-types.json');
    fs.writeFileSync(taskTypes, '{ damaged');
    fs.utimesSync(taskTypes, new Date(NOW), new Date(NOW));
    await catalog.scan();
    state = catalog.snapshot();
    expect(state.problems.map((problem) => [problem.file, problem.reason])).toEqual([
      ['catalog/task-types.json', 'not valid JSON'],
    ]);
    expect(state.taskTypes).toHaveLength(count);
  });
});
