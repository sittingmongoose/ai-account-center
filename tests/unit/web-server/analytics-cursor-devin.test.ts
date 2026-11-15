/**
 * DESIGN 16.2 and 16.3 on the server: the scanner's Cursor and Devin answers become cells and
 * usage on every computer it reads; T3's thread binding travels as explicit evidence; a run that
 * fits neither token convention is named, never guessed; Cursor without a run store is left to
 * presence ("No local usage records"), one cell only.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { loadAnalyticsRemoteSources } from '../../../src/web-server/services/analytics-remote-sources';
import { parseAnalyticsRemoteResponse } from '../../../src/web-server/services/analytics-remote-transport';

const NOW = Date.parse('2026-11-13T16:30:00Z');
const MIN_DATE = NOW - 31 * 86_400_000;
const STORE = 'd4'.repeat(32);
const DEVIN = 'e5'.repeat(32);
const AGENT_KEY = 'f6'.repeat(8);

let dir: string;
let savedCcs: string | undefined;
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-cursor-devin-'));
  savedCcs = process.env.CCS_HOME;
  process.env.CCS_HOME = path.join(dir, 'ccs');
});
afterEach(() => {
  if (savedCcs === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = savedCcs;
  fs.rmSync(dir, { recursive: true, force: true });
});

const row = (k: string, f: string, m: string, i: number) => ({
  k,
  f,
  m,
  h: '2026-11-11 21:00',
  i,
  o: 10,
  cr: 500,
  cw: 0,
  c: 0,
  n: 1,
});

function answer(
  cursor: Record<string, unknown>,
  rows: unknown[],
  sessions?: unknown,
  extra: Record<string, unknown> = {}
) {
  return parseAnalyticsRemoteResponse(
    JSON.stringify({
      version: 2,
      truncated: false,
      kinds: {
        claude: { state: 'not_installed', fingerprints: {} },
        codex: { state: 'not_installed', fingerprints: {} },
        omp: { state: 'not_installed', fingerprints: {} },
        muse: { state: 'not_installed', fingerprints: {} },
        zcode: { state: 'not_installed', fingerprints: {} },
        antigravity: { state: 'not_installed', fingerprints: {} },
        cursor,
        devin: {
          state: 'ok',
          fingerprints: { [DEVIN]: { size: 9, mtimeMs: 9, walSize: 0, walMtimeMs: 0 } },
        },
      },
      rows,
      srows: sessions
        ? [
            {
              k: 'cursor',
              f: STORE,
              s: AGENT_KEY,
              m: 'grok-4.7',
              a: Date.parse('2026-11-11T21:30:00Z'),
              z: Date.parse('2026-11-11T21:33:00Z'),
              i: 300,
              o: 10,
              cr: 500,
              cw: 0,
              c: 0,
              n: 1,
            },
          ]
        : [],
      ...(sessions ? { sessions } : {}),
      ...extra,
    })
  );
}

describe('Cursor and Devin on a scanned computer', () => {
  it('projects native stale evidence with both clocks and preserves it on cached reads', async () => {
    const reply = answer(
      { state: 'ok', fingerprints: { [STORE]: { size: 1, mtimeMs: 1 } } },
      [row('cursor', STORE, 'grok-4.7', 300)],
      undefined,
      {
        discovery: {
          kinds: {
            cursor: {
              installed: true,
              signals: ['home'],
              roots: [],
              counts: { roots: 0, filesFound: 1, filesRead: 1, filesFailed: 0, newestMtime: NOW },
              freshness: {
                state: 'stale',
                newestDataMs: Date.parse('2026-11-11T21:00:00Z'),
                newestActivityMs: NOW,
              },
              unmeasuredFiles: 1,
            },
          },
        },
      }
    );
    const deps = {
      cacheDir: path.join(dir, 'stale-cache'),
      hosts: async () => ({ mac: null, windows: null, nas1: 'nas1-agent', alexis: null }),
      now: () => NOW,
      localScanner: () => ({ ok: false as const, cause: 'binary_not_built' }),
      runHelper: async () => reply,
    };
    const first = await loadAnalyticsRemoteSources(MIN_DATE, { ...deps, force: true });
    const cell = first.states.find((s) => s.host === 'nas1' && s.tool === 'cursor');
    expect(cell).toMatchObject({ state: 'stale', cause: 'newer_activity', rowCount: 1 });
    expect(cell?.detail).toContain('2026-11-11T21:00:00.000Z');
    expect(cell?.detail).toContain('2026-11-13T16:30:00.000Z');
    expect(cell?.detail).toContain('no numeric usage');
    const again = await loadAnalyticsRemoteSources(MIN_DATE, deps);
    expect(again.states.find((s) => s.host === 'nas1' && s.tool === 'cursor')).toMatchObject({
      state: 'stale',
      cause: 'newer_activity',
    });
  });

  it('counts both, names unreadable runs, and keeps the T3 binding as explicit evidence', async () => {
    const cache = path.join(dir, 'cache');
    const reply = answer(
      {
        state: 'ok',
        partial: true,
        unreadableRecords: 2,
        fingerprints: { [STORE]: { size: 1, mtimeMs: 1, walSize: 0, walMtimeMs: 0 } },
      },
      [row('cursor', STORE, 'grok-4.7', 300), row('devin', DEVIN, 'swe-2-max', 1500)],
      {
        [AGENT_KEY]: {
          hash: `${AGENT_KEY}${'0'.repeat(48)}`,
          cwd: null,
          t3: {
            threadId: 'thread-cursor-1',
            providerInstanceId: 'cursor-instance-1',
            identitySource: 't3_native_ref',
            confidence: 'explicit',
          },
        },
      }
    );
    expect(reply.sessions?.[AGENT_KEY]?.t3?.threadId).toBe('thread-cursor-1');
    const { results, states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: cache,
      hosts: async () => ({ mac: null, windows: null, nas1: 'nas1-agent', alexis: null }),
      now: () => NOW,
      force: true,
      localScanner: () => ({ ok: false, cause: 'binary_not_built' }),
      runHelper: async () => reply,
    });
    expect(results.map((entry) => entry.tool).sort()).toEqual(['cursor', 'devin']);
    const cell = (tool: string) =>
      states.find((entry) => entry.host === 'nas1' && entry.tool === tool);
    expect(cell('devin')).toMatchObject({ state: 'ok', rowCount: 1 });
    expect(cell('cursor')?.detail).toContain('2 Cursor runs could not be read');
    expect(cell('cursor')?.detail).toContain('never guessed');
    const saved = JSON.parse(
      fs.readFileSync(path.join(cache, 'analytics-remote-v1', 'nas1.json'), 'utf8')
    );
    expect(saved.sessions[AGENT_KEY].t3).toMatchObject({ identitySource: 't3_native_ref' });
  });

  it('leaves Cursor without a run store to presence: one cell, "no local usage records"', async () => {
    const withPresence = answer({ state: 'not_installed', fingerprints: {} }, [], undefined, {
      discovery: {
        kinds: {},
        tools: {
          cursor: {
            installed: true,
            signals: ['home'],
            cause: 'no_local_usage_records',
            counts: { filesFound: 1, newestMtime: null },
          },
        },
      },
    });
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: path.join(dir, 'cache'),
      hosts: async () => ({ mac: null, windows: null, nas1: 'nas1-agent', alexis: null }),
      now: () => NOW,
      force: true,
      localScanner: () => ({ ok: false, cause: 'binary_not_built' }),
      runHelper: async () => withPresence,
    });
    const cursor = states.filter((entry) => entry.host === 'nas1' && entry.tool === 'cursor');
    expect(cursor).toHaveLength(1);
    expect(cursor[0]).toMatchObject({ state: 'no_reader', cause: 'no_local_usage_records' });
  });

  it('names a linked log folder that did not answer, never leaving it out silently', async () => {
    const reply = answer({ state: 'not_installed', fingerprints: {} }, [], undefined, {
      discovery: {
        kinds: {
          codex: {
            installed: true,
            signals: ['home'],
            roots: [
              {
                root: 'a'.repeat(16),
                origin: 'default',
                state: 'ok',
                counts: { filesFound: 0, newestMtime: null },
              },
              {
                root: 'b'.repeat(16),
                origin: 'default',
                state: 'link_unreachable',
                counts: { filesFound: 0, newestMtime: null },
              },
            ],
            counts: { roots: 2, filesFound: 0, filesRead: 0, filesFailed: 0, newestMtime: null },
          },
        },
      },
    });
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: path.join(dir, 'cache'),
      hosts: async () => ({ mac: null, windows: null, nas1: 'nas1-agent', alexis: null }),
      now: () => NOW,
      force: true,
      localScanner: () => ({ ok: false, cause: 'binary_not_built' }),
      runHelper: async () => reply,
    });
    const codex = states.find((entry) => entry.host === 'nas1' && entry.tool === 'codex');
    expect(codex?.detail).toContain('1 linked log folder did not answer');
  });
});
