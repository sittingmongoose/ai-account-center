/**
 * The five-model review of b09066667 on the server's side (FIX-ROUND-BIG.md in the share folder
 * 2026-11-13-analytics-discovery): what the scanner now says reaches the cells, and failures never
 * read as success. Each test fails at b09066667.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  parseAnalyticsRemoteResponse,
  type AnalyticsRemoteResponse,
} from '../../../src/web-server/services/analytics-remote-transport';
import {
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
  unfinishedText,
} from '../../../src/web-server/services/analytics-remote-sources';

const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');
const FILE_1 = 'a1'.repeat(32);
const FILE_2 = 'b2'.repeat(32);
const ROOT = 'c3'.repeat(8);

let cache: string;
let saved: string | undefined;
beforeEach(() => {
  saved = process.env.CCS_HOME;
  cache = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-big-review-'));
  process.env.CCS_HOME = cache;
});
afterEach(() => {
  if (saved === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = saved;
  fs.rmSync(cache, { recursive: true, force: true });
});

const row = (f: string, i: number) => ({
  k: 'codex',
  f,
  m: 'gpt-6.1-sol',
  h: '2026-10-01 15:00',
  i,
  o: 1,
  cr: 0,
  cw: 0,
  c: 0,
  n: 1,
});

const print = { size: 10, mtimeMs: 20, head: 'a'.repeat(64), tail: 'b'.repeat(64) };

function answer(overrides: Record<string, unknown>): AnalyticsRemoteResponse {
  return parseAnalyticsRemoteResponse(
    JSON.stringify({
      version: 2,
      truncated: false,
      kinds: { codex: { state: 'ok', fingerprints: { [FILE_1]: print } } },
      rows: [row(FILE_1, 10)],
      srows: [],
      ...overrides,
    })
  );
}

/**
 * One mac host, scanned once per answer in `answers`, a scan apart. Each call (one per kind on a
 * host still catching up) gets the pass's answer for its own kinds; the others are not installed.
 */
async function scans(answers: AnalyticsRemoteResponse[]) {
  let clock = Date.parse('2026-10-02T00:00:00Z');
  let last: Awaited<ReturnType<typeof loadAnalyticsRemoteSources>> | undefined;
  for (const base of answers) {
    clock += 600_000;
    last = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: cache,
      now: () => clock,
      force: true,
      hosts: async () => ({ mac: 'mac-alias', windows: null, nas1: null, alexis: null }),
      localScanner: () => ({ ok: false, cause: 'binary_not_built' }),
      runHelper: async (_alias, _host, request) => ({
        ...base,
        kinds: Object.fromEntries(
          request.kinds.map((kind) => [
            kind,
            base.kinds[kind] ?? { state: 'not_installed', fingerprints: {} },
          ])
        ) as AnalyticsRemoteResponse['kinds'],
        rows: base.rows.filter((entry) => request.kinds.includes(entry.k)),
        srows: base.srows.filter((entry) => request.kinds.includes(entry.k)),
        ...(base.discovery
          ? {
              discovery: {
                kinds: Object.fromEntries(
                  Object.entries(base.discovery.kinds).filter(([kind]) =>
                    request.kinds.includes(kind as never)
                  )
                ),
              },
            }
          : {}),
      }),
    });
  }
  if (!last) throw new Error('no scan');
  const cell = last.states.find((entry) => entry.host === 'mac' && entry.tool === 'codex');
  const input = last.results
    .filter((entry) => entry.tool === 'codex')
    .flatMap((entry) => entry.data.hourly)
    .reduce((sum, hour) => sum + hour.inputTokens, 0);
  return { last, cell, input };
}

describe('what the scanner says about failures reaches the cell', () => {
  it('parses unreadable roots with their cause, folder failures, limits, mounts and a stall', () => {
    const parsed = answer({
      kinds: {
        codex: {
          state: 'ok',
          partial: true,
          unreadableFolders: 2,
          searchLimit: { read: 64, found: 65 },
          fingerprints: { [FILE_1]: { ...print, unreadableRecords: 1, walUnread: true } },
        },
      },
      discovery: {
        kinds: {
          codex: {
            installed: true,
            signals: ['home'],
            roots: [
              {
                root: ROOT,
                origin: 'default',
                state: 'unreadable',
                cause: 'permission_denied',
                counts: { filesFound: 0, newestMtime: null },
              },
            ],
            counts: { roots: 1, filesFound: 0, filesRead: 0, filesFailed: 0, newestMtime: null },
          },
        },
      },
      unansweredMounts: 1,
      stalled: true,
    });
    expect(parsed.kinds.codex.unreadableFolders).toBe(2);
    expect(parsed.kinds.codex.searchLimit).toEqual({ read: 64, found: 65 });
    expect(parsed.discovery?.kinds.codex?.roots[0]).toMatchObject({
      state: 'unreadable',
      cause: 'permission_denied',
    });
    expect(parsed.unansweredMounts).toBe(1);
    expect(parsed.stalled).toBe(true);
  });

  // Sol M5, Argon H1, Wonder M2, Fable M7: a log folder that cannot be listed is "can't read",
  // never "installed, no logs", and the usage read before stays.
  it('keeps the rows of a log folder that became unreadable and says so', async () => {
    const unreadable = answer({
      kinds: { codex: { state: 'not_installed', partial: true, fingerprints: {} } },
      rows: [],
      discovery: {
        kinds: {
          codex: {
            installed: true,
            signals: ['home'],
            roots: [
              {
                root: ROOT,
                origin: 'default',
                state: 'unreadable',
                cause: 'permission_denied',
                counts: { filesFound: 0, newestMtime: null },
              },
            ],
            counts: { roots: 1, filesFound: 0, filesRead: 0, filesFailed: 0, newestMtime: null },
          },
        },
      },
    });
    const { cell, input } = await scans([answer({}), unreadable]);
    expect(input).toBe(10);
    expect(cell?.state).toBe('unreadable');
    expect(cell?.detail).toContain('permission denied');
  });

  // Astra M7, Fable M4: a search limit is said on the cell.
  it('names a search limit on the cell', async () => {
    const limited = answer({
      kinds: {
        codex: {
          state: 'ok',
          partial: true,
          searchLimit: { read: 64, found: 65 },
          fingerprints: { [FILE_1]: print },
        },
      },
    });
    const { cell } = await scans([limited]);
    expect(cell?.detail).toContain('search limit reached: 64 of 65');
  });

  // FIX-ROUND-BIG 1: the Python fallback keys Codex logs by path, the scanner by conversation.
  // After a switch, a partial answer never keeps the other reader's rows beside its own.
  it('never keeps rows of the other reader after a switch', async () => {
    const python = {
      ...answer({}),
      reader: { name: 'python-fallback' as const, cause: 'binary_failed' },
    };
    const scanner = answer({
      kinds: { codex: { state: 'ok', partial: true, fingerprints: { [FILE_2]: print } } },
      rows: [row(FILE_2, 7)],
    });
    const { input } = await scans([python, scanner]);
    expect(input).toBe(7);
  });

  // FIX-ROUND-BIG 9: a network folder that did not answer, and a scan that stalled, are said.
  it('names network folders that did not answer, and a stalled scan', async () => {
    const mounts = answer({ unansweredMounts: 2 });
    const first = await scans([mounts]);
    expect(first.cell?.detail).toContain('2 network folders on this computer did not answer');
    const stalled = answer({
      truncated: true,
      kinds: { codex: { state: 'pending', fingerprints: {} } },
      rows: [],
      stalled: true,
    });
    const second = await scans([answer({}), stalled]);
    expect(second.input).toBe(10);
    expect(second.cell?.detail).toContain('did not answer within its time');
  });
});

describe('recheck 1 leftovers (RECHECK-BIG-1, FIX-ROUND-BIG-2)', () => {
  const unreadableRoot = {
    root: ROOT,
    origin: 'default',
    state: 'unreadable',
    cause: 'permission_denied',
    counts: { filesFound: 0, newestMtime: null },
  };
  const discoveryWith = (roots: unknown[]) => ({
    kinds: {
      codex: {
        installed: true,
        signals: ['home'],
        roots,
        counts: {
          roots: roots.length,
          filesFound: 0,
          filesRead: 0,
          filesFailed: 0,
          newestMtime: null,
        },
      },
    },
  });

  // M1: a log with no session header is named "partly read: no session id".
  it('parses and names a log with no session id', () => {
    const parsed = answer({
      kinds: {
        codex: {
          state: 'ok',
          partial: true,
          readFailures: 1,
          unfinished: [
            {
              name: 'log-0123456789ab.jsonl',
              sizeBytes: 500,
              readThroughBytes: null,
              decodedBytes: null,
              reason: 'no_session_id',
            },
          ],
          unfinishedCount: 1,
          fingerprints: { [FILE_1]: print },
        },
      },
    });
    expect(parsed.kinds.codex.unfinished).toHaveLength(1);
    expect(unfinishedText(parsed.kinds.codex.unfinished ?? [], 1)).toContain(
      'partly read: no session id'
    );
  });

  // M5: a cached answer keeps the unreadable-folder cause of the last real scan, and a later
  // successful scan clears it. At 106f05a5c the cached cell read only "cached remote scan".
  it('keeps the unreadable cause on cached cells until a successful scan', async () => {
    const unreadable = answer({
      kinds: { codex: { state: 'not_installed', partial: true, fingerprints: {} } },
      rows: [],
      discovery: discoveryWith([unreadableRoot]),
    });
    const live = await scans([answer({}), unreadable]);
    expect(live.cell?.state).toBe('unreadable');
    const cached = loadAnalyticsRemoteCachedSources(MIN_DATE, {
      cacheDir: cache,
      localScanner: () => ({ ok: false, cause: 'binary_not_built' }),
    });
    const cell = cached.states.find((entry) => entry.host === 'mac' && entry.tool === 'codex');
    expect(cell?.state).toBe('unreadable');
    expect(cell?.cause).toBe('unreadable_folder');
    expect(cell?.detail).toContain('permission denied');
    const tokens = cached.results
      .filter((entry) => entry.tool === 'codex')
      .flatMap((entry) => entry.data.hourly)
      .reduce((sum, hour) => sum + hour.inputTokens, 0);
    expect(tokens).toBe(10);
    // A successful scan with the folder readable again clears the cause.
    const healed = answer({
      discovery: discoveryWith([
        {
          root: ROOT,
          origin: 'default',
          state: 'ok',
          counts: { filesFound: 1, newestMtime: null },
        },
      ]),
    });
    await scans([healed]);
    const after = loadAnalyticsRemoteCachedSources(MIN_DATE, {
      cacheDir: cache,
      localScanner: () => ({ ok: false, cause: 'binary_not_built' }),
    });
    const cleared = after.states.find((entry) => entry.host === 'mac' && entry.tool === 'codex');
    expect(cleared?.state).not.toBe('unreadable');
    expect(cleared?.detail ?? '').not.toContain('permission denied');
  });

  // L2: presence says Cursor is installed with no local records, while the run-store reader read
  // it. One cell, the reader's. At 106f05a5c both cells appeared.
  it('shows one Cursor cell when presence and the reader both speak', async () => {
    const base = parseAnalyticsRemoteResponse(
      JSON.stringify({
        version: 2,
        truncated: false,
        kinds: { cursor: { state: 'ok', fingerprints: { [FILE_1]: print } } },
        rows: [
          {
            k: 'cursor',
            f: FILE_1,
            m: 'grok-4.7',
            h: '2026-10-01 15:00',
            i: 300,
            o: 50,
            cr: 0,
            cw: 0,
            c: 0,
            n: 1,
          },
        ],
        srows: [],
        discovery: {
          kinds: {
            cursor: {
              installed: true,
              signals: ['home'],
              roots: [
                {
                  root: ROOT,
                  origin: 'default',
                  state: 'ok',
                  counts: { filesFound: 1, newestMtime: null },
                },
              ],
              counts: { roots: 1, filesFound: 1, filesRead: 1, filesFailed: 0, newestMtime: null },
            },
          },
          tools: {
            cursor: {
              installed: true,
              signals: ['home'],
              cause: 'no_local_usage_records',
              counts: { filesFound: 1, newestMtime: null },
            },
          },
        },
      })
    );
    const loaded = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: cache,
      now: () => Date.parse('2026-10-02T00:00:00Z'),
      force: true,
      hosts: async () => ({ mac: 'mac-alias', windows: null, nas1: null, alexis: null }),
      localScanner: () => ({ ok: false, cause: 'binary_not_built' }),
      runHelper: async (_alias, _host, request) => ({
        ...base,
        kinds: Object.fromEntries(
          request.kinds.map((kind) => [
            kind,
            base.kinds[kind] ?? { state: 'not_installed', fingerprints: {} },
          ])
        ) as AnalyticsRemoteResponse['kinds'],
        rows: base.rows.filter((entry) => request.kinds.includes(entry.k)),
        srows: base.srows.filter((entry) => request.kinds.includes(entry.k)),
      }),
    });
    const cells = loaded.states.filter((entry) => entry.host === 'mac' && entry.tool === 'cursor');
    expect(cells).toHaveLength(1);
    expect(cells[0].state).toBe('ok');
  });
});
