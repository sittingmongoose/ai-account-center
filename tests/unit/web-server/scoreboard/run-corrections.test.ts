import { afterEach, expect, it, spyOn } from 'bun:test';
import fs from 'fs';
import path from 'path';
import fixture from '../../../fixtures/run-corrections.json';
import { ReadersCheckWriter } from '../../../../src/web-server/status/readers-check';
import { RunsReader } from '../../../../src/web-server/scoreboard/runs-reader';
import { CatalogReader, seedCatalog } from '../../../../src/web-server/scoreboard/catalog';
import { buildScoreboardView } from '../../../../src/web-server/scoreboard/scoreboard-view';
import { NOW, tempDirs } from '../status/status-test-helpers';
import { runDoc, writeRun } from './scoreboard-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());
async function read() {
  const root = dirs.make('run-corrections-');
  for (const doc of fixture.runs) writeRun(root, doc as ReturnType<typeof runDoc>);
  fs.mkdirSync(path.join(root, 'v1/catalog'), { recursive: true });
  await seedCatalog(root);
  const options = { root: () => root, now: () => NOW };
  const reader = new RunsReader(options);
  const catalog = new CatalogReader(options);
  await Promise.all([reader.scan(), catalog.scan()]);
  return { root, reader, catalog };
}

it('resolves the shared correction fixture before counting, with honest refusals', async () => {
  const { reader } = await read();
  const state = reader.snapshot();
  expect(state.runs.map((r) => r.doc.id)).toEqual(fixture.ids);
  for (const [id, reason] of Object.entries(fixture.refusals)) {
    expect(state.problems.find((p) => p.file.endsWith(`--${id}.json`))?.reason).toContain(reason);
  }
  expect(state.runs.find((r) => r.doc.id.endsWith('--retarget'))?.doc.review?.of).toBe(
    'vm.corrections--build-new'
  );
  const hook = state.runs.find((r) => r.doc.id.endsWith('--hook-fixed'))?.doc;
  expect(hook?.model).toMatchObject({ provider: 'openai', effort: 'max' });
});

it('debits only the right builder after correcting a review of the wrong build', async () => {
  const { reader, catalog, root } = await read();
  const view = buildScoreboardView({
    root,
    now: NOW,
    timeZone: 'UTC',
    runs: reader.snapshot(),
    catalog: catalog.snapshot(),
  });
  const rows = view.uses
    .find((u) => u.id === 'building')
    ?.taskTypes.find((t) => t.id === 'build')?.rows;
  const opus = rows?.find((r) => r.model.includes('opus'))?.observed;
  expect(opus?.measures.quality).toMatchObject({ of: 0, per: 1 });
  const sol = rows?.filter((r) => r.model.includes('sol')) ?? [];
  const originalBuilder = sol.find((r) => r.effort === 'low')?.observed;
  expect(originalBuilder?.measures.quality).toMatchObject({ of: 1, per: 1 });
  expect(originalBuilder?.measures.overclaim.value).toBe(0);
  expect(sol.reduce((n, r) => n + (r.observed?.n ?? 0), 0)).toBe(4);
  expect(view.recent.some((r) => r.id.endsWith('--duplicate') || r.id.endsWith('--void'))).toBe(
    false
  );
});

it('resolves chains across date folders on each rescan and restores nothing from a losing branch', async () => {
  const { reader, root } = await read();
  writeRun(
    root,
    runDoc('vm.corrections', 'hook-final', { corrects: 'vm.corrections--hook-fixed' }),
    '2026-11-07'
  );
  writeRun(
    root,
    runDoc('vm.corrections', 'fork-child', { corrects: 'vm.corrections--fork-early' })
  );
  await reader.scan();
  expect(reader.snapshot().runs.some((r) => r.doc.id.endsWith('--hook-fixed'))).toBe(false);
  expect(reader.snapshot().runs.some((r) => r.doc.id.endsWith('--hook-final'))).toBe(true);
  expect(reader.snapshot().runs.some((r) => r.doc.id.endsWith('--fork-child'))).toBe(false);
  // The reports are all fixture files under the test's temporary root.
  expect(fs.existsSync(path.join(root, 'v1/writers/vm.corrections/runs'))).toBe(true);
});

it('refuses malformed correction fields without changing the original', async () => {
  const { reader, root } = await read();
  for (const [id, fields] of Object.entries({
    'empty-corrects': { corrects: '' },
    'orphan-void': { void: { reason: 'Duplicate' } },
    'empty-reason': { corrects: 'vm.corrections--wrong', void: { reason: '' } },
    'null-void': { corrects: 'vm.corrections--wrong', void: null },
  }))
    writeRun(root, runDoc('vm.corrections', id, fields));
  await reader.scan();
  expect(reader.snapshot().runs.some((r) => r.doc.id === 'vm.corrections--wrong')).toBe(true);
  for (const id of ['empty-corrects', 'orphan-void', 'empty-reason', 'null-void'])
    expect(reader.snapshot().problems.some((p) => p.file.endsWith(`--${id}.json`))).toBe(true);
});

it('uses real timestamp instants for conflicts and a stable id tie-break', async () => {
  const root = dirs.make();
  const docs = [
    runDoc('vm.corrections', 'base'),
    runDoc('vm.corrections', 'older', {
      corrects: 'vm.corrections--base',
      seq: 90,
      writtenAt: '2026-11-06T16:00:00.090Z',
    }),
    runDoc('vm.corrections', 'newer-a', {
      corrects: 'vm.corrections--base',
      writtenAt: '2026-11-06T16:00:00.1Z',
    }),
    runDoc('vm.corrections', 'newer-z', {
      corrects: 'vm.corrections--base',
      writtenAt: '2026-11-06T16:00:00.100Z',
    }),
  ];
  for (const doc of docs.reverse()) writeRun(root, doc);
  const reader = new RunsReader({ root: () => root, now: () => NOW });
  await reader.scan();
  expect(reader.snapshot().runs.map((r) => r.doc.id)).toEqual(['vm.corrections--newer-z']);
  expect(reader.snapshot().problems.filter((p) => p.reason.includes('conflict'))).toHaveLength(2);
});

it("keeps the reviewer's nanosecond-newer correction and credits Sol, never Opus", async () => {
  const probe = JSON.parse(
    fs.readFileSync(
      path.resolve(import.meta.dir, '../../../fixtures/run-corrections-precision.json'),
      'utf8'
    )
  );
  const root = dirs.make('run-corrections-precision-');
  for (const doc of probe.runs) writeRun(root, doc);
  fs.mkdirSync(path.join(root, 'v1/catalog'), { recursive: true });
  await seedCatalog(root);
  const options = { root: () => root, now: () => NOW };
  const reader = new RunsReader(options);
  const catalog = new CatalogReader(options);
  await Promise.all([reader.scan(), catalog.scan()]);
  const state = reader.snapshot();
  expect(state.runs.map((r) => r.doc.id)).toEqual(['vm.probe--a-newer']);
  expect(state.problems).toHaveLength(1);
  expect(state.problems[0]?.file).toContain('vm.probe--z-older');
  const view = buildScoreboardView({
    root,
    now: NOW,
    timeZone: 'UTC',
    runs: state,
    catalog: catalog.snapshot(),
  });
  const rows = view.uses
    .find((u) => u.id === 'building')
    ?.taskTypes.find((t) => t.id === 'build')?.rows;
  expect(rows).toHaveLength(1);
  expect(rows?.[0]?.model).toBe('openai/gpt-6.1-sol');
  expect(rows?.[0]?.observed?.n).toBe(1);
});

const precisionCases = JSON.parse(
  fs.readFileSync(
    path.resolve(import.meta.dir, '../../../fixtures/run-corrections-precision.json'),
    'utf8'
  )
);
for (const testCase of precisionCases.cases) {
  it(`orders accepted timestamps exactly: ${testCase.name}`, async () => {
    const root = dirs.make('run-corrections-precision-');
    for (const original of precisionCases.runs) {
      const doc = structuredClone(original);
      if (doc.id.endsWith('--z-older')) doc.writtenAt = testCase.older;
      if (doc.id.endsWith('--a-newer')) doc.writtenAt = testCase.newer;
      writeRun(root, doc);
    }
    const reader = new RunsReader({ root: () => root, now: () => NOW });
    await reader.scan();
    expect(reader.snapshot().runs.map((r) => r.doc.id)).toEqual([testCase.keep]);
    expect(reader.snapshot().problems).toHaveLength(1);
    expect(reader.snapshot().problems[0]?.reason).toContain('conflict');
  });
}
