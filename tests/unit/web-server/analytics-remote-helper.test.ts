import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { execFileSync, spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { collectAccountActivity } from '../../../src/web-server/usage/account-activity-collector';

const HELPER = path.resolve(
  import.meta.dir,
  '../../../scripts/analytics-remote/analytics_usage_remote.py'
);
const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');

let HAVE_PYTHON = false;
try {
  execFileSync('python3', ['--version'], { stdio: 'pipe' });
  HAVE_PYTHON = true;
} catch {
  HAVE_PYTHON = false;
}

let home: string;
beforeEach(() => {
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-analytics-remote-'));
});
afterEach(() => fs.rmSync(home, { recursive: true, force: true }));

function ompRecord(model = 'deepseek-v4.1-flash', cost = 0.25) {
  return JSON.stringify({
    id: 'm1',
    timestamp: '2026-10-01T15:05:00Z',
    type: 'message',
    message: {
      role: 'assistant',
      model,
      provider: 'chutes',
      usage: {
        input: 1000,
        output: 200,
        cacheRead: 3000,
        cacheWrite: 400,
        cost: { total: cost },
      },
    },
  });
}

function museRecord() {
  return JSON.stringify({
    schema_version: 1,
    recorded_at: Date.parse('2026-10-01T15:10:00Z'),
    payload: {
      event: {
        kind: 'model_completed',
        model: 'muse-spark-1.3-contributor',
        usage: {
          input_tokens: 500,
          output_tokens: 60,
          cache_read_tokens: 400,
          cache_write_tokens: 5,
        },
      },
    },
  });
}

function writeFixtures(options: { muse?: boolean; zcode?: boolean } = {}) {
  const { muse = true, zcode = true } = options;
  const ompDir = path.join(home, '.omp', 'agent', 'sessions', 'slug');
  fs.mkdirSync(ompDir, { recursive: true });
  fs.writeFileSync(
    path.join(ompDir, '2026-10-01T15-00_uuid.jsonl'),
    `${ompRecord()}\n${JSON.stringify({ type: 'session' })}\n`
  );
  const custom = path.join(home, 'PM-Experiments', 'proj', 'sessions');
  fs.mkdirSync(custom, { recursive: true });
  fs.writeFileSync(path.join(custom, '2026-10-01T15-00_uuid2.jsonl'), `${ompRecord('k3', 1.5)}\n`);
  if (muse) {
    const dir = path.join(
      home,
      '.local',
      'share',
      'muse',
      'sessions',
      '2026',
      '10',
      '01',
      'uuid-9'
    );
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, 'session.jsonl'), `${museRecord()}\n`);
  }
  if (zcode) {
    const dir = path.join(home, '.zcode', 'cli', 'db');
    fs.mkdirSync(dir, { recursive: true });
    execFileSync(
      'python3',
      [
        '-c',
        [
          'import sqlite3,sys',
          'db=sqlite3.connect(sys.argv[1])',
          'db.execute("CREATE TABLE model_usage (model_id TEXT, provider_id TEXT, started_at INTEGER, input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER, cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER)")',
          `db.execute("INSERT INTO model_usage VALUES ('GLM-5.3-Flash','zai',${Date.parse('2026-10-01T15:20:00Z')},10000,200,0,9000,100)")`,
          `db.execute("INSERT INTO model_usage VALUES ('GLM-5.3-Flash','zai',${Date.parse('2026-08-01T00:00:00Z')},777,7,0,0,0)")`,
          'db.commit()',
        ].join(';'),
        path.join(dir, 'db.sqlite'),
      ],
      { stdio: 'pipe' }
    );
  }
}

/** Runs the helper with some of its module constants replaced (caps too large to reach in a fixture). */
function runPatchedHelper(
  request: Record<string, unknown>,
  patch: Record<string, number>
): Record<string, unknown> {
  const code = [
    'import importlib.util,json,sys',
    "spec=importlib.util.spec_from_file_location('helper',sys.argv[1])",
    'module=importlib.util.module_from_spec(spec)',
    'spec.loader.exec_module(module)',
    'for key,value in json.loads(sys.argv[2]).items(): setattr(module,key,value)',
    'module.main()',
  ].join('\n');
  const output = execFileSync('python3', ['-c', code, HELPER, JSON.stringify(patch)], {
    input: JSON.stringify(request),
    encoding: 'utf8',
    timeout: 30_000,
    maxBuffer: 8 * 1024 * 1024,
    env: { ...process.env, HOME: home, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' },
  });
  return JSON.parse(output) as Record<string, unknown>;
}

function sqlite(dbPath: string, statements: string[]): void {
  execFileSync(
    'python3',
    [
      '-c',
      ['import sqlite3,sys', 'db=sqlite3.connect(sys.argv[1])', ...statements, 'db.commit()'].join(
        ';'
      ),
      dbPath,
    ],
    { stdio: 'pipe' }
  );
}

function zcodeInsert(at: string, input: number, read: number): string {
  return `db.execute("INSERT INTO model_usage VALUES ('GLM-5.3-Flash','zai',${Date.parse(at)},${input},10,0,${read},0)")`;
}

function runHelper(request: Record<string, unknown>): Record<string, unknown> {
  const output = execFileSync('python3', [HELPER], {
    input: JSON.stringify(request),
    encoding: 'utf8',
    timeout: 30_000,
    maxBuffer: 8 * 1024 * 1024,
    env: { ...process.env, HOME: home, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' },
  });
  return JSON.parse(output) as Record<string, unknown>;
}

describe.skipIf(!HAVE_PYTHON)('analytics remote helper', () => {
  it('aggregates omp, muse and zcode per model and hour without leaking paths', () => {
    writeFixtures();
    const response = runHelper({ kinds: ['omp', 'muse', 'zcode'], minDateMs: MIN_DATE });
    expect(response.version).toBe(1);
    const kinds = response.kinds as Record<string, { state: string }>;
    expect(kinds.omp.state).toBe('ok');
    expect(kinds.muse.state).toBe('ok');
    expect(kinds.zcode.state).toBe('ok');
    const rows = response.rows as Array<Record<string, unknown>>;
    const byModel = new Map(rows.map((row) => [row.m, row]));
    expect(byModel.get('deepseek-v4.1-flash')).toMatchObject({
      k: 'omp',
      h: '2026-10-01 15:00',
      i: 1000,
      o: 200,
      cr: 3000,
      cw: 400,
      c: 0.25,
      n: 1,
    });
    expect(byModel.get('k3')?.c).toBe(1.5);
    // Muse input includes the cache reads: 500 - 400 uncached.
    expect(byModel.get('muse-spark-1.3-contributor')).toMatchObject({
      k: 'muse',
      i: 100,
      cr: 400,
      n: 1,
    });
    // zcode nets cache reads out of input; the August row is outside the window.
    expect(byModel.get('GLM-5.3-Flash')).toMatchObject({
      k: 'zcode',
      h: '2026-10-01 15:00',
      i: 1000,
      o: 200,
      cr: 9000,
      cw: 100,
      n: 1,
    });
    const serialized = JSON.stringify(response);
    expect(serialized).not.toContain(home);
    expect(serialized).not.toContain('uuid');
  });

  it('returns rows only for new or changed files on later scans', () => {
    writeFixtures();
    const first = runHelper({ kinds: ['omp', 'muse', 'zcode'], minDateMs: MIN_DATE });
    const prints = Object.fromEntries(
      Object.entries(first.kinds as Record<string, { fingerprints: unknown }>).map(
        ([kind, value]) => [kind, value.fingerprints]
      )
    );
    const second = runHelper({
      kinds: ['omp', 'muse', 'zcode'],
      minDateMs: MIN_DATE,
      fingerprints: prints,
    });
    expect((second.rows as unknown[]).length).toBe(0);
    const ompFile = path.join(
      home,
      '.omp',
      'agent',
      'sessions',
      'slug',
      '2026-10-01T15-00_uuid.jsonl'
    );
    fs.appendFileSync(ompFile, `${ompRecord('qwen3.8-max', 0)}\n`);
    const third = runHelper({
      kinds: ['omp', 'muse', 'zcode'],
      minDateMs: MIN_DATE,
      fingerprints: prints,
    });
    const rows = third.rows as Array<Record<string, unknown>>;
    // The changed file re-emits all of its rows under one filekey; the
    // server merge replaces that filekey, so nothing is counted twice.
    expect(rows.length).toBe(2);
    expect(rows.map((row) => row.m).sort()).toEqual(['deepseek-v4.1-flash', 'qwen3.8-max']);
    expect(new Set(rows.map((row) => row.f)).size).toBe(1);
  });

  it('reports not_installed for missing kinds', () => {
    writeFixtures({ muse: false, zcode: false });
    const response = runHelper({ kinds: ['omp', 'muse', 'zcode'], minDateMs: MIN_DATE });
    const kinds = response.kinds as Record<string, { state: string }>;
    expect(kinds.omp.state).toBe('ok');
    expect(kinds.muse.state).toBe('not_installed');
    expect(kinds.zcode.state).toBe('not_installed');
  });

  it('rejects malformed requests without scanning', () => {
    writeFixtures();
    expect(() => runHelper({ kinds: ['cursor'], minDateMs: MIN_DATE })).toThrow();
    expect(() => runHelper({ kinds: ['omp'], minDateMs: MIN_DATE, roots: ['/etc'] })).toThrow();
  });

  it('collects local zcode through the helper with one row per model and hour', async () => {
    writeFixtures();
    const dbPath = path.join(home, '.zcode', 'cli', 'db', 'db.sqlite');
    const data = await collectAccountActivity(
      { kind: 'zcode', dbPath },
      { minDate: MIN_DATE, cacheDir: path.join(home, 'cache') }
    );
    expect(data.eventCount).toBe(1);
    expect(data.hourly).toHaveLength(1);
    expect(data.hourly[0].hour).toBe('2026-10-01 15:00');
    const breakdown = data.hourly[0].modelBreakdowns[0];
    expect(breakdown.modelName).toBe('GLM-5.3-Flash');
    expect(breakdown.inputTokens).toBe(1000);
    expect(breakdown.cacheReadTokens).toBe(9000);
    // A second scan reuses the fingerprint without re-querying rows twice.
    const again = await collectAccountActivity(
      { kind: 'zcode', dbPath },
      { minDate: MIN_DATE, cacheDir: path.join(home, 'cache') }
    );
    expect(again.eventCount).toBe(1);
  });

  it('replaces a changed local zcode database instead of adding it again', async () => {
    writeFixtures();
    const dbPath = path.join(home, '.zcode', 'cli', 'db', 'db.sqlite');
    const scan = () =>
      collectAccountActivity(
        { kind: 'zcode', dbPath },
        { minDate: MIN_DATE, cacheDir: path.join(home, 'cache') }
      );
    const input = (data: Awaited<ReturnType<typeof scan>>) =>
      data.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0);
    expect(input(await scan())).toBe(1000);
    expect(input(await scan())).toBe(1000);
    sqlite(dbPath, [zcodeInsert('2026-10-01T15:40:00Z', 110, 100)]);
    const changed = await scan();
    expect(changed.eventCount).toBe(2);
    expect(input(changed)).toBe(1010);
    const again = await scan();
    expect(again.eventCount).toBe(2);
    expect(input(again)).toBe(1010);
  });

  it('sees zcode rows that are still in the write-ahead log', async () => {
    writeFixtures({ muse: false });
    const dbPath = path.join(home, '.zcode', 'cli', 'db', 'db.sqlite');
    sqlite(dbPath, ["db.execute('PRAGMA journal_mode=WAL')"]);
    const scan = () =>
      collectAccountActivity(
        { kind: 'zcode', dbPath },
        { minDate: MIN_DATE, cacheDir: path.join(home, 'cache') }
      );
    expect((await scan()).eventCount).toBe(1);
    // A writer that keeps its connection open leaves the new row in the WAL:
    // the database file itself does not change.
    const before = fs.statSync(dbPath);
    const writer = spawn(
      'python3',
      [
        '-c',
        [
          'import sqlite3,sys',
          'db=sqlite3.connect(sys.argv[1])',
          "db.execute('PRAGMA wal_autocheckpoint=0')",
          zcodeInsert('2026-10-01T15:45:00Z', 220, 200),
          'db.commit()',
          "sys.stdout.write('ready\\n')",
          'sys.stdout.flush()',
          'sys.stdin.read()',
        ].join('\n'),
        dbPath,
      ],
      { stdio: ['pipe', 'pipe', 'pipe'] }
    );
    try {
      await new Promise<void>((resolve, reject) => {
        writer.stdout.on('data', (chunk: Buffer) => {
          if (chunk.toString().includes('ready')) resolve();
        });
        writer.once('exit', () => reject(new Error('writer exited')));
      });
      const after = fs.statSync(dbPath);
      expect(after.size).toBe(before.size);
      expect(fs.statSync(`${dbPath}-wal`).size).toBeGreaterThan(0);
      const data = await scan();
      expect(data.eventCount).toBe(2);
      expect(data.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(1020);
    } finally {
      writer.stdin.end();
      await new Promise((resolve) => writer.once('close', resolve));
    }
  });

  it('reports an unreadable zcode database as an error and keeps what was read', async () => {
    writeFixtures({ muse: false });
    const dbPath = path.join(home, '.zcode', 'cli', 'db', 'db.sqlite');
    const scan = () =>
      collectAccountActivity(
        { kind: 'zcode', dbPath },
        { minDate: MIN_DATE, cacheDir: path.join(home, 'cache') }
      );
    expect((await scan()).eventCount).toBe(1);
    fs.writeFileSync(dbPath, 'this is not a database file at all, only text'.repeat(200));
    const response = runHelper({ kinds: ['zcode'], minDateMs: MIN_DATE });
    const zcode = (response.kinds as Record<string, { state: string; fingerprints: object }>).zcode;
    expect(zcode.state).toBe('error');
    expect(Object.keys(zcode.fingerprints)).toHaveLength(0);
    const kept = await scan();
    expect(kept.eventCount).toBe(1);
    expect(kept.scan?.complete).toBe(false);
  });

  it('reads every found root when the custom-root search hits its bounds', () => {
    writeFixtures({ muse: false, zcode: false });
    const deep = path.join(home, 'PM-Experiments', 'a', 'b', 'c', 'proj', 'sessions');
    fs.mkdirSync(deep, { recursive: true });
    fs.writeFileSync(path.join(deep, '2026-10-01T15-00_uuid3.jsonl'), `${ompRecord('glm', 0.1)}\n`);
    const response = runPatchedHelper(
      { kinds: ['omp', 'muse', 'zcode'], minDateMs: MIN_DATE },
      { SCAN_MAX_DIRS: 2 }
    );
    // Only discovery is cut: the default root is still read in full.
    expect(response.truncated).toBe(false);
    expect(response.discoveryTruncated).toBe(true);
    const rows = response.rows as Array<Record<string, unknown>>;
    expect(rows.map((row) => row.m)).toContain('deepseek-v4.1-flash');
    expect(rows.map((row) => row.m)).not.toContain('glm');
    const prints = (response.kinds as Record<string, { fingerprints: object }>).omp.fingerprints;
    expect(Object.keys(prints).length).toBeGreaterThan(0);
  });

  it('counts a resumed omp session copied into a second root once', () => {
    writeFixtures({ muse: false, zcode: false });
    const name = '2026-09-25T17-03-32-611Z_01a0d985.jsonl';
    const first = path.join(home, 'PM-Experiments', 'job', 'resume-01', 'sessions');
    const second = path.join(home, 'PM-Experiments', 'job', 'resume-02', 'sessions');
    fs.mkdirSync(first, { recursive: true });
    fs.mkdirSync(second, { recursive: true });
    const original = `${ompRecord('glm-5.3-flash', 0.1)}\n`;
    fs.writeFileSync(path.join(first, name), original);
    fs.writeFileSync(path.join(second, name), `${original}${ompRecord('glm-5.3-flash', 0.2)}\n`);
    const response = runHelper({ kinds: ['omp'], minDateMs: MIN_DATE });
    const glm = (response.rows as Array<Record<string, unknown>>).filter(
      (row) => row.m === 'glm-5.3-flash'
    );
    expect(glm.reduce((sum, row) => sum + (row.n as number), 0)).toBe(2);
    expect(glm.reduce((sum, row) => sum + (row.c as number), 0)).toBeCloseTo(0.3, 9);
  });

  it('never mixes logged and unlogged omp events in one row', () => {
    writeFixtures({ muse: false, zcode: false });
    const file = path.join(
      home,
      '.omp',
      'agent',
      'sessions',
      'slug',
      '2026-10-01T15-00_uuid.jsonl'
    );
    fs.appendFileSync(file, `${ompRecord('deepseek-v4.1-flash', 0)}\n`);
    const response = runHelper({ kinds: ['omp'], minDateMs: MIN_DATE });
    const rows = (response.rows as Array<Record<string, unknown>>).filter(
      (row) => row.m === 'deepseek-v4.1-flash'
    );
    expect(rows).toHaveLength(2);
    expect(rows.map((row) => row.c).sort()).toEqual([0, 0.25]);
  });
});
