/**
 * Antigravity in the packaged analytics helper: a port of T3 Code's reader
 * (apps/server/src/usage/antigravityUsageReader.ts, tag v0.0.46-nightly.20261006.2735). The databases here are
 * synthetic, built in T3's protobuf layout; no real conversation data.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { Database } from 'bun:sqlite';
import { execFileSync } from 'child_process';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { collectAccountActivity } from '../../../src/web-server/usage/account-activity-collector';
import { analyticsSessionKey } from '../../../src/web-server/usage/analytics-session-key';
import {
  antigravityUsagePresent,
  resolveAntigravityRoots,
} from '../../../src/web-server/usage/antigravity-native-usage-collector';
import { parseAnalyticsRemoteResponse } from '../../../src/web-server/services/analytics-remote-transport';

const HELPER = path.resolve(
  import.meta.dir,
  '../../../scripts/analytics-remote/analytics_usage_remote.py'
);

let HAVE_PYTHON = false;
try {
  execFileSync('python3', ['--version'], { stdio: 'pipe' });
  HAVE_PYTHON = true;
} catch {
  HAVE_PYTHON = false;
}

// T3's test encoders (usageTranscriptReader.test.ts).
function protoNumber(field: number, value: number): number[] {
  const varint = (number: number) => {
    const bytes: number[] = [];
    do {
      const byte = number % 128;
      number = Math.floor(number / 128);
      bytes.push(byte + (number > 0 ? 128 : 0));
    } while (number > 0);
    return bytes;
  };
  return [...varint(field * 8), ...varint(value)];
}
function protoBytes(field: number, bytes: readonly number[]): number[] {
  const encoded = protoNumber(field, bytes.length);
  encoded[0] = encoded[0] + 2;
  return [...encoded, ...bytes];
}
function protoText(field: number, value: string): number[] {
  return protoBytes(field, [...Buffer.from(value)]);
}

let home: string;
beforeEach(() => {
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-agy-usage-'));
});
afterEach(() => fs.rmSync(home, { recursive: true, force: true }));

const CONVERSATIONS = path.join('.gemini', 'antigravity-cli', 'conversations');

/** A conversation database with gen_metadata and steps (or the tables given). */
function conversation(
  relative: string,
  rows: { generations?: number[][]; steps?: number[][]; trajectory?: number[] },
  tables: Array<'gen_metadata' | 'steps'> = ['gen_metadata', 'steps']
): string {
  const file = path.join(home, relative);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const db = new Database(file);
  try {
    if (tables.includes('gen_metadata'))
      db.run('CREATE TABLE gen_metadata (idx INTEGER, data BLOB)');
    if (tables.includes('steps')) db.run('CREATE TABLE steps (idx INTEGER, metadata BLOB)');
    for (const [idx, data] of (rows.generations ?? []).entries())
      db.query('INSERT INTO gen_metadata VALUES (?, ?)').run(idx, new Uint8Array(data));
    for (const [idx, data] of (rows.steps ?? []).entries())
      db.query('INSERT INTO steps VALUES (?, ?)').run(idx, new Uint8Array(data));
    if (rows.trajectory) {
      db.run('CREATE TABLE trajectory_metadata_blob (data BLOB)');
      db.query('INSERT INTO trajectory_metadata_blob VALUES (?)').run(
        new Uint8Array(rows.trajectory)
      );
    }
  } finally {
    db.close();
  }
  return file;
}

interface HelperResponse {
  truncated: boolean;
  kinds: Record<
    string,
    {
      state: string;
      partial?: boolean;
      unreadable?: boolean;
      fingerprints: Record<string, unknown>;
    }
  >;
  rows: Array<Record<string, number | string>>;
  srows: Array<Record<string, number | string>>;
}

function runHelper(
  request: Record<string, unknown> = {},
  env: Record<string, string> = {}
): HelperResponse {
  const base = { ...process.env };
  delete base.ANTIGRAVITY_DATA_DIR;
  const output = execFileSync('python3', [HELPER], {
    input: JSON.stringify({ kinds: ['antigravity'], minDateMs: 0, ...request }),
    encoding: 'utf8',
    timeout: 30_000,
    env: { ...base, HOME: home, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1', ...env },
  });
  return JSON.parse(output) as HelperResponse;
}

/** Totals per model of the antigravity rows: events, uncached input, output, cache read, cache write. */
function byModel(response: HelperResponse): Record<string, number[]> {
  const result: Record<string, number[]> = {};
  for (const row of response.rows) {
    if (row.k !== 'antigravity') continue;
    const sums = (result[row.m as string] ??= [0, 0, 0, 0, 0]);
    ['n', 'i', 'o', 'cr', 'cw'].forEach((field, index) => (sums[index] += row[field] as number));
  }
  return result;
}

/**
 * Runs Python with the helper loaded as `helper` (its main() not run), then `code`; prints what `code` prints.
 * Used to stand in for a writer that opens a database mid-read, or to shrink a bound.
 */
function runWithHelper(
  code: string,
  args: string[] = [],
  env: Record<string, string> = {}
): string {
  const base = { ...process.env };
  delete base.ANTIGRAVITY_DATA_DIR;
  const driver = [
    'import importlib.util,json,os,sys',
    "spec=importlib.util.spec_from_file_location('helper',sys.argv[1])",
    'helper=importlib.util.module_from_spec(spec)',
    'spec.loader.exec_module(helper)',
    code,
  ].join('\n');
  return execFileSync('python3', ['-c', driver, HELPER, ...args], {
    input: JSON.stringify({ kinds: ['antigravity'], minDateMs: 0 }),
    encoding: 'utf8',
    timeout: 30_000,
    env: { ...base, HOME: home, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1', ...env },
  });
}

/** A WAL-mode database that its writer closed cleanly: no -wal or -shm file is left. */
function closedWalConversation(relative: string, input: number): string {
  const file = path.join(home, relative);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const db = new Database(file);
  try {
    db.run('PRAGMA journal_mode = WAL');
    db.run('CREATE TABLE steps (idx INTEGER, metadata BLOB)');
    db.query('INSERT INTO steps VALUES (?, ?)').run(
      0,
      new Uint8Array(
        protoBytes(9, [...protoNumber(1, 246), ...protoNumber(2, input), ...protoText(11, 'c-0')])
      )
    );
  } finally {
    db.close();
  }
  return file;
}

const hourOf = (ms: number) => new Date(ms).toISOString().slice(0, 13).replace('T', ' ') + ':00';

describe.skipIf(!HAVE_PYTHON)('Antigravity usage in the analytics helper', () => {
  it('deduplicates generation and step usage while keeping retry models and token buckets', () => {
    const stamp = protoNumber(1, 1780000000);
    const usage = [
      ...protoNumber(2, 100),
      ...protoNumber(3, 40),
      ...protoNumber(4, 5),
      ...protoNumber(5, 20),
      ...protoNumber(9, 10),
      ...protoText(11, 'response-1'),
    ];
    const retry = [
      ...protoNumber(1, 1026),
      ...protoNumber(2, 12),
      ...protoNumber(3, 3),
      ...protoText(11, 'retry-1'),
    ];
    conversation(path.join(CONVERSATIONS, 'session-1.db'), {
      generations: [
        protoBytes(1, [
          ...protoBytes(4, usage),
          ...protoText(19, 'Gemini 3 Pro'),
          ...protoBytes(9, protoBytes(4, stamp)),
        ]),
      ],
      steps: [
        [...protoBytes(9, usage), ...protoBytes(8, stamp), ...protoBytes(28, protoBytes(2, retry))],
      ],
    });
    const response = runHelper();
    expect(response.kinds.antigravity.state).toBe('ok');
    expect(response.kinds.antigravity.unreadable).toBeUndefined();
    expect(byModel(response)).toEqual({
      'gemini-3-pro': [1, 100, 40, 20, 5],
      'claude-opus-4-6': [1, 12, 3, 0, 0],
    });
    expect(response.rows.every((row) => row.h === hourOf(1780000000000))).toBe(true);
    // One session row per model, under the session's published key; no id or path travels.
    expect(new Set(response.srows.map((row) => row.s))).toEqual(
      new Set([analyticsSessionKey('antigravity', 'session-1')])
    );
    expect(JSON.stringify(response)).not.toContain('response-1');
    expect(JSON.stringify(response)).not.toContain(home);
    // The window applies to the merged record's timestamp.
    expect(byModel(runHelper({ minDateMs: 1780000000001 }))).toEqual({});
  });

  it("uses the matching generation's model for a model-less step", () => {
    conversation(path.join(CONVERSATIONS, 'model-switch.db'), {
      generations: ['Gemini 3 Pro', 'Claude Opus 4.6'].map((name) =>
        protoBytes(1, protoText(19, name))
      ),
      steps: [0, 1].map((idx) => protoBytes(9, protoNumber(2, 10 + idx))),
    });
    expect(byModel(runHelper())).toEqual({
      'gemini-3-pro': [1, 10, 0, 0, 0],
      'claude-opus-4-6': [1, 11, 0, 0, 0],
    });
  });

  it('merges aliases that bridge previously separate step records', () => {
    conversation(path.join(CONVERSATIONS, 'bridge.db'), {
      steps: [
        protoBytes(9, [...protoNumber(2, 100), ...protoText(11, 'response')]),
        protoBytes(9, [...protoNumber(3, 40), ...protoText(12, 'provider')]),
      ],
      generations: [
        protoBytes(1, [
          ...protoText(19, 'Gemini 3 Pro'),
          ...protoBytes(4, [
            ...protoNumber(2, 50),
            ...protoNumber(5, 20),
            ...protoText(11, 'response'),
            ...protoText(12, 'provider'),
          ]),
        ]),
      ],
    });
    const response = runHelper();
    const totals = Object.values(byModel(response));
    expect(totals).toEqual([[1, 100, 40, 20, 0]]);
  });

  it('merges provider and message aliases across roots, keeping the first owner', () => {
    for (const index of [0, 1]) {
      const usage = (identity: number) => [
        ...protoNumber(1, 246),
        ...protoNumber(2, index === 0 ? 100 : 150),
        ...protoText(11, `response-${index}-${identity}`),
        ...protoText(identity, `shared-${identity}`),
      ];
      conversation(
        path.join(`root-${index}`, `session-${index}.db`),
        { steps: [protoBytes(9, usage(7)), protoBytes(9, usage(12))] },
        ['steps']
      );
    }
    const response = runHelper(
      {},
      { ANTIGRAVITY_DATA_DIR: `${path.join(home, 'root-0')},${path.join(home, 'root-1')}` }
    );
    expect(byModel(response)).toEqual({ 'gemini-2.5-pro': [2, 300, 0, 0, 0] });
    expect(new Set(response.srows.map((row) => row.s))).toEqual(
      new Set([analyticsSessionKey('antigravity', 'session-0')])
    );
  });

  it('upgrades fallback timestamps before applying the window', () => {
    for (const fallback of ['mtime', 'trajectory']) {
      const generations: number[][] = [];
      const steps: number[][] = [];
      for (const [index, seconds] of [1780000000, 1780000200].entries()) {
        const usage = [...protoNumber(2, 10), ...protoText(11, `${fallback}-${index}`)];
        steps.push(protoBytes(9, usage));
        generations.push(
          protoBytes(1, [
            ...protoBytes(4, usage),
            ...protoBytes(9, protoBytes(4, protoNumber(1, seconds))),
          ])
        );
      }
      const file = conversation(path.join(CONVERSATIONS, `${fallback}.db`), {
        generations,
        steps,
        ...(fallback === 'trajectory'
          ? { trajectory: protoBytes(2, protoNumber(1, 1780000200)) }
          : {}),
      });
      fs.utimesSync(file, 1780000000, 1780000000);
    }
    const response = runHelper({ minDateMs: 1780000100000 });
    const rows = response.rows.filter((row) => row.k === 'antigravity');
    expect(rows.reduce((sum, row) => sum + (row.n as number), 0)).toBe(2);
    expect(rows.every((row) => row.h === hourOf(1780000200000))).toBe(true);
  });

  it('reads step-only stores and reports a malformed database without failing the rest', () => {
    conversation(
      path.join(CONVERSATIONS, 'steps.db'),
      {
        steps: [
          [
            ...protoBytes(9, [...protoNumber(1, 246), ...protoNumber(2, 10), ...protoNumber(3, 5)]),
            ...protoBytes(8, protoNumber(1, 1780000000)),
          ],
        ],
      },
      ['steps']
    );
    fs.writeFileSync(path.join(home, CONVERSATIONS, 'broken.db'), 'not a sqlite database');
    const first = runHelper();
    expect(first.kinds.antigravity.state).toBe('ok');
    expect(first.kinds.antigravity.unreadable).toBe(true);
    expect(byModel(first)).toEqual({ 'gemini-2.5-pro': [1, 10, 5, 0, 0] });
    // The store print is withheld, so the next call reads every database again.
    expect(first.kinds.antigravity.fingerprints).toEqual({});
  });

  it('ignores large values in unused protobuf fields', () => {
    const unused = [...protoNumber(99, 0).slice(0, -1), ...Array(9).fill(0xff), 0x01];
    conversation(
      path.join(CONVERSATIONS, 'large-varint.db'),
      { steps: [protoBytes(9, [...protoNumber(1, 246), ...protoNumber(2, 10), ...unused])] },
      ['steps']
    );
    const response = runHelper();
    expect(response.kinds.antigravity.unreadable).toBeUndefined();
    expect(byModel(response)).toEqual({ 'gemini-2.5-pro': [1, 10, 0, 0, 0] });
  });

  it('names a numeric model id from the same store, so one model is one row', () => {
    conversation(path.join(CONVERSATIONS, 'named.db'), {
      // Steps carry the usage with id 1318 and no name; generations name id 1318.
      steps: [0, 1, 2].map((idx) =>
        protoBytes(9, [
          ...protoNumber(1, 1318),
          ...protoNumber(2, 100),
          ...protoText(11, `r-${idx}`),
        ])
      ),
      generations: [protoBytes(1, [...protoNumber(3, 1318), ...protoText(19, 'Gemini 3.8 Flash')])],
    });
    expect(byModel(runHelper())).toEqual({ 'gemini-3.8-flash': [3, 300, 0, 0, 0] });
  });

  it("finds every T3 instance's antigravity-acp store and counts copies once", () => {
    const usage = (id: string, input: number) =>
      protoBytes(9, [...protoNumber(1, 246), ...protoNumber(2, input), ...protoText(11, id)]);
    conversation(path.join(CONVERSATIONS, 'a.db'), { steps: [usage('shared', 10)] }, ['steps']);
    // A backup copy and a T3 instance copy of the same record, plus the instance's own record.
    conversation(
      path.join('.gemini', 'antigravity-backup', 'conversations', 'a.db'),
      { steps: [usage('shared', 10)] },
      ['steps']
    );
    const instance = path.join(
      '.t3',
      'userdata',
      'providers',
      'antigravity',
      createHash('sha256').update('antigravity_party').digest('hex'),
      'antigravity-acp'
    );
    conversation(
      path.join(instance, 'conversations', 'b.db'),
      { steps: [usage('shared', 10), usage('own', 7)] },
      ['steps']
    );
    // Credentials and secrets sit next to the store and are never opened.
    const token = path.join(home, instance, 'acp_token.json');
    fs.writeFileSync(token, '{}');
    fs.chmodSync(token, 0o000);
    conversation(
      path.join(instance, 'conversations', 'secrets', 'hidden.db'),
      { steps: [usage('secret', 1000)] },
      ['steps']
    );
    // A root with a conversations folder is read there only: the summaries database is never opened.
    fs.writeFileSync(
      path.join(home, '.gemini', 'antigravity-cli', 'conversation_summaries.db'),
      'x'
    );
    const response = runHelper();
    expect(response.kinds.antigravity.unreadable).toBeUndefined();
    expect(byModel(response)).toEqual({ 'gemini-2.5-pro': [2, 17, 0, 0, 0] });
    expect(new Set(response.srows.map((row) => row.s))).toEqual(
      new Set(['a', 'b'].map((session) => analyticsSessionKey('antigravity', session)))
    );
    fs.chmodSync(token, 0o600);
  });

  it('follows links only inside a root and never into ~/PM-Experiments', () => {
    const usage = (id: string, input: number) =>
      protoBytes(9, [...protoNumber(1, 246), ...protoNumber(2, input), ...protoText(11, id)]);
    const inside = conversation(
      path.join(CONVERSATIONS, 'real.db'),
      { steps: [usage('real', 1)] },
      ['steps']
    );
    fs.symlinkSync(inside, path.join(home, CONVERSATIONS, 'alias.db'));
    const outside = conversation(path.join('elsewhere', 'out.db'), { steps: [usage('out', 20)] }, [
      'steps',
    ]);
    fs.symlinkSync(outside, path.join(home, CONVERSATIONS, 'outside.db'));
    conversation(
      path.join('PM-Experiments', 'agy', 'conversations', 'x.db'),
      { steps: [usage('pm', 300)] },
      ['steps']
    );
    fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });
    fs.symlinkSync(
      path.join(home, 'PM-Experiments', 'agy'),
      path.join(home, '.gemini', 'antigravity-ide')
    );
    expect(byModel(runHelper())).toEqual({ 'gemini-2.5-pro': [1, 1, 0, 0, 0] });
    // Named directly, ~/PM-Experiments is still never entered.
    const pm = runHelper({}, { ANTIGRAVITY_DATA_DIR: path.join(home, 'PM-Experiments', 'agy') });
    expect(byModel(pm)).toEqual({});
    expect(pm.kinds.antigravity.state).toBe('not_installed');
    // The server's own presence check agrees.
    expect(
      antigravityUsagePresent({
        homeDir: home,
        env: { ANTIGRAVITY_DATA_DIR: path.join(home, 'PM-Experiments', 'agy') },
      })
    ).toBe(false);
  });

  it('reads a live write-ahead log read-only, and an unchanged store returns no rows', () => {
    const file = path.join(home, CONVERSATIONS, 'live.db');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const writer = new Database(file);
    try {
      writer.run('PRAGMA journal_mode = WAL');
      writer.run('PRAGMA wal_autocheckpoint = 0');
      writer.run('CREATE TABLE steps (idx INTEGER, metadata BLOB)');
      const insert = (idx: number, input: number) =>
        writer
          .query('INSERT INTO steps VALUES (?, ?)')
          .run(
            idx,
            new Uint8Array(
              protoBytes(9, [
                ...protoNumber(1, 246),
                ...protoNumber(2, input),
                ...protoText(11, `w-${idx}`),
              ])
            )
          );
      insert(0, 10);
      const digest = (target: string) =>
        createHash('sha256').update(fs.readFileSync(target)).digest('hex');
      const before = [digest(file), digest(`${file}-wal`)];
      expect(fs.statSync(`${file}-wal`).size).toBeGreaterThan(0);
      const first = runHelper();
      // mode=ro sees rows still in the write-ahead log (immutable=1 would not).
      expect(byModel(first)).toEqual({ 'gemini-2.5-pro': [1, 10, 0, 0, 0] });
      // Reading wrote nothing: the database and its log are byte for byte the same.
      expect([digest(file), digest(`${file}-wal`)]).toEqual(before);
      const prints = first.kinds.antigravity.fingerprints;
      expect(Object.keys(prints)).toHaveLength(1);
      const again = runHelper({ fingerprints: { antigravity: prints } });
      expect(again.rows).toEqual([]);
      expect(again.kinds.antigravity.fingerprints).toEqual(prints);
      // A new record changes the store print and the whole store is read again.
      insert(1, 5);
      const next = runHelper({ fingerprints: { antigravity: prints } });
      expect(byModel(next)).toEqual({ 'gemini-2.5-pro': [2, 15, 0, 0, 0] });
      expect(next.kinds.antigravity.fingerprints).not.toEqual(prints);
      // The writer keeps writing after the reads.
      insert(2, 1);
    } finally {
      writer.close();
    }
  });

  it('reads a cleanly closed WAL database without creating -wal or -shm files', () => {
    const file = closedWalConversation(path.join(CONVERSATIONS, 'clean.db'), 12);
    const listing = () => fs.readdirSync(path.dirname(file)).sort();
    expect(listing()).toEqual(['clean.db']);
    expect(fs.readFileSync(file).subarray(18, 20)).toEqual(Buffer.from([2, 2]));
    const digest = createHash('sha256').update(fs.readFileSync(file)).digest('hex');
    const response = runHelper();
    expect(byModel(response)).toEqual({ 'gemini-2.5-pro': [1, 12, 0, 0, 0] });
    expect(response.kinds.antigravity.unreadable).toBeUndefined();
    expect(listing()).toEqual(['clean.db']);
    expect(createHash('sha256').update(fs.readFileSync(file)).digest('hex')).toBe(digest);
  });

  it('drops a cleanly closed database a writer opens mid-read, and reads it next scan', () => {
    const file = closedWalConversation(path.join(CONVERSATIONS, 'race.db'), 9);
    // Stand in for Antigravity opening the database between the check and the read: its -wal appears.
    const raced = JSON.parse(
      runWithHelper(
        [
          'real=helper.sqlite3.connect',
          'def connect(target,*a,**k):',
          '    if "immutable=1" in target: open(sys.argv[2]+"-wal","wb").close()',
          '    return real(target,*a,**k)',
          'helper.sqlite3.connect=connect',
          'helper.main()',
        ].join('\n'),
        [file]
      )
    ) as HelperResponse;
    expect(byModel(raced)).toEqual({});
    expect(raced.kinds.antigravity.unreadable).toBe(true);
    // The store print is withheld, so the next scan reads again: now mode=ro, with the -wal present.
    expect(raced.kinds.antigravity.fingerprints).toEqual({});
    expect(fs.existsSync(`${file}-wal`)).toBe(true);
    expect(byModel(runHelper())).toEqual({ 'gemini-2.5-pro': [1, 9, 0, 0, 0] });
  });

  it('bounds one database by size and by the deadline, and skips databases without usage tables', () => {
    const file = closedWalConversation(path.join('store', 'a.db'), 3);
    const big = conversation(
      path.join('big', 'b.db'),
      {
        steps: Array.from({ length: 600 }, (_, idx) =>
          protoBytes(9, [
            ...protoNumber(1, 246),
            ...protoNumber(2, 1),
            ...protoText(11, `b-${idx}`),
          ])
        ),
      },
      ['steps']
    );
    // A valid SQLite database of another app in a walked root holds no usage: skipped quietly.
    const other = new Database(path.join(home, 'store', 'other.db'));
    other.run('CREATE TABLE notes (body TEXT)');
    other.close();
    const env = { ANTIGRAVITY_DATA_DIR: path.join(home, 'store') };
    const plain = runHelper({}, env);
    expect(byModel(plain)).toEqual({ 'gemini-2.5-pro': [1, 3, 0, 0, 0] });
    expect(plain.kinds.antigravity.unreadable).toBeUndefined();
    // Past the size bound a database is not read, and the kind says so.
    const capped = JSON.parse(
      runWithHelper(['helper.AGY_MAX_DB_BYTES=1', 'helper.main()'].join('\n'), [], env)
    ) as HelperResponse;
    expect(byModel(capped)).toEqual({});
    expect(capped.kinds.antigravity.unreadable).toBe(true);
    // A deadline that passes inside one database aborts its read instead of running on.
    const aborted = runWithHelper(
      [
        'try:',
        '    helper._agy_read_db(sys.argv[2], 0, lambda: True)',
        '    print("read")',
        'except Exception as error:',
        '    print(type(error).__name__)',
      ].join('\n'),
      [big]
    );
    // Either the query is interrupted, or the parse loop stops at its next check.
    expect(['OperationalError', '_AgyDeadline']).toContain(aborted.trim());
    expect(fs.existsSync(`${file}-wal`)).toBe(false);
  });

  it('says not installed without any store, and the transport accepts the kind', () => {
    const response = runHelper();
    expect(response.kinds.antigravity.state).toBe('not_installed');
    const parsed = parseAnalyticsRemoteResponse(
      JSON.stringify({
        version: 1,
        truncated: false,
        kinds: { antigravity: { state: 'ok', fingerprints: {}, unreadable: true } },
        rows: [
          {
            k: 'antigravity',
            f: 'a'.repeat(64),
            m: 'gemini-3.8-flash',
            h: '2026-10-01 15:00',
            i: 1,
            o: 2,
            cr: 3,
            cw: 0,
            c: 0,
            n: 1,
          },
        ],
        srows: [],
      })
    );
    expect(parsed.kinds.antigravity).toEqual({ state: 'ok', fingerprints: {}, unreadable: true });
    expect(parsed.rows).toHaveLength(1);
    expect(() =>
      parseAnalyticsRemoteResponse(
        JSON.stringify({
          version: 1,
          truncated: false,
          kinds: { antigravity: { state: 'ok', fingerprints: {}, unreadable: 'yes' } },
          rows: [],
          srows: [],
        })
      )
    ).toThrow();
  });

  it('feeds the local collector, which keeps its rows while the store is unchanged', async () => {
    conversation(
      path.join(CONVERSATIONS, 'c.db'),
      {
        steps: [
          [
            ...protoBytes(9, [
              ...protoNumber(1, 246),
              ...protoNumber(2, 40),
              ...protoNumber(5, 60),
              ...protoText(11, 'c'),
            ]),
            ...protoBytes(8, protoNumber(1, Math.floor(Date.now() / 1000) - 3600)),
          ],
        ],
      },
      ['steps']
    );
    const cacheDir = path.join(home, 'cache');
    const saved = { HOME: process.env.HOME, DIR: process.env.ANTIGRAVITY_DATA_DIR };
    process.env.HOME = home;
    delete process.env.ANTIGRAVITY_DATA_DIR;
    try {
      expect(antigravityUsagePresent({ homeDir: home, env: {} })).toBe(true);
      const options = { minDate: Date.now() - 31 * 86_400_000, cacheDir };
      for (let pass = 0; pass < 2; pass++) {
        const result = await collectAccountActivity({ kind: 'antigravity' }, options);
        expect(result.eventCount).toBe(1);
        expect(result.scan?.complete).toBe(true);
        expect(result.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(40);
        expect(result.hourly.reduce((sum, hour) => sum + hour.cacheReadTokens, 0)).toBe(60);
        expect(result.session.map((row) => row.sessionId)).toEqual([
          analyticsSessionKey('antigravity', 'c'),
        ]);
        expect(result.session[0].target).toBe('antigravity');
      }
    } finally {
      if (saved.HOME === undefined) delete process.env.HOME;
      else process.env.HOME = saved.HOME;
      if (saved.DIR !== undefined) process.env.ANTIGRAVITY_DATA_DIR = saved.DIR;
    }
  });
});

describe('Antigravity roots', () => {
  it("lists T3's roots: the env override, else the default stores, then every T3 instance", () => {
    const instances = path.join(home, '.t3', 'userdata', 'providers', 'antigravity');
    fs.mkdirSync(path.join(instances, 'b'), { recursive: true });
    fs.mkdirSync(path.join(instances, 'a'), { recursive: true });
    expect(resolveAntigravityRoots({ homeDir: home, env: {} })).toEqual([
      path.join(home, '.gemini', 'antigravity'),
      path.join(home, '.gemini', 'antigravity-cli'),
      path.join(home, '.gemini', 'antigravity-ide'),
      path.join(home, '.gemini', 'antigravity-backup'),
      path.join(home, '.config', 'antigravity'),
      path.join(instances, 'a', 'antigravity-acp'),
      path.join(instances, 'b', 'antigravity-acp'),
    ]);
    expect(
      resolveAntigravityRoots({
        homeDir: home,
        env: { ANTIGRAVITY_DATA_DIR: ' /x , ~/y ,relative' },
      })
    ).toEqual([
      '/x',
      path.join(home, 'y'),
      ...['a', 'b'].map((id) => path.join(instances, id, 'antigravity-acp')),
    ]);
    // Present only once a root folder exists.
    expect(antigravityUsagePresent({ homeDir: home, env: {} })).toBe(false);
    fs.mkdirSync(path.join(instances, 'a', 'antigravity-acp'));
    expect(antigravityUsagePresent({ homeDir: home, env: {} })).toBe(true);
  });
});
