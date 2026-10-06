import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { execFileSync, spawn } from 'child_process';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { collectAccountActivity } from '../../../src/web-server/usage/account-activity-collector';
import { analyticsSessionKey } from '../../../src/web-server/usage/analytics-session-key';
import { ompSessionIdForFile } from '../../../src/web-server/usage/omp-native-usage-collector';

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
          'db.execute("CREATE TABLE model_usage (model_id TEXT, provider_id TEXT, started_at INTEGER, input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER, cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER, session_id TEXT)")',
          `db.execute("INSERT INTO model_usage VALUES ('GLM-5.3-Flash','zai',${Date.parse('2026-10-01T15:20:00Z')},10000,200,0,9000,100,'sess-9')")`,
          `db.execute("INSERT INTO model_usage VALUES ('GLM-5.3-Flash','zai',${Date.parse('2026-08-01T00:00:00Z')},777,7,0,0,0,'sess-old')")`,
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

function zcodeInsert(at: string, input: number, read: number, session = 'sess-9'): string {
  return `db.execute("INSERT INTO model_usage VALUES ('GLM-5.3-Flash','zai',${Date.parse(at)},${input},10,0,${read},0,'${session}')")`;
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
    // Session aggregates carry each session's published key — derived here, so the log's own id
    // never travels — with first and last event: one per session, model and logged-cost split.
    const srows = response.srows as Array<Record<string, unknown>>;
    expect(srows.length).toBe(4);
    for (const srow of srows) expect(String(srow.s)).toMatch(/^[0-9a-f]{16}$/);
    expect(serialized).not.toContain('uuid');
    expect(serialized).not.toContain('sess-9');
    const bySession = new Map(srows.map((srow) => [srow.s, srow]));
    expect(bySession.get(analyticsSessionKey('omp', '2026-10-01T15-00_uuid'))).toMatchObject({
      k: 'omp',
      m: 'deepseek-v4.1-flash',
      p: 'chutes',
      i: 1000,
      o: 200,
      cr: 3000,
      cw: 400,
      c: 0.25,
      n: 1,
    });
    expect(bySession.get(analyticsSessionKey('omp', '2026-10-01T15-00_uuid2'))).toMatchObject({
      k: 'omp',
      m: 'k3',
    });
    expect(bySession.get(analyticsSessionKey('muse', 'uuid-9'))).toMatchObject({ k: 'muse', n: 1 });
    expect(bySession.get(analyticsSessionKey('zcode', 'sess-9'))).toMatchObject({
      k: 'zcode',
      m: 'GLM-5.3-Flash',
      p: 'zai',
      i: 1000,
      n: 1,
      a: Date.parse('2026-10-01T15:20:00Z'),
      z: Date.parse('2026-10-01T15:20:00Z'),
    });
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
    expect(() =>
      runHelper({ kinds: ['omp'], minDateMs: MIN_DATE, extraRoots: { cursor: ['/x'] } })
    ).toThrow();
    expect(() =>
      runHelper({ kinds: ['omp'], minDateMs: MIN_DATE, extraRoots: { omp: ['relative/path'] } })
    ).toThrow();
    expect(() =>
      runHelper({ kinds: ['omp'], minDateMs: MIN_DATE, extraRoots: { omp: ['/x/../y'] } })
    ).toThrow();
    // Saved extra roots stay capped at 16 per kind; zcode also takes the experiment databases.
    const many = (count: number) => Array.from({ length: count }, (_, i) => `/nope/${i}.sqlite`);
    expect(() =>
      runHelper({ kinds: ['claude'], minDateMs: MIN_DATE, extraRoots: { claude: many(17) } })
    ).toThrow();
    expect(
      runHelper({ kinds: ['zcode'], minDateMs: MIN_DATE, extraRoots: { zcode: many(600) } }).kinds
    ).toBeDefined();
    expect(() =>
      runHelper({ kinds: ['zcode'], minDateMs: MIN_DATE, extraRoots: { zcode: many(2049) } })
    ).toThrow();
    expect(() =>
      runHelper({ kinds: ['zcode'], minDateMs: MIN_DATE, zcodeRecords: 'yes' })
    ).toThrow();
  });

  it('sends zcode rows as keyed records in records mode, never ids, and nothing when unchanged', () => {
    writeFixtures({ muse: false });
    const first = runHelper({ kinds: ['zcode'], minDateMs: MIN_DATE, zcodeRecords: true });
    expect(first.rows).toEqual([]);
    expect(first.srows).toEqual([]);
    const zrecs = first.zrecs as Record<string, unknown[][]>;
    const lists = Object.values(zrecs);
    expect(lists).toHaveLength(1);
    expect(lists[0]).toHaveLength(1);
    const [key, session, model, provider, started, input, output, cacheRead, cacheWrite] =
      lists[0][0];
    expect(key).toMatch(/^[0-9a-f]{16}$/);
    expect(session).toMatch(/^[0-9a-f]{16}$/);
    expect([model, provider, started, input, output, cacheRead, cacheWrite]).toEqual([
      'GLM-5.3-Flash',
      'zai',
      Date.parse('2026-10-01T15:20:00Z'),
      1000,
      200,
      9000,
      100,
    ]);
    expect(JSON.stringify(first)).not.toContain('sess-9');
    // Without records mode the response carries no records field.
    expect(runHelper({ kinds: ['zcode'], minDateMs: MIN_DATE }).zrecs).toBeUndefined();
    // Unchanged since the fingerprints sent: confirmed, no records again.
    const kinds = first.kinds as Record<string, { fingerprints: unknown }>;
    const again = runHelper({
      kinds: ['zcode'],
      minDateMs: MIN_DATE,
      zcodeRecords: true,
      fingerprints: { zcode: kinds.zcode.fingerprints },
    });
    expect(again.zrecs).toEqual({});
    expect((again.kinds as Record<string, { fingerprints: object }>).zcode.fingerprints).toEqual(
      kinds.zcode.fingerprints
    );
  });

  it('scans saved extra roots alongside the defaults without leaking paths', () => {
    writeFixtures({ muse: false, zcode: false });
    const extraOmp = path.join(home, 'extra-omp');
    fs.mkdirSync(path.join(extraOmp, 'slug'), { recursive: true });
    fs.writeFileSync(
      path.join(extraOmp, 'slug', '2026-10-01T15-00_uuid3.jsonl'),
      `${ompRecord('extra-model', 0.75)}\n`
    );
    const extraMuse = path.join(home, 'extra-muse', 'uuid-7');
    fs.mkdirSync(extraMuse, { recursive: true });
    fs.writeFileSync(path.join(extraMuse, 'session.jsonl'), `${museRecord()}\n`);
    const response = runHelper({
      kinds: ['omp', 'muse'],
      minDateMs: MIN_DATE,
      extraRoots: { omp: [extraOmp], muse: [path.join(home, 'extra-muse')] },
    });
    const kinds = response.kinds as Record<string, { state: string }>;
    // The default Muse sessions are missing, but the extra still scans.
    expect(kinds.omp.state).toBe('ok');
    expect(kinds.muse.state).toBe('ok');
    const rows = response.rows as Array<Record<string, unknown>>;
    const byModel = new Map(rows.map((row) => [row.m, row]));
    expect(byModel.get('extra-model')).toMatchObject({ k: 'omp', c: 0.75, n: 1 });
    expect(byModel.get('muse-spark-1.3-contributor')).toMatchObject({ k: 'muse', n: 1 });
    expect(JSON.stringify(response)).not.toContain(home);
  });

  it('scans extra zcode databases and skips missing extras', () => {
    writeFixtures({ zcode: false });
    const extraDb = path.join(home, 'extra', 'db.sqlite');
    fs.mkdirSync(path.dirname(extraDb), { recursive: true });
    sqlite(extraDb, [
      'db.execute("CREATE TABLE model_usage (model_id TEXT, provider_id TEXT, started_at INTEGER, input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER, cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER, session_id TEXT)")',
      zcodeInsert('2026-10-01T15:20:00Z', 10000, 9000),
    ]);
    const response = runHelper({
      kinds: ['zcode'],
      minDateMs: MIN_DATE,
      extraRoots: { zcode: [extraDb, path.join(home, 'missing', 'db.sqlite')] },
    });
    const kinds = response.kinds as Record<string, { state: string }>;
    expect(kinds.zcode.state).toBe('ok');
    const rows = response.rows as Array<Record<string, unknown>>;
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ k: 'zcode', m: 'GLM-5.3-Flash', i: 1000, n: 1 });
  });

  it('groups zcode rows by session when its database has one, and the server counts them', async () => {
    writeFixtures({ muse: false, zcode: false });
    const dbPath = path.join(home, '.zcode', 'cli', 'db', 'db.sqlite');
    fs.mkdirSync(path.dirname(dbPath), { recursive: true });
    sqlite(dbPath, [
      'db.execute("CREATE TABLE model_usage (model_id TEXT, provider_id TEXT, session_id TEXT, started_at INTEGER, input_tokens INTEGER, output_tokens INTEGER, cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER)")',
      `db.execute("INSERT INTO model_usage VALUES ('GLM-5.3-Flash','zai','zs-1',${Date.parse(
        '2026-10-01T15:20:00Z'
      )},10000,200,9000,100)")`,
      `db.execute("INSERT INTO model_usage VALUES ('GLM-5.3-Flash','zai','zs-2',${Date.parse(
        '2026-10-01T15:40:00Z'
      )},2000,20,1000,0)")`,
    ]);
    const response = runHelper({ kinds: ['zcode'], minDateMs: MIN_DATE });
    const rows = response.rows as Array<Record<string, unknown>>;
    // The hourly query keeps its shape: one row per model, provider and hour, cache reads netted
    // out of input ((10000-9000) + (2000-1000)).
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ k: 'zcode', m: 'GLM-5.3-Flash', i: 2000, n: 2 });
    // Sessions ride beside the hours as their own aggregates, keyed on this host: these are the
    // sessions the page never saw.
    const srows = response.srows as Array<Record<string, unknown>>;
    expect(srows.map((srow) => srow.s).sort()).toEqual(
      [analyticsSessionKey('zcode', 'zs-1'), analyticsSessionKey('zcode', 'zs-2')].sort()
    );
    expect(JSON.stringify(response)).not.toContain('zs-1');
    const data = await collectAccountActivity(
      { kind: 'zcode', dbPath },
      { minDate: MIN_DATE, cacheDir: path.join(home, 'cache') }
    );
    expect(data.session.map((session) => session.sessionId).sort()).toEqual(
      [analyticsSessionKey('zcode', 'zs-1'), analyticsSessionKey('zcode', 'zs-2')].sort()
    );
    // Input nets the cache reads out per session and in the hour alike: (10000-9000) + (2000-1000).
    expect(data.hourly).toHaveLength(1);
    expect(data.hourly[0].inputTokens).toBe(2000);
    expect(data.eventCount).toBe(2);
  });

  it('keys an omp session exactly as the server derives its id, advisor files included', () => {
    writeFixtures({ muse: false, zcode: false });
    const slug = path.join(home, '.omp', 'agent', 'sessions', 'slug');
    const sessionDir = path.join(slug, '2026-10-01T16-00_uuid7');
    fs.mkdirSync(sessionDir, { recursive: true });
    const advisor = path.join(sessionDir, '__advisor.jsonl');
    fs.writeFileSync(advisor, `${ompRecord('advisor-model', 0)}\n`);
    const response = runHelper({ kinds: ['omp'], minDateMs: MIN_DATE });
    const srows = response.srows as Array<Record<string, unknown>>;
    const byModel = new Map(srows.map((srow) => [srow.m, srow]));
    // A session file keys by its own stem; an advisor file by the session directory it sits in,
    // both exactly as the server's own reader derives them.
    expect(byModel.get('deepseek-v4.1-flash')?.s).toBe(
      analyticsSessionKey(
        'omp',
        ompSessionIdForFile(path.join(slug, '2026-10-01T15-00_uuid.jsonl'))
      )
    );
    expect(byModel.get('advisor-model')?.s).toBe(
      analyticsSessionKey('omp', ompSessionIdForFile(advisor))
    );
    expect(byModel.get('advisor-model')?.s).toBe(
      analyticsSessionKey('omp', '2026-10-01T16-00_uuid7')
    );
  });

  it('keeps the hourly rows when the session cap is hit, marking the scan partial', () => {
    writeFixtures();
    const capped = runPatchedHelper(
      { kinds: ['omp', 'muse', 'zcode'], minDateMs: MIN_DATE },
      { MAX_SROWS: 1 }
    );
    // Sessions stop at their own cap and say so; the hours they came from are all still read,
    // exactly as an uncapped scan reads them.
    expect(capped.truncated).toBe(true);
    expect((capped.srows as unknown[]).length).toBe(1);
    const full = runHelper({ kinds: ['omp', 'muse', 'zcode'], minDateMs: MIN_DATE });
    expect(capped.rows).toEqual(full.rows);
  });

  it('marks kinds cut short by a persisted row cap partial, never a silent ok', () => {
    writeFixtures();
    const capped = runPatchedHelper(
      { kinds: ['omp', 'muse', 'zcode'], minDateMs: MIN_DATE },
      { MAX_ROWS: 1 }
    );
    // The row cap outlives the kind that hit it: every later kind stops reading
    // early, and each of them says partial instead of claiming a complete scan.
    expect(capped.truncated).toBe(true);
    const kinds = capped.kinds as Record<string, { state: string; partial?: boolean }>;
    expect(kinds.omp.partial).toBe(true);
    expect(kinds.muse.partial).toBe(true);
    expect(kinds.zcode.partial).toBe(true);
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
    // The helper keys the session where it read the database, exactly as the file readers do,
    // so the cached rows and the page hold a digest and never the log's own id.
    expect(data.session).toHaveLength(1);
    expect(data.session[0].sessionId).toBe(analyticsSessionKey('zcode', 'sess-9'));
    expect(data.session[0].target).toBe('zcode');
    expect(data.session[0].firstActivity).toBe('2026-10-01T15:20:00.000Z');
    expect(data.session[0].lastActivity).toBe('2026-10-01T15:20:00.000Z');
    // A second scan reuses the fingerprint without re-querying rows twice.
    const again = await collectAccountActivity(
      { kind: 'zcode', dbPath },
      { minDate: MIN_DATE, cacheDir: path.join(home, 'cache') }
    );
    expect(again.eventCount).toBe(1);
    expect(again.session).toHaveLength(1);
    expect(again.session[0].sessionId).toBe(analyticsSessionKey('zcode', 'sess-9'));
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

  it('reads claude projects like the local parser without leaking content', () => {
    const dir = path.join(home, '.claude', 'projects', 'proj1');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(
      path.join(dir, 'sess.jsonl'),
      [
        JSON.stringify({
          type: 'assistant',
          message: {
            model: 'claude-haiku-4-5',
            usage: {
              input_tokens: 100,
              output_tokens: 50,
              cache_read_input_tokens: 1000,
              cache_creation_input_tokens: 10,
            },
          },
          timestamp: '2026-10-01T15:05:00Z',
          sessionId: 's1',
          cwd: '/secret/project',
        }),
        JSON.stringify({ type: 'user', message: { content: 'hidden prompt' } }),
        JSON.stringify({
          type: 'assistant',
          message: { model: 'claude-haiku-4-5', usage: { input_tokens: 5 } },
          timestamp: '2026-10-01T15:06:00Z',
        }),
      ].join('\n')
    );
    const response = runHelper({ kinds: ['claude'], minDateMs: MIN_DATE });
    const kinds = response.kinds as Record<string, { state: string }>;
    expect(kinds.claude.state).toBe('ok');
    const rows = response.rows as Array<Record<string, unknown>>;
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      k: 'claude',
      m: 'claude-haiku-4-5',
      h: '2026-10-01 15:00',
      i: 100,
      o: 50,
      cr: 1000,
      cw: 10,
      c: 0,
      n: 1,
    });
    expect(rows[0].p).toBeUndefined();
    const srows = response.srows as Array<Record<string, unknown>>;
    expect(srows).toHaveLength(1);
    expect(srows[0]).toMatchObject({
      k: 'claude',
      // The record's own id is keyed on this host: the digest travels, the id does not.
      s: analyticsSessionKey('claude', 's1'),
      m: 'claude-haiku-4-5',
      i: 100,
      n: 1,
      a: Date.parse('2026-10-01T15:05:00Z'),
      z: Date.parse('2026-10-01T15:05:00Z'),
    });
    expect(JSON.stringify(srows)).not.toContain('"s1"');
    const serialized = JSON.stringify(response);
    expect(serialized).not.toContain(home);
    expect(serialized).not.toContain('secret');
    expect(serialized).not.toContain('hidden prompt');
    // Missing projects read as not installed.
    const missing = runHelper({ kinds: ['codex'], minDateMs: MIN_DATE });
    expect((missing.kinds as Record<string, { state: string }>).codex.state).toBe('not_installed');
  });

  it('counts one multi-line claude response once, keeping the last usage', () => {
    const dir = path.join(home, '.claude', 'projects', 'proj1');
    fs.mkdirSync(dir, { recursive: true });
    const line = (output: number) =>
      JSON.stringify({
        type: 'assistant',
        uuid: `u-${output}`,
        requestId: 'r1',
        sessionId: 's1',
        timestamp: '2026-10-01T15:05:00Z',
        message: {
          id: 'm1',
          model: 'claude-haiku-4-5',
          usage: { input_tokens: 100, output_tokens: output },
        },
      });
    fs.writeFileSync(path.join(dir, 'sess.jsonl'), [line(10), line(20), line(30)].join('\n'));
    const response = runHelper({ kinds: ['claude'], minDateMs: MIN_DATE });
    const rows = response.rows as Array<Record<string, unknown>>;
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ o: 30, n: 1 });
  });

  it('discovers custom and depth-7 omp roots but skips marked sandbox trees', () => {
    const record = (model: string) =>
      JSON.stringify({
        id: 'm1',
        timestamp: '2026-10-01T15:05:00Z',
        type: 'message',
        message: {
          role: 'assistant',
          model,
          usage: { input: 10, output: 1, cacheRead: 0, cacheWrite: 0 },
        },
      });
    const worker = path.join(home, 'PM-Experiments', 'worktrees', 'omp', 'fw4-n4o-sessions');
    fs.mkdirSync(worker, { recursive: true });
    fs.writeFileSync(
      path.join(worker, '2026-10-01T15-00_uuid.jsonl'),
      `${record('model-worker')}\n`
    );
    const deep = path.join(
      home,
      'PM-Experiments',
      'exp',
      'runs',
      'r1',
      'stage',
      'jobs',
      'j1',
      'sessions'
    );
    fs.mkdirSync(deep, { recursive: true });
    fs.writeFileSync(path.join(deep, '2026-10-01T15-00_uuid.jsonl'), `${record('model-deep')}\n`);
    const data = path.join(home, 'PM-Experiments', 'worktrees', 'omp', 'fw4-t9x-run', 'data');
    const sandbox = path.join(data, 'omp', 'sessions');
    fs.mkdirSync(sandbox, { recursive: true });
    fs.writeFileSync(
      path.join(sandbox, '2026-10-01T15-00_uuid.jsonl'),
      `${record('model-sandbox')}\n`
    );
    fs.writeFileSync(path.join(data, '.aac-synthetic'), 'synthetic\n');
    const response = runHelper({ kinds: ['omp'], minDateMs: MIN_DATE });
    const rows = response.rows as Array<Record<string, unknown>>;
    const models = rows.map((row) => row.m).sort();
    expect(models).toEqual(['model-deep', 'model-worker']);
  });

  it('differences codex rollout counters and skips cliproxy sessions', () => {
    const dir = path.join(home, '.codex', 'sessions');
    fs.mkdirSync(dir, { recursive: true });
    const token = (total: object, last: object) => ({
      timestamp: '2026-10-01T15:05:00Z',
      type: 'event_msg',
      payload: {
        type: 'token_count',
        info: { total_token_usage: total, last_token_usage: last },
      },
    });
    fs.writeFileSync(
      path.join(dir, 'rollout-2026-10-01.jsonl'),
      [
        JSON.stringify({
          timestamp: '2026-10-01T15:00:00Z',
          type: 'session_meta',
          payload: { id: 'cx1', cwd: '/secret', cli_version: '1.0', model_provider: 'openai' },
        }),
        JSON.stringify({
          timestamp: '2026-10-01T15:01:00Z',
          type: 'turn_context',
          payload: { model: 'gpt-5', cwd: '/secret' },
        }),
        JSON.stringify(
          token(
            { input_tokens: 1000, cached_input_tokens: 100, output_tokens: 200 },
            { input_tokens: 1000, cached_input_tokens: 100, output_tokens: 200 }
          )
        ),
        JSON.stringify(
          token(
            { input_tokens: 1500, cached_input_tokens: 100, output_tokens: 260 },
            { input_tokens: 500, output_tokens: 60 }
          )
        ),
        JSON.stringify({
          timestamp: '2026-10-01T15:06:00Z',
          type: 'response_item',
          payload: { text: 'hidden prompt' },
        }),
      ].join('\n')
    );
    fs.writeFileSync(
      path.join(dir, 'rollout-cliproxy.jsonl'),
      [
        JSON.stringify({
          timestamp: '2026-10-01T15:00:00Z',
          type: 'session_meta',
          payload: { id: 'cx2', model_provider: 'cliproxy' },
        }),
        JSON.stringify(
          token(
            { input_tokens: 999, output_tokens: 999 },
            { input_tokens: 999, output_tokens: 999 }
          )
        ),
      ].join('\n')
    );
    const response = runHelper({ kinds: ['codex'], minDateMs: MIN_DATE });
    expect((response.kinds as Record<string, { state: string }>).codex.state).toBe('ok');
    const rows = response.rows as Array<Record<string, unknown>>;
    expect(rows).toHaveLength(1);
    // First event takes last_token_usage, the second the counter delta.
    expect(rows[0]).toMatchObject({
      k: 'codex',
      m: 'gpt-5',
      h: '2026-10-01 15:00',
      i: 1400,
      o: 260,
      cr: 100,
      cw: 0,
      c: 0,
      n: 2,
    });
    const srows = response.srows as Array<Record<string, unknown>>;
    expect(srows).toHaveLength(1);
    expect(srows[0]).toMatchObject({
      k: 'codex',
      s: analyticsSessionKey('codex', 'cx1'),
      m: 'gpt-5',
      i: 1400,
      n: 2,
    });
    expect(JSON.stringify(srows)).not.toContain('cx1');
    const serialized = JSON.stringify(response);
    expect(serialized).not.toContain(home);
    expect(serialized).not.toContain('secret');
    expect(serialized).not.toContain('hidden prompt');
  });

  it('keeps hourly rows when the zcode database has no session ids', () => {
    writeFixtures({ zcode: false });
    const dir = path.join(home, '.zcode', 'cli', 'db');
    fs.mkdirSync(dir, { recursive: true });
    sqlite(path.join(dir, 'db.sqlite'), [
      'db.execute("CREATE TABLE model_usage (model_id TEXT, provider_id TEXT, started_at INTEGER, input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER, cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER)")',
      `db.execute("INSERT INTO model_usage VALUES ('GLM-5.3-Flash','zai',${Date.parse('2026-10-01T15:20:00Z')},10000,200,0,9000,100)")`,
    ]);
    const response = runHelper({ kinds: ['zcode'], minDateMs: MIN_DATE });
    const kinds = response.kinds as Record<string, { state: string }>;
    expect(kinds.zcode.state).toBe('ok');
    expect((response.rows as unknown[]).length).toBe(1);
    expect((response.srows as unknown[]).length).toBe(0);
  });

  it('scans extra claude projects and codex homes alongside the defaults', () => {
    const extraProjects = path.join(home, 'extra-claude-projects');
    fs.mkdirSync(extraProjects, { recursive: true });
    fs.writeFileSync(
      path.join(extraProjects, 's.jsonl'),
      `${JSON.stringify({
        type: 'assistant',
        message: { model: 'extra-claude-model', usage: { input_tokens: 7, output_tokens: 8 } },
        timestamp: '2026-10-01T15:05:00Z',
      })}\n`
    );
    const extraHome = path.join(home, 'extra-codex');
    fs.mkdirSync(path.join(extraHome, 'sessions'), { recursive: true });
    fs.writeFileSync(
      path.join(extraHome, 'sessions', 'rollout-x.jsonl'),
      [
        JSON.stringify({
          timestamp: '2026-10-01T15:00:00Z',
          type: 'session_meta',
          payload: { id: 'e1' },
        }),
        JSON.stringify({
          timestamp: '2026-10-01T15:01:00Z',
          type: 'turn_context',
          payload: { model: 'extra-codex-model' },
        }),
        JSON.stringify({
          timestamp: '2026-10-01T15:02:00Z',
          type: 'event_msg',
          payload: {
            type: 'token_count',
            info: {
              total_token_usage: { input_tokens: 11, output_tokens: 12 },
              last_token_usage: { input_tokens: 11, output_tokens: 12 },
            },
          },
        }),
      ].join('\n')
    );
    const response = runHelper({
      kinds: ['claude', 'codex'],
      minDateMs: MIN_DATE,
      extraRoots: { claude: [extraProjects], codex: [extraHome] },
    });
    const kinds = response.kinds as Record<string, { state: string }>;
    // The defaults are missing, but the extras still scan.
    expect(kinds.claude.state).toBe('ok');
    expect(kinds.codex.state).toBe('ok');
    const rows = response.rows as Array<Record<string, unknown>>;
    const byModel = new Map(rows.map((row) => [row.m, row]));
    expect(byModel.get('extra-claude-model')).toMatchObject({ k: 'claude', i: 7, o: 8, n: 1 });
    expect(byModel.get('extra-codex-model')).toMatchObject({ k: 'codex', i: 11, o: 12, n: 1 });
    expect(JSON.stringify(response)).not.toContain(home);
  });

  // The helper confirms files in visit order, so the fingerprint key order is the visit order.
  function filekey(kind: string, file: string): string {
    return createHash('sha256')
      .update(kind, 'utf8')
      .update(Buffer.from([0]))
      .update(path.resolve(file), 'utf8')
      .digest('hex');
  }

  it('visits omp session files newest first so a cut scan banks the newest progress', () => {
    const dir = path.join(home, '.omp', 'agent', 'sessions', 'slug');
    fs.mkdirSync(dir, { recursive: true });
    const files = [
      '2026-09-01T10-00_old.jsonl',
      '2026-09-15T10-00_mid.jsonl',
      '2026-10-01T10-00_new.jsonl',
    ];
    for (const name of files) fs.writeFileSync(path.join(dir, name), `${ompRecord()}\n`);
    // Alphabetical order is old, mid, new; stamp the reverse so order must come from mtime.
    const atime = new Date('2026-10-02T00:00:00Z');
    fs.utimesSync(path.join(dir, files[0]), atime, new Date('2026-10-03T00:00:00Z'));
    fs.utimesSync(path.join(dir, files[1]), atime, new Date('2026-10-02T00:00:00Z'));
    fs.utimesSync(path.join(dir, files[2]), atime, new Date('2026-10-01T00:00:00Z'));
    const response = runHelper({ kinds: ['omp'], minDateMs: MIN_DATE });
    const prints = (response.kinds as Record<string, { fingerprints: Record<string, unknown> }>).omp
      .fingerprints;
    expect(Object.keys(prints)).toEqual([
      filekey('omp', path.join(dir, files[0])),
      filekey('omp', path.join(dir, files[1])),
      filekey('omp', path.join(dir, files[2])),
    ]);
  });

  it('visits claude project files newest first', () => {
    const dir = path.join(home, '.claude', 'projects', 'proj1');
    fs.mkdirSync(dir, { recursive: true });
    const record = JSON.stringify({
      type: 'assistant',
      message: { model: 'claude-haiku-4-5', usage: { input_tokens: 3 } },
      timestamp: '2026-10-01T15:05:00Z',
    });
    for (const name of ['a.jsonl', 'b.jsonl', 'c.jsonl'])
      fs.writeFileSync(path.join(dir, name), `${record}\n`);
    const atime = new Date('2026-10-02T00:00:00Z');
    fs.utimesSync(path.join(dir, 'a.jsonl'), atime, new Date('2026-10-01T00:00:00Z'));
    fs.utimesSync(path.join(dir, 'b.jsonl'), atime, new Date('2026-10-03T00:00:00Z'));
    fs.utimesSync(path.join(dir, 'c.jsonl'), atime, new Date('2026-10-02T00:00:00Z'));
    const response = runHelper({ kinds: ['claude'], minDateMs: MIN_DATE });
    const prints = (response.kinds as Record<string, { fingerprints: Record<string, unknown> }>)
      .claude.fingerprints;
    expect(Object.keys(prints)).toEqual([
      filekey('claude', path.join(dir, 'b.jsonl')),
      filekey('claude', path.join(dir, 'c.jsonl')),
      filekey('claude', path.join(dir, 'a.jsonl')),
    ]);
  });

  it('accepts an incremental request holding thousands of fingerprints', () => {
    writeFixtures();
    // Six thousand prints are ~1.5 MB, over the old 1 MB request cap: the Mac
    // holds thousands of session files, and every one of its scans carries them.
    const prints: Record<string, unknown> = {};
    for (let index = 0; index < 6000; index++)
      prints[createHash('sha256').update(`file-${index}`).digest('hex')] = {
        size: 100 + index,
        mtimeMs: MIN_DATE + index,
        head: createHash('sha256').update(`head-${index}`).digest('hex'),
        tail: createHash('sha256').update(`tail-${index}`).digest('hex'),
      };
    const request = JSON.stringify({
      kinds: ['omp', 'muse', 'zcode'],
      minDateMs: MIN_DATE,
      fingerprints: { omp: prints, muse: {}, zcode: {} },
    });
    expect(request.length).toBeGreaterThan(1024 * 1024);
    const output = execFileSync('python3', [HELPER], {
      input: request,
      encoding: 'utf8',
      timeout: 30_000,
      maxBuffer: 8 * 1024 * 1024,
      env: { ...process.env, HOME: home, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' },
    });
    const response = JSON.parse(output) as Record<string, unknown>;
    expect(response.version).toBe(1);
  });
});

describe.skipIf(!HAVE_PYTHON)('analytics remote helper: T3 Code homes', () => {
  const filekey = (kind: string, file: string): string =>
    createHash('sha256')
      .update(kind, 'utf8')
      .update(Buffer.from([0]))
      .update(path.resolve(file), 'utf8')
      .digest('hex');
  const write = (relative: string, records: unknown[]): string => {
    const file = path.join(home, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, records.map((record) => JSON.stringify(record)).join('\n') + '\n');
    return file;
  };
  const link = (target: string, relative: string): void => {
    const at = path.join(home, relative);
    fs.mkdirSync(path.dirname(at), { recursive: true });
    fs.symlinkSync(path.join(home, target), at);
  };
  const claudeLine = (mid: string, output: number, session = 's1') => ({
    type: 'assistant',
    uuid: `u-${mid}-${output}`,
    requestId: `r-${mid}`,
    sessionId: session,
    timestamp: '2026-10-01T15:05:00Z',
    message: {
      id: mid,
      model: 'claude-opus-5-5',
      usage: { input_tokens: 10, output_tokens: output, cache_read_input_tokens: 100 },
    },
  });
  const ID_A = '01a0f2a8-a754-7233-903b-569c995cf226';
  const ID_NEW = '03c2f2a8-a754-7233-903b-569c995cf228';
  const rollout = (id: string, input: number, output: number, at: string) => [
    { timestamp: at, type: 'session_meta', payload: { id, model_provider: 'openai' } },
    { timestamp: at, type: 'turn_context', payload: { model: 'gpt-6-sol' } },
    {
      timestamp: at,
      type: 'event_msg',
      payload: {
        type: 'token_count',
        info: { total_token_usage: { input_tokens: input, output_tokens: output } },
      },
    },
  ];
  const totals = (response: Record<string, unknown>, kind: string) => {
    const rows = (response.rows as Array<Record<string, number | string>>).filter(
      (row) => row.k === kind
    );
    return {
      n: rows.reduce((sum, row) => sum + (row.n as number), 0),
      i: rows.reduce((sum, row) => sum + (row.i as number), 0),
      o: rows.reduce((sum, row) => sum + (row.o as number), 0),
    };
  };

  it('counts each T3 Claude transcript once and re-reads a copy when its default changes', () => {
    write('.claude/projects/p/s1.jsonl', [claudeLine('m1', 10), claudeLine('m1', 20)]);
    const single = runHelper({ kinds: ['claude'], minDateMs: MIN_DATE });
    expect(totals(single, 'claude')).toEqual({ n: 1, i: 10, o: 20 });
    // alpha: a new session plus a non-identical copy of s1; beta: a copy of alpha's session in a
    // subagent folder; gamma: its projects folder links to the default root.
    write('.claude-t3/alpha/projects/p/s2.jsonl', [
      claudeLine('m2', 5, 's2'),
      claudeLine('m2', 7, 's2'),
    ]);
    const copy = write('.claude-t3/alpha/projects/p/s1.jsonl', [claudeLine('m1', 20)]);
    write('.claude-t3/beta/projects/q/x/subagents/s2.jsonl', [claudeLine('m2', 7, 's2')]);
    fs.mkdirSync(path.join(home, '.claude-t3', 'gamma'), { recursive: true });
    link('.claude/projects', '.claude-t3/gamma/projects');
    // A settings home outside ~/.claude-t3.
    write('elsewhere/claude-x/projects/r/s3.jsonl', [claudeLine('m3', 9, 's3')]);
    fs.mkdirSync(path.join(home, '.t3', 'userdata'), { recursive: true });
    fs.writeFileSync(
      path.join(home, '.t3', 'userdata', 'settings.json'),
      JSON.stringify({
        providerInstances: {
          x: { driver: 'claudeAgent', config: { homePath: '~/elsewhere/claude-x' } },
        },
      })
    );
    const first = runHelper({ kinds: ['claude'], minDateMs: MIN_DATE });
    expect((first.kinds as Record<string, { state: string }>).claude.state).toBe('ok');
    expect(totals(first, 'claude')).toEqual({ n: 3, i: 30, o: 36 });
    expect(JSON.stringify(first)).not.toContain(home);
    // A new response in the default s1 changes the T3 copy's print, so the copy is read again
    // (and still adds nothing); unchanged T3 files are not.
    const prints = (first.kinds as Record<string, { fingerprints: Record<string, unknown> }>).claude
      .fingerprints;
    fs.appendFileSync(
      path.join(home, '.claude/projects/p/s1.jsonl'),
      JSON.stringify(claudeLine('m4', 3)) + '\n'
    );
    const second = runHelper({
      kinds: ['claude'],
      minDateMs: MIN_DATE,
      fingerprints: { claude: prints },
    });
    const rowsByFile = new Set((second.rows as Array<{ f: string }>).map((row) => row.f));
    expect(rowsByFile.has(filekey('claude', path.join(home, '.claude/projects/p/s1.jsonl')))).toBe(
      true
    );
    expect(rowsByFile.size).toBe(1);
    const copyKey = filekey('claude', fs.realpathSync(copy));
    const after = (second.kinds as Record<string, { fingerprints: Record<string, unknown> }>).claude
      .fingerprints;
    expect(after[copyKey]).toBeDefined();
    expect(after[copyKey]).not.toEqual(prints[copyKey]);
  });

  it('adds nothing for Codex shadow homes linked to ~/.codex/sessions: totals equal the single home', () => {
    const day = '.codex/sessions/2026/10/01';
    write(
      `${day}/rollout-2026-10-01T14-00-00-${ID_A}.jsonl`,
      rollout(ID_A, 100, 10, '2026-10-01T14:00:00Z')
    );
    const single = runHelper({ kinds: ['codex'], minDateMs: MIN_DATE });
    expect(totals(single, 'codex')).toEqual({ n: 1, i: 100, o: 10 });
    for (const account of ['alpha', 'beta', 'gamma'])
      link('.codex/sessions', `.codex-t3/${account}/sessions`);
    link('.codex/sessions', '.t3/userdata/providers/codex/codex_1/shadow/sessions');
    const linked = runHelper({ kinds: ['codex'], minDateMs: MIN_DATE });
    expect(totals(linked, 'codex')).toEqual(totals(single, 'codex'));
    // A real shadow folder of links, a hard link, a byte copy and a link loop, plus a new session.
    const shadow = '.codex-t3/delta/sessions';
    link('.codex/sessions/2026', `${shadow}/2026`);
    const original = path.join(home, `${day}/rollout-2026-10-01T14-00-00-${ID_A}.jsonl`);
    fs.mkdirSync(path.join(home, shadow, 'kept'), { recursive: true });
    fs.linkSync(original, path.join(home, shadow, 'kept', path.basename(original)));
    fs.copyFileSync(original, path.join(home, shadow, 'kept', `rollout-copy-${ID_A}.jsonl`));
    link(shadow, `${shadow}/kept/loop`);
    write(
      `${shadow}/own/rollout-2026-10-01T16-00-00-${ID_NEW}.jsonl`,
      rollout(ID_NEW, 500, 50, '2026-10-01T16:00:00Z')
    );
    const mixed = runHelper({ kinds: ['codex'], minDateMs: MIN_DATE });
    expect(mixed.truncated).toBe(false);
    expect(totals(mixed, 'codex')).toEqual({ n: 2, i: 600, o: 60 });
  });

  it('never follows links outside T3 homes: a linked folder under a default root is not read', () => {
    write('.claude/projects/p/s1.jsonl', [claudeLine('m1', 10)]);
    write('outside/p/s9.jsonl', [claudeLine('m9', 99)]);
    link('outside', '.claude/projects/linked-outside');
    const response = runHelper({ kinds: ['claude'], minDateMs: MIN_DATE });
    expect(totals(response, 'claude')).toEqual({ n: 1, i: 10, o: 10 });
  });

  it('follows a link inside a T3 root once, and never a link out of it', () => {
    write('.claude/projects/p/s1.jsonl', [claudeLine('m1', 10)]);
    write('.claude-t3/alpha/projects/p/s2.jsonl', [claudeLine('m2', 5, 's2')]);
    write('.claude-t3/alpha/projects/kept/s3.jsonl', [claudeLine('m3', 7, 's3')]);
    // Inside: a relative and an absolute link to the same folder, a file link and a loop.
    fs.symlinkSync('../kept', path.join(home, '.claude-t3/alpha/projects/p/kept-rel'));
    link('.claude-t3/alpha/projects/kept', '.claude-t3/alpha/projects/kept-abs');
    link('.claude-t3/alpha/projects/kept/s3.jsonl', '.claude-t3/alpha/projects/file-link.jsonl');
    link('.claude-t3/alpha/projects', '.claude-t3/alpha/projects/kept/loop');
    // Outside: a folder beside the home (like a NAS mount), ~/PM-Experiments, a sibling account,
    // a relative link up and out, and a file link out.
    const outside = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-t3-outside-'));
    try {
      fs.writeFileSync(path.join(outside, 'o.jsonl'), JSON.stringify(claudeLine('m9', 99)) + '\n');
      fs.symlinkSync(outside, path.join(home, '.claude-t3/alpha/projects/to-mount'));
      write('PM-Experiments/exp/sessions/x.jsonl', [claudeLine('m8', 88)]);
      link('PM-Experiments/exp', '.claude-t3/alpha/projects/to-experiments');
      fs.symlinkSync('../../../PM-Experiments', path.join(home, '.claude-t3/alpha/projects/up'));
      link('PM-Experiments/exp/sessions/x.jsonl', '.claude-t3/alpha/projects/p/file-out.jsonl');
      write('.claude-t3/beta/projects/q/t.jsonl', [claudeLine('m4', 3, 's4')]);
      link('.claude-t3/beta/projects', '.claude-t3/alpha/projects/to-beta');
      const response = runHelper({ kinds: ['claude'], minDateMs: MIN_DATE });
      expect(response.truncated).toBe(false);
      // s1 (default) + s2 + s3 once (alpha) + t (beta, as its own root) = 4; nothing from outside.
      expect(totals(response, 'claude')).toEqual({ n: 4, i: 40, o: 25 });
    } finally {
      fs.rmSync(outside, { recursive: true, force: true });
    }
  });

  it('ignores a deeply nested T3 settings file instead of failing every tool', () => {
    write('.claude/projects/p/s1.jsonl', [claudeLine('m1', 10)]);
    writeFixtures();
    fs.mkdirSync(path.join(home, '.t3', 'userdata'), { recursive: true });
    // About 400 KB, under the 1 MB settings cap: json.loads raises RecursionError on it.
    const depth = 200_000;
    fs.writeFileSync(
      path.join(home, '.t3', 'userdata', 'settings.json'),
      `{"providerInstances":${'['.repeat(depth)}${']'.repeat(depth)}}`
    );
    const response = runHelper({ kinds: ['claude', 'codex', 'omp'], minDateMs: MIN_DATE });
    const kinds = response.kinds as Record<string, { state: string }>;
    expect(kinds.claude.state).toBe('ok');
    expect(kinds.codex.state).toBe('not_installed');
    expect(kinds.omp.state).toBe('ok');
    expect(totals(response, 'claude')).toEqual({ n: 1, i: 10, o: 10 });
  });
});
