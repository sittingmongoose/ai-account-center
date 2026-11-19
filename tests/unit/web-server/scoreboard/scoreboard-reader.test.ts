import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import { execFileSync } from 'child_process';
import fs from 'fs';
import path from 'path';
import {
  nodeReaderFs,
  PERSISTENT_LISTING_ERROR,
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
      // Once, between good scans: the share's rename-over race, not a problem.
      fail = error;
      await runs.scan();
      expect(runs.snapshot().runs).toHaveLength(1);
      expect(runs.snapshot().problems).toEqual([]);
      fail = null;
      await runs.scan();
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

  it('names a listed file whose ENOENT or ESTALE persists past one scan, keeping its copy', async () => {
    for (const make of [
      () => errno('ENOENT'),
      () => errno('ESTALE'),
      () => new Error('Unknown system error -116'),
    ]) {
      const root = dirs.make();
      const file = writeRun(root, runDoc('vm.lead', 'A', { seq: 1 }));
      let now = NOW;
      let failRead = false;
      let failStat = false;
      const flaky: ReaderFs = {
        ...nodeReaderFs,
        stat: async (target) => {
          if (failStat && target === file) throw make();
          return nodeReaderFs.stat(target);
        },
        readFile: async (target, cap) => {
          if (failRead && target === file) throw make();
          return nodeReaderFs.readFile(target, cap);
        },
      };
      const runs = reader(root, { fs: flaky, now: () => now });
      await runs.scan();
      // The listing still names the file; every read of it fails.
      failStat = true;
      now += 60_000;
      await runs.scan();
      expect(runs.snapshot().problems).toEqual([]);
      now += 60_000;
      await runs.scan();
      let state = runs.snapshot();
      expect(state.runs).toHaveLength(1);
      expect(state.problems).toHaveLength(1);
      expect(state.problems[0].file).toContain('vm.lead');
      expect(state.problems[0].reason).toContain('two scans in a row');
      // Since the first failure, not the scan that named it.
      expect(state.problems[0].since).toBe(NOW + 60_000);
      failStat = false;
      await runs.scan();
      expect(runs.snapshot().problems).toEqual([]);

      // The same for a stat that works and a read that does not, on a file that changed.
      fs.writeFileSync(file, JSON.stringify(runDoc('vm.lead', 'A', { seq: 2 })));
      // Same size as before: move the mtime on so the change shows even within one timestamp tick.
      const later = new Date(Date.now() + 10_000);
      fs.utimesSync(file, later, later);
      failRead = true;
      await runs.scan();
      expect(runs.snapshot().problems).toEqual([]);
      await runs.scan();
      state = runs.snapshot();
      expect(state.problems.map((problem) => problem.reason)).toHaveLength(1);
      expect(state.runs[0].doc.seq).toBe(1);
      failRead = false;
      await runs.scan();
      expect(runs.snapshot().problems).toEqual([]);
      expect(runs.snapshot().runs[0].doc.seq).toBe(2);
    }
  });

  it('names a runs folder whose listing fails on two scans in a row, keeping its runs', async () => {
    for (const [folder, make] of [
      ['2026-11-06', () => errno('ESTALE')],
      ['runs', () => errno('ENOENT')],
      ['vm.lead', () => new Error('Unknown system error -116')],
    ] as const) {
      const root = dirs.make();
      writeRun(root, runDoc('vm.lead', 'A', { seq: 1 }));
      let now = NOW;
      let failing = false;
      const flaky: ReaderFs = {
        ...nodeReaderFs,
        readdir: async (dir) => {
          if (failing && path.basename(dir) === folder) throw make();
          return nodeReaderFs.readdir(dir);
        },
      };
      const runs = reader(root, { fs: flaky, now: () => now });
      await runs.scan();
      failing = true;
      now += 60_000;
      await runs.scan();
      expect(runs.snapshot().problems).toEqual([]);
      now += 60_000;
      await runs.scan();
      const state = runs.snapshot();
      expect(state.runs).toHaveLength(1);
      expect(state.problems).toHaveLength(1);
      expect(state.problems[0].file.endsWith(folder)).toBe(true);
      expect(state.problems[0].reason).toBe(PERSISTENT_LISTING_ERROR);
      expect(state.problems[0].since).toBe(NOW + 60_000);
      failing = false;
      await runs.scan();
      expect(runs.snapshot().problems).toEqual([]);
      expect(runs.snapshot().runs).toHaveLength(1);
    }
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

describe('scan limits', () => {
  it('an oversized run file takes nothing from the byte budget, on any scan', async () => {
    const root = dirs.make();
    let now = NOW;
    for (const local of ['A', 'B', 'C', 'D'])
      writeRun(root, runDoc('vm.lead', local), '2026-11-05');
    const big = path.join(root, 'v1', 'writers', 'vm.lead', 'runs', '2026-11-06', 'big.json');
    fs.mkdirSync(path.dirname(big), { recursive: true });
    fs.writeFileSync(big, '{}');
    fs.truncateSync(big, 70 * 1024 * 1024);
    const r = new RunsReader({ root: () => root, now: () => now });
    for (let scan = 0; scan < 3; scan += 1) {
      await r.scan();
      const seen = r.snapshot();
      expect([scan, seen.runs.length]).toEqual([scan, 4]);
      expect(seen.problems.map((p) => p.file.split('/').pop())).toEqual(['big.json']);
      // Past the grace period: a file wrongly left unread would be forgotten by the next look.
      now += 3 * 60_000;
    }
  });

  it('names anything but a folder directly in runs/', async () => {
    const root = dirs.make();
    const file = writeRun(root, runDoc('vm.a', 'R'), '2026-11-05');
    const runs = path.join(root, 'v1', 'writers', 'vm.a', 'runs');
    const elsewhere = dirs.make();
    fs.copyFileSync(file, path.join(elsewhere, 'vm.a--S.json'));
    fs.symlinkSync(elsewhere, path.join(runs, '2026-10-02'));
    fs.writeFileSync(path.join(runs, 'stray.txt'), 'x');
    fs.writeFileSync(path.join(runs, '.hidden'), 'x');
    const r = reader(root);
    await r.scan();
    expect(r.snapshot().runs).toHaveLength(1);
    expect(r.snapshot().problems.map((p) => [p.file.split('/').pop(), p.reason])).toEqual([
      ['2026-10-02', 'is not a folder'],
      ['stray.txt', 'is not a folder'],
    ]);
  });

  it('past the writer limit keeps the writers with the newest run dates, not the newest runs/ folders', async () => {
    const root = dirs.make();
    // vm.a holds the newest run anywhere, but its runs/ folder changed longest ago.
    writeRun(root, runDoc('vm.a', 'R'), '2026-11-09');
    writeRun(root, runDoc('vm.b', 'R'), '2026-11-03');
    writeRun(root, runDoc('vm.c', 'R'), '2026-11-07');
    const stamp = (writer: string, at: number) =>
      fs.utimesSync(path.join(root, 'v1', 'writers', writer, 'runs'), at, at);
    stamp('vm.a', 1000);
    stamp('vm.b', 3000);
    stamp('vm.c', 2000);
    const r = new RunsReader({ root: () => root, now: () => NOW, limits: { writers: 2 } });
    await r.scan();
    expect(
      r
        .snapshot()
        .runs.map((run) => run.doc.writer)
        .sort()
    ).toEqual(['vm.a', 'vm.c']);
    expect(r.snapshot().problems.map((p) => p.file)).toEqual(['limits/run-writers']);
  });

  it('a writer past the writer cap is not listed, and the cap says the not-read count is a lower bound', async () => {
    const root = dirs.make();
    writeRun(root, runDoc('vm.a', 'R'), '2026-11-07');
    writeRun(root, runDoc('vm.b', 'R'), '2026-11-06');
    const listings: string[] = [];
    const counting: ReaderFs = {
      ...nodeReaderFs,
      readdir: (dir) => {
        listings.push(dir);
        return nodeReaderFs.readdir(dir);
      },
    };
    const r = new RunsReader({
      root: () => root,
      now: () => NOW,
      fs: counting,
      limits: { writers: 1 },
    });
    await r.scan();
    const seen = r.snapshot();
    expect(seen.runs.map((run) => run.doc.id)).toEqual(['vm.a--R']);
    expect(seen.skipped).toEqual({ runFiles: 0, writerFolders: 1 });
    const problem = seen.problems.find((p) => p.file === 'limits/run-writers');
    expect(problem?.reason).toBe(
      '1 writer folders past the limit of 1 were not listed (writers rank by their newest run-date folder, which may be empty, so a newer empty date folder can outrank a writer with newer runs), so their run files are not read and not counted: the not-read run file count is a lower bound'
    );
    // The dropped writer's date folders are never opened: ranking needs only its writer and runs/
    // folders, so a share past the cap cannot grow the listing without bound.
    expect(listings.filter((dir) => dir.includes(path.join('writers', 'vm.b')))).toEqual([
      path.join(root, 'v1', 'writers', 'vm.b'),
      path.join(root, 'v1', 'writers', 'vm.b', 'runs'),
    ]);
    // A writer the cap pushes out leaves the read set at once, not after the grace period.
    const both = new RunsReader({ root: () => root, now: () => NOW, limits: { writers: 2 } });
    await both.scan();
    expect(both.snapshot().runs.map((run) => run.doc.id).sort()).toEqual(['vm.a--R', 'vm.b--R']);
    const capped = new RunsReader({ root: () => root, now: () => NOW, limits: { writers: 1 } });
    await capped.scan();
    expect(capped.snapshot().runs.map((run) => run.doc.id)).toEqual(['vm.a--R']);
  });

  it('runs under a folder that cannot be listed stay counted as not read, from the last good listing', async () => {
    const root = dirs.make();
    writeRun(root, runDoc('vm.a', 'A'), '2026-11-07');
    writeRun(root, runDoc('vm.b', 'B'), '2026-11-06');
    let now = NOW;
    let failWriter: string | null = null;
    const flaky: ReaderFs = {
      ...nodeReaderFs,
      readdir: async (dir) => {
        if (failWriter && path.basename(dir) === failWriter) throw errno('EIO');
        return nodeReaderFs.readdir(dir);
      },
    };
    const r = new RunsReader({ root: () => root, now: () => now, fs: flaky, limits: { runs: 1 } });
    await r.scan();
    let seen = r.snapshot();
    expect(seen.runs.map((run) => run.doc.id)).toEqual(['vm.a--A']); // the newest day wins
    expect(seen.skipped.runFiles).toBe(1); // vm.b's file past the run cap
    // The writer folder stops listing: its run stays named and counted, not dropped from the total.
    failWriter = 'vm.b';
    now += 60_000;
    await r.scan();
    seen = r.snapshot();
    expect(seen.skipped.runFiles).toBe(1);
    expect(seen.problems.map((p) => [p.file, p.reason])).toEqual([
      ['writers/vm.b', 'folder could not be listed'],
    ]);
    // A folder whose held copies still score is not also counted as not read.
    failWriter = 'vm.a';
    now += 60_000;
    await r.scan();
    seen = r.snapshot();
    expect(seen.runs.map((run) => run.doc.id).sort()).toEqual(['vm.a--A', 'vm.b--B']);
    expect(seen.skipped.runFiles).toBe(0);
    expect(seen.problems.map((p) => [p.file, p.reason])).toEqual([
      ['writers/vm.a', 'folder could not be listed'],
    ]);
  });

  it('a grace-held copy of a deleted file never cancels a different never-read file (M1)', async () => {
    const root = dirs.make();
    // Run cap 1: A and B share a day; scan 1 reads A, skips B.
    writeRun(root, runDoc('vm.a', 'A'), '2026-11-06');
    writeRun(root, runDoc('vm.a', 'B'), '2026-11-06');
    let now = NOW;
    let failDate: string | null = null;
    const flaky: ReaderFs = {
      ...nodeReaderFs,
      readdir: async (dir) => {
        if (failDate && path.basename(dir) === failDate) throw errno('EIO');
        return nodeReaderFs.readdir(dir);
      },
    };
    const r = new RunsReader({ root: () => root, now: () => now, fs: flaky, limits: { runs: 1 } });
    await r.scan();
    expect(r.snapshot().runs.map((run) => run.doc.id)).toEqual(['vm.a--A']);
    expect(r.snapshot().skipped.runFiles).toBe(1); // B past the cap
    // A is deleted, C arrives on a newer day; scan 2 (within grace) holds A, reads C, skips B.
    fs.rmSync(path.join(root, 'v1', 'writers', 'vm.a', 'runs', '2026-11-06', 'vm.a--A.json'));
    writeRun(root, runDoc('vm.a', 'C'), '2026-11-07');
    now += 60_000;
    await r.scan();
    expect(r.snapshot().runs.map((run) => run.doc.id).sort()).toEqual(['vm.a--A', 'vm.a--C']);
    expect(r.snapshot().skipped.runFiles).toBe(1); // B still skipped
    // The day B lives in stops listing. B still exists and was never read: the grace-held A (a
    // different file, no longer listed) must not subtract from B's count.
    failDate = '2026-11-06';
    now += 60_000;
    await r.scan();
    const seen = r.snapshot();
    expect(seen.skipped.runFiles).toBe(1);
    expect(seen.provisionalRunFiles).toBe(1);
    expect(seen.problems.map((p) => [p.file, p.reason])).toEqual([
      ['writers/vm.a/runs/2026-11-06', 'folder could not be listed'],
    ]);
  });

  it('a writer the cap pushed out keeps its known count for a later failed listing (M1)', async () => {
    const root = dirs.make();
    writeRun(root, runDoc('vm.a', 'A'), '2026-11-07');
    writeRun(root, runDoc('vm.b', 'B'), '2026-11-06');
    writeRun(root, runDoc('vm.b', 'C'), '2026-11-06');
    let now = NOW;
    let failRuns = false;
    const flaky: ReaderFs = {
      ...nodeReaderFs,
      readdir: async (dir) => {
        if (failRuns && dir === path.join(root, 'v1', 'writers', 'vm.b', 'runs')) throw errno('EIO');
        return nodeReaderFs.readdir(dir);
      },
    };
    const r = new RunsReader({ root: () => root, now: () => now, fs: flaky, limits: { writers: 2 } });
    await r.scan();
    expect(r.snapshot().runs).toHaveLength(3);
    // Two newer writers push vm.b past the cap; its files leave at once and are not counted.
    writeRun(root, runDoc('vm.c', 'D'), '2026-11-08');
    writeRun(root, runDoc('vm.d', 'E'), '2026-11-09');
    now += 60_000;
    await r.scan();
    expect(r.snapshot().runs.map((run) => run.doc.id).sort()).toEqual(['vm.c--D', 'vm.d--E']);
    expect(r.snapshot().skipped).toEqual({ runFiles: 0, writerFolders: 2 });
    // vm.b's runs/ folder now fails to list: its two never-read files stay counted from the last
    // good listing, not lost because the cap had pushed the writer out.
    failRuns = true;
    now += 60_000;
    await r.scan();
    const seen = r.snapshot();
    expect(seen.skipped.runFiles).toBe(2);
    expect(seen.provisionalRunFiles).toBe(2);
    expect(seen.problems.map((p) => p.file)).toContain('writers/vm.b/runs');
  });

  it('a failed listing counts a provisional last-known number, even for a file since deleted (M1)', async () => {
    const root = dirs.make();
    writeRun(root, runDoc('vm.a', 'A'), '2026-11-06');
    writeRun(root, runDoc('vm.a', 'B'), '2026-11-06');
    let now = NOW;
    let fail = false;
    const flaky: ReaderFs = {
      ...nodeReaderFs,
      readdir: async (dir) => {
        if (fail && path.basename(dir) === '2026-11-06') throw errno('EIO');
        return nodeReaderFs.readdir(dir);
      },
    };
    const r = new RunsReader({ root: () => root, now: () => now, fs: flaky, limits: { runs: 1 } });
    await r.scan();
    expect(r.snapshot().skipped.runFiles).toBe(1); // B skipped
    // B is deleted, then the folder fails to list: the reader cannot know B is gone, so it keeps
    // the last-known count, flagged provisional (not a current count or a guaranteed lower bound).
    fs.rmSync(path.join(root, 'v1', 'writers', 'vm.a', 'runs', '2026-11-06', 'vm.a--B.json'));
    fail = true;
    now += 60_000;
    await r.scan();
    const seen = r.snapshot();
    expect(seen.skipped.runFiles).toBe(1);
    expect(seen.provisionalRunFiles).toBe(1);
  });

  it('a newer empty date folder outranks a writer with newer runs, and the cap says so (Low 1)', async () => {
    const root = dirs.make();
    writeRun(root, runDoc('vm.a', 'A'), '2026-11-07');
    writeRun(root, runDoc('vm.b', 'B'), '2026-11-06');
    // vm.b's empty Nov 8 folder is newer than vm.a's run day, so the date-folder ranking keeps vm.b.
    fs.mkdirSync(path.join(root, 'v1', 'writers', 'vm.b', 'runs', '2026-11-08'), { recursive: true });
    const r = new RunsReader({ root: () => root, now: () => NOW, limits: { writers: 1 } });
    await r.scan();
    expect(r.snapshot().runs.map((run) => run.doc.id)).toEqual(['vm.b--B']);
    const problem = r.snapshot().problems.find((p) => p.file === 'limits/run-writers');
    expect(problem?.reason).toContain('newest run-date folder, which may be empty');
    expect(problem?.reason).toContain('a newer empty date folder can outrank a writer with newer runs');
  });

  it('names a FIFO or a link in a run folder', async () => {
    const root = dirs.make();
    const file = writeRun(root, runDoc('vm.a', 'R'));
    fs.symlinkSync(file, path.join(path.dirname(file), 'vm.a--link.json'));
    execFileSync('mkfifo', [path.join(path.dirname(file), 'vm.a--pipe.json')]);
    const r = reader(root);
    await r.scan();
    expect(r.snapshot().runs).toHaveLength(1);
    expect(r.snapshot().problems.map((p) => [p.file.split('/').pop(), p.reason])).toEqual([
      ['vm.a--link.json', 'is not a regular file'],
      ['vm.a--pipe.json', 'is not a regular file'],
    ]);
  });

  it('names run files past the limit and keeps the newest days', async () => {
    const root = dirs.make();
    writeRun(root, runDoc('vm.old', 'A'), '2026-01-01');
    writeRun(root, runDoc('vm.old', 'B'), '2026-11-05');
    writeRun(root, runDoc('vm.new', 'C'), '2026-11-06');
    const r = new RunsReader({ root: () => root, now: () => NOW, limits: { runs: 2 } });
    await r.scan();
    const seen = r.snapshot();
    expect(seen.runs.map((run) => run.doc.id).sort()).toEqual(['vm.new--C', 'vm.old--B']);
    expect(seen.problems.map((p) => p.file)).toEqual(['limits/runs']);
    // The cap counts run files, and says so.
    expect(seen.problems[0].reason).toBe(
      '1 run files past the limit of 2 run files or 256 MB were not read (the newest days are kept)'
    );
  });

  it('names writers and bytes past their limits', async () => {
    const root = dirs.make();
    const file = writeRun(root, runDoc('vm.a', 'A'));
    writeRun(root, runDoc('vm.b', 'B'));
    const byWriters = new RunsReader({ root: () => root, now: () => NOW, limits: { writers: 1 } });
    await byWriters.scan();
    expect(byWriters.snapshot().runs).toHaveLength(1);
    expect(byWriters.snapshot().problems.map((p) => p.file)).toEqual(['limits/run-writers']);
    const byBytes = new RunsReader({
      root: () => root,
      now: () => NOW,
      limits: { bytes: fs.statSync(file).size },
    });
    await byBytes.scan();
    expect(byBytes.snapshot().runs).toHaveLength(1);
    expect(byBytes.snapshot().problems.map((p) => p.file)).toEqual(['limits/runs']);
  });
});

describe('catalog', () => {
  it('names a look at a seed that fails, and writes nothing over what it cannot see', async () => {
    const root = dirs.make();
    const target = path.join(root, 'v1', 'catalog');
    fs.mkdirSync(target, { recursive: true });
    const realLstat = fs.promises.lstat;
    const lstat = spyOn(fs.promises, 'lstat').mockImplementation(((file: fs.PathLike) =>
      path.basename(String(file)) === 'models.json'
        ? Promise.reject(Object.assign(new Error('system words'), { code: 'EACCES' }))
        : realLstat(file)) as typeof fs.promises.lstat);
    const failures: Array<{ file: string; code: string }> = [];
    let written: string[];
    try {
      written = await seedCatalog(root, undefined, (failure) => failures.push(failure));
    } finally {
      lstat.mockRestore();
    }
    expect(failures).toEqual([{ file: 'models.json', code: 'EACCES' }]);
    expect(written).not.toContain('models.json');
    expect(written).toContain('uses.json');
    expect(fs.existsSync(path.join(target, 'models.json'))).toBe(false);
  });

  it('never overwrites a file written while the seed was on its way', async () => {
    const root = dirs.make();
    const target = path.join(root, 'v1', 'catalog');
    fs.mkdirSync(target, { recursive: true });
    const real = fs.promises.copyFile;
    const copy = spyOn(fs.promises, 'copyFile').mockImplementation(async (from, to) => {
      // Another machine seeds or edits this file after the look and before the write.
      if (path.basename(String(from)) === 'models.json')
        fs.writeFileSync(path.join(target, 'models.json'), 'edited on the share');
      return real(from, to);
    });
    try {
      const written = await seedCatalog(root);
      expect(written).not.toContain('models.json');
      expect(written).toContain('uses.json');
    } finally {
      copy.mockRestore();
    }
    expect(fs.readFileSync(path.join(target, 'models.json'), 'utf8')).toBe('edited on the share');
    expect(fs.readdirSync(target).filter((name) => name.startsWith('.'))).toEqual([]);
  });

  it('accepts every packaged seed', () => {
    for (const name of CATALOG_FILES) {
      const parsed = parseCatalogFile(name, fs.readFileSync(path.join(packagedCatalogDir(), name)));
      expect(parsed.ok).toBe(true);
    }
  });

  it('packs Fable 5.2 in the models seed', () => {
    const parsed = parseCatalogFile(
      'models.json',
      fs.readFileSync(path.join(packagedCatalogDir(), 'models.json'))
    );
    expect(parsed.ok).toBe(true);
    if (!parsed.ok) return;
    const fable = parsed.value.models.find((model) => model.model === 'anthropic/claude-fable-5-2');
    expect(fable?.label).toBe('Fable 5.2');
    expect(fable?.runtimes).toEqual(['claude-code']);
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
