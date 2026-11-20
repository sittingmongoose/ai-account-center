/**
 * DESIGN 16.1 on the server: a log read across calls adds each call's rows to what was kept for
 * it, an answer that does not start where the last one stopped is dropped, the logs not read to
 * their end are named on the cell (without conversation ids), and the Python fallback never sees
 * a part-read print.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
  unfinishedText,
} from '../../../src/web-server/services/analytics-remote-sources';
import {
  parseAnalyticsRemoteResponse,
  withoutPartialPrints,
  type AnalyticsRemoteKind,
} from '../../../src/web-server/services/analytics-remote-transport';

const MIN_DATE = Date.parse('2026-10-15T00:00:00Z');
const KINDS: AnalyticsRemoteKind[] = ['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity'];
const FILE = 'a1'.repeat(32);
const SESSION = 'c3'.repeat(8);
const NAME = 'rollout-2026-10-31T08-06-21-0123456789abcdef.jsonl.zst';
const PRINT = {
  size: 3_397_722_778,
  mtimeMs: 1_793_000_000_000,
  head: 'a'.repeat(64),
  tail: 'b'.repeat(64),
};
const STATE = {
  model: 'gpt-6.1-sol',
  provider: 'openai',
  previous: [1, 2, 3, 4],
  sessionDigest: 'd'.repeat(16),
};

let dir: string;
let savedCcs: string | undefined;
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-resume-'));
  savedCcs = process.env.CCS_HOME;
  process.env.CCS_HOME = path.join(dir, 'ccs');
});
afterEach(() => {
  if (savedCcs === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = savedCcs;
  fs.rmSync(dir, { recursive: true, force: true });
});

const row = (hour: string, n: number) => ({
  k: 'codex',
  f: FILE,
  m: 'gpt-6.1-sol',
  h: hour,
  i: 10 * n,
  o: n,
  cr: 0,
  cw: 0,
  c: 0,
  n,
});
const srow = (a: string, z: string, n: number) => ({
  k: 'codex',
  f: FILE,
  s: SESSION,
  m: 'gpt-6.1-sol',
  a: Date.parse(a),
  z: Date.parse(z),
  i: 10 * n,
  o: n,
  cr: 0,
  cw: 0,
  c: 0,
  n,
});

function answer(codex: Record<string, unknown>, rows: unknown[], srows: unknown[]) {
  return parseAnalyticsRemoteResponse(
    JSON.stringify({
      version: 2,
      truncated: false,
      kinds: Object.fromEntries(
        KINDS.map((kind) => [kind, kind === 'codex' ? codex : { state: 'ok', fingerprints: {} }])
      ),
      rows,
      srows,
    })
  );
}

const hosts = async () => ({ mac: null, windows: null, nas1: 'nas1-agent', alexis: null, odroid: null });
const cacheFile = (cache: string) => path.join(cache, 'analytics-remote-v1', 'nas1.json');
const codexCell = (states: Array<{ host: string; tool: string }>) =>
  states.find((entry) => entry.host === 'nas1' && entry.tool === 'codex') as
    { state: string; detail: string | null; rowCount: number } | undefined;

async function scan(cache: string, reply: ReturnType<typeof answer>, at: string) {
  return loadAnalyticsRemoteSources(MIN_DATE, {
    cacheDir: cache,
    hosts,
    now: () => Date.parse(at),
    force: true,
    runHelper: async () => reply,
  });
}

const partial = (through: number) => ({
  state: 'ok',
  partial: true,
  fingerprints: {
    [FILE]: {
      ...PRINT,
      readThrough: through,
      readState: STATE,
      readName: NAME,
      decodedSize: 4_637_763_647,
    },
  },
  unfinished: [
    {
      name: NAME,
      sizeBytes: PRINT.size,
      readThroughBytes: through,
      decodedBytes: 4_637_763_647,
      reason: 'deadline',
    },
  ],
  unfinishedCount: 1,
});

describe('a log read across calls', () => {
  it('adds a resumed answer to the rows kept for its file, and names the log until it is read', async () => {
    const cache = path.join(dir, 'cache');
    // Call 1: the deadline cut the log 31% in.
    const first = await scan(
      cache,
      answer(
        partial(1_450_000_000),
        [row('2026-10-31 08:00', 2)],
        [srow('2026-10-31T08:10:00Z', '2026-10-31T08:40:00Z', 2)]
      ),
      '2026-11-13T10:00:00Z'
    );
    expect(codexCell(first.states)?.detail).toContain(
      `${NAME} (3.2 GB, partly read: 1,450,000,000 of 4,637,763,647 bytes)`
    );
    expect(JSON.stringify(first.states)).not.toMatch(/[0-9a-f]{8}-[0-9a-f]{4}-/);
    // After a restart, the cell still names it.
    const restarted = loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache });
    expect(codexCell(restarted.states)?.detail).toContain(
      `${NAME} (3.2 GB, partly read: 1,450,000,000 of 4,637,763,647 bytes)`
    );

    // Call 2 reads on from there to the end: its rows add to the first call's.
    const second = await scan(
      cache,
      answer(
        { state: 'ok', fingerprints: { [FILE]: { ...PRINT, readFrom: 1_450_000_000 } } },
        [row('2026-10-31 08:00', 1), row('2026-10-31 09:00', 3)],
        [srow('2026-10-31T08:50:00Z', '2026-10-31T09:20:00Z', 4)]
      ),
      '2026-11-13T10:05:00Z'
    );
    const saved = JSON.parse(fs.readFileSync(cacheFile(cache), 'utf8'));
    expect(saved.rows.map((r: { h: string; n: number }) => [r.h, r.n])).toEqual([
      ['2026-10-31 08:00', 3],
      ['2026-10-31 09:00', 3],
    ]);
    expect(saved.srows).toHaveLength(1);
    expect(saved.srows[0]).toMatchObject({
      n: 6,
      a: Date.parse('2026-10-31T08:10:00Z'),
      z: Date.parse('2026-10-31T09:20:00Z'),
    });
    // The saved print says the file is read; `readFrom` described one answer only.
    expect(saved.fingerprints.codex[FILE].readThrough).toBeUndefined();
    expect(saved.fingerprints.codex[FILE].readFrom).toBeUndefined();
    expect(saved.kinds.codex.unfinished).toBeUndefined();
    expect(codexCell(second.states)).toMatchObject({ state: 'ok', rowCount: 6 });
    expect(codexCell(second.states)?.detail ?? '').not.toContain('not read to the end');
  });

  it('drops a resumed answer that does not start where the last one stopped, so the file is read from 0', async () => {
    const cache = path.join(dir, 'cache');
    await scan(
      cache,
      answer(partial(1_000), [row('2026-10-31 08:00', 2)], []),
      '2026-11-13T10:00:00Z'
    );
    await scan(
      cache,
      answer(
        { state: 'ok', fingerprints: { [FILE]: { ...PRINT, readFrom: 5_000 } } },
        [row('2026-10-31 09:00', 7)],
        []
      ),
      '2026-11-13T10:05:00Z'
    );
    const saved = JSON.parse(fs.readFileSync(cacheFile(cache), 'utf8'));
    // Neither the gap-holed answer nor the stale part: no print, so the next call starts at 0.
    expect(saved.fingerprints.codex?.[FILE]).toBeUndefined();
    expect(saved.rows.filter((r: { f: string }) => r.f === FILE)).toEqual([]);
  });

  it('a whole read of a part-read file replaces what was kept', async () => {
    const cache = path.join(dir, 'cache');
    await scan(
      cache,
      answer(partial(1_000), [row('2026-10-31 08:00', 2)], []),
      '2026-11-13T10:00:00Z'
    );
    await scan(
      cache,
      answer(
        { state: 'ok', fingerprints: { [FILE]: { ...PRINT } } },
        [row('2026-10-31 08:00', 5)],
        []
      ),
      '2026-11-13T10:05:00Z'
    );
    const saved = JSON.parse(fs.readFileSync(cacheFile(cache), 'utf8'));
    expect(saved.rows.map((r: { n: number }) => r.n)).toEqual([5]);
  });
});

describe('naming and the fallback', () => {
  it('names at most five logs and counts the rest', () => {
    const list = Array.from({ length: 6 }, (_, index) => ({
      name: `rollout-2026-11-0${index + 1}T00-00-00-0123456789abcdef.jsonl.zst`,
      sizeBytes: 700 * 1024 ** 2,
      readThroughBytes: index === 5 ? null : 100 * 1024 ** 2,
      decodedBytes: index === 0 ? 1000 * 1024 ** 2 : null,
      reason: index === 5 ? ('unreadable' as const) : ('deadline' as const),
    }));
    const text = unfinishedText(list, 9);
    expect(text).toStartWith('9 logs not read to the end yet: ');
    expect(text).toContain(
      'rollout-2026-11-01T00-00-00-0123456789abcdef.jsonl.zst (700 MB, partly read: 104,857,600 of 1,048,576,000 bytes)'
    );
    expect(text).toContain(
      'rollout-2026-11-02T00-00-00-0123456789abcdef.jsonl.zst (700 MB, partly read: 104,857,600 bytes, full size not stated in the file)'
    );
    expect(text).toContain('and 4 more');
  });

  it('never hands the Python fallback a part-read print, which it would take as read', () => {
    const out = withoutPartialPrints({
      codex: {
        [FILE]: { ...PRINT, readThrough: 10, readState: STATE, readName: NAME },
        ['b2'.repeat(32)]: { ...PRINT, readFrom: 10 },
      },
      claude: { ['c3'.repeat(32)]: { ...PRINT } },
    });
    expect(out.codex[FILE]).toBeUndefined();
    expect(out.codex['b2'.repeat(32)]).toEqual(PRINT);
    expect(out.claude['c3'.repeat(32)]).toEqual(PRINT);
  });

  it('refuses a print whose name or state could carry an identifier, and drops a bad unfinished list', () => {
    const withPrint = (print: Record<string, unknown>) => () =>
      answer({ state: 'ok', fingerprints: { [FILE]: { ...PRINT, ...print } } }, [], []);
    expect(withPrint({ readName: 'rollout-01a19f7c-b191-7a42-9bd9-c0b3dd7f3f33.jsonl' })).toThrow();
    // FIX-ROUND-BIG 18 (Astra M11): only `rollout-<time>-<session key>` or `log-<digest>` names pass;
    // a renamed log's own suffix (an opaque session handle) never does.
    expect(
      withPrint({ readName: 'rollout-2026-11-11T12-00-00-private-session-handle.jsonl' })
    ).toThrow();
    expect(withPrint({ readName: 'notes-of-a-private-project.jsonl' })).toThrow();
    expect(withPrint({ readName: 'log-0123456789ab.jsonl' })).not.toThrow();
    expect(withPrint({ readState: { ...STATE, session: 'raw-id' } })).toThrow();
    expect(withPrint({ readState: { ...STATE, sessionDigest: 'not-a-digest' } })).toThrow();
    const parsed = answer(
      {
        ...partial(10),
        unfinished: [
          {
            name: 'x-01a19f7c-b191-7a42-9bd9-c0b3dd7f3f33',
            sizeBytes: 1,
            readThroughBytes: 1,
            decodedBytes: null,
            reason: 'deadline',
          },
        ],
      },
      [],
      []
    );
    expect(parsed.kinds.codex?.unfinished).toBeUndefined();
    expect(parsed.kinds.codex?.fingerprints[FILE]?.readThrough).toBe(10);
  });
});
