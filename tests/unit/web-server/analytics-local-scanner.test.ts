/**
 * DESIGN 16.0 B: the scanner reads Codex on the dashboard's own computer too. It catches up into
 * its own cache (a fresh namespace) while the TypeScript collector's numbers stand; its first
 * complete read takes the kind over in one step (the collector's rows dropped as the scanner's are
 * added, and the collector not run for it afterwards); without a usable binary the collector reads
 * it, labelled. Nothing is counted twice at any point, also after a restart.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AccountAnalyticsActivityService,
  type AccountAnalyticsActivityRequest,
} from '../../../src/web-server/services/account-analytics-activity';
import type { AccountAnalyticsQuery } from '../../../src/web-server/services/account-analytics-types';
import {
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
  localScannerOwned,
} from '../../../src/web-server/services/analytics-remote-sources';
import {
  MAX_EXPERIMENT_ROOTS,
  parseAnalyticsRemoteResponse,
  type AnalyticsRemoteRequest,
} from '../../../src/web-server/services/analytics-remote-transport';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';

const NOW = Date.parse('2026-11-13T16:30:00Z');
const MIN_DATE = NOW - 31 * 86_400_000;
const FILE = 'a1'.repeat(32);
const SESSION = 'c3'.repeat(8);
const PRINT = {
  size: 1000,
  mtimeMs: 1_793_000_000_000,
  head: 'a'.repeat(64),
  tail: 'b'.repeat(64),
};

let dir: string;
let savedCcs: string | undefined;
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-local-scanner-'));
  savedCcs = process.env.CCS_HOME;
  process.env.CCS_HOME = path.join(dir, 'ccs');
});
afterEach(() => {
  if (savedCcs === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = savedCcs;
  fs.rmSync(dir, { recursive: true, force: true });
});

/** The scanner's answer for this computer's Codex: part-read, or read to the end. */
function codexAnswer(complete: boolean, input = 150) {
  return parseAnalyticsRemoteResponse(
    JSON.stringify({
      version: 2,
      truncated: !complete,
      kinds: {
        cursor: { state: 'not_installed', fingerprints: {} },
        devin: { state: 'not_installed', fingerprints: {} },
        codex: complete
          ? { state: 'ok', fingerprints: { [FILE]: PRINT } }
          : {
              state: 'ok',
              partial: true,
              fingerprints: {
                [FILE]: {
                  ...PRINT,
                  readThrough: 500,
                  readName: 'rollout-2026-11-03T03-38-31-0123456789abcdef.jsonl.zst',
                },
              },
              unfinished: [
                {
                  name: 'rollout-2026-11-03T03-38-31-0123456789abcdef.jsonl.zst',
                  sizeBytes: 1000,
                  readThroughBytes: 500,
                  decodedBytes: 2000,
                  reason: 'deadline',
                },
              ],
              unfinishedCount: 1,
            },
      },
      rows: [
        {
          k: 'codex',
          f: FILE,
          m: 'gpt-6.1-sol',
          h: '2026-11-13 15:00',
          i: input,
          o: 5,
          cr: 0,
          cw: 0,
          c: 0,
          n: 3,
        },
      ],
      srows: [
        {
          k: 'codex',
          f: FILE,
          s: SESSION,
          m: 'gpt-6.1-sol',
          a: Date.parse('2026-11-13T15:10:00Z'),
          z: Date.parse('2026-11-13T15:20:00Z'),
          i: input,
          o: 5,
          cr: 0,
          cw: 0,
          c: 0,
          n: 3,
        },
      ],
      reader: {
        name: 'aac-usage-scan',
        version: '0.1.0',
        protocol: 3,
        target: 'x86_64-unknown-linux-gnu',
      },
    })
  );
}

const noRemote = async () => ({ mac: null, windows: null, nas1: null, alexis: null });

describe('the scanner on this computer', () => {
  it('checks freshness with the native reader for every local Analytics tool', async () => {
    const asked: string[] = [];
    const answer = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: path.join(dir, 'all-tools'),
      hosts: noRemote,
      force: true,
      now: () => NOW,
      localScanner: () => ({ ok: true }),
      runLocal: async (request) => {
        asked.push(...request.kinds);
        return parseAnalyticsRemoteResponse(
          JSON.stringify({
            version: 2,
            truncated: false,
            kinds: Object.fromEntries(
              request.kinds.map((kind) => [
                kind,
                {
                  state: 'ok',
                  fingerprints: {
                    [FILE]: { ...PRINT, ...(kind === 'claude' ? { claudeGroup: FILE } : {}) },
                  },
                  ...(kind === 'claude' ? { replacedFiles: [FILE] } : {}),
                },
              ])
            ),
            rows: request.kinds.map((kind) => ({
              k: kind,
              f: FILE,
              m: 'fixture-model',
              h: '2026-11-13 15:00',
              i: 100,
              o: 5,
              cr: 0,
              cw: 0,
              c: 0,
              n: 3,
            })),
            srows: [],
            discovery: {
              kinds: Object.fromEntries(
                request.kinds.map((kind) => [
                  kind,
                  {
                    installed: true,
                    roots: [],
                    signals: ['home'],
                    counts: {
                      roots: 0,
                      filesFound: 1,
                      filesRead: 1,
                      filesFailed: 0,
                      newestMtime: NOW,
                    },
                    freshness: {
                      state: 'stale',
                      newestDataMs: NOW - 86400000,
                      newestActivityMs: NOW,
                      activityComplete: true,
                    },
                  },
                ])
              ),
            },
          })
        );
      },
    });
    expect([...new Set(asked)].sort()).toEqual([
      'antigravity',
      'claude',
      'codex',
      'cursor',
      'devin',
      'muse',
      'omp',
      'zcode',
    ]);
    for (const kind of new Set(asked)) {
      const cell = answer.states.find((entry) => entry.host === 'ubuntu' && entry.tool === kind);
      expect(cell).toMatchObject({ state: 'stale', cause: 'newer_activity', rowCount: 3 });
      expect(cell?.detail).toContain(new Date(NOW - 86400000).toISOString());
      expect(cell?.detail).toContain(new Date(NOW).toISOString());
    }
  });
  // Sol M7 (FIX-ROUND-BIG 6): more experiment folders than the scanner takes, after it took Codex
  // over, hands Codex back to the built-in reader, which then reads it; never neither reader.
  it('hands a kind back to the built-in reader when its roots overflow after a take-over', async () => {
    const cache = path.join(dir, 'cache');
    let roots: string[] = ['/home/u/lab/run/codex'];
    const load = () =>
      loadAnalyticsRemoteSources(MIN_DATE, {
        cacheDir: cache,
        hosts: noRemote,
        now: () => NOW,
        force: true,
        localScanner: () => ({ ok: true }),
        localExperimentRoots: () => ({ codex: roots }),
        runLocal: async () => codexAnswer(true),
      });
    expect((await load()).local.owned).toContain('codex');
    roots = Array.from(
      { length: MAX_EXPERIMENT_ROOTS + 1 },
      (_, n) => `/home/u/lab/run-${n}/codex`
    );
    const over = await load();
    expect(over.local.owned).not.toContain('codex');
    expect(over.local.tooManyRoots).toContain('codex');
    const saved = JSON.parse(
      fs.readFileSync(path.join(cache, 'analytics-remote-v1', 'ubuntu.json'), 'utf8')
    ) as { owned?: Record<string, string> };
    expect(saved.owned?.codex).toBeUndefined();
  });

  it('catches up apart, takes the kind over at its first complete read, and serves it after a restart only while it can run', async () => {
    const cache = path.join(dir, 'cache');
    const asked: AnalyticsRemoteRequest[] = [];
    let complete = false;
    const load = () =>
      loadAnalyticsRemoteSources(MIN_DATE, {
        cacheDir: cache,
        hosts: noRemote,
        now: () => NOW,
        force: true,
        localScanner: () => ({ ok: true }),
        localExperimentRoots: () => ({ codex: ['/home/u/t3setup/retained-usage/run/codex'] }),
        runLocal: async (request) => {
          asked.push(request);
          return codexAnswer(complete);
        },
      });
    // Catching up: nothing of the scanner's Codex counts yet, and the collector's cell is told why.
    // Cursor and Devin have no collector to replace: the scanner owns them from its first answer.
    const first = await load();
    const ubuntu = (answer: typeof first) =>
      answer.states.filter((entry) => entry.host === 'ubuntu').map((entry) => entry.tool);
    expect(first.results).toEqual([]);
    expect(ubuntu(first)).toEqual(['devin']);
    expect(first.local.owned.sort()).toEqual(['cursor', 'devin']);
    expect(first.local.catchingUp.codex).toContain('taking over');
    expect(first.local.catchingUp.codex).toContain(
      'rollout-2026-11-03T03-38-31-0123456789abcdef.jsonl.zst'
    );
    // A cold computer gets one call per kind; each carries the experiment folders.
    expect(asked.flatMap((request) => request.kinds).sort()).toEqual([
      'antigravity',
      'claude',
      'codex',
      'cursor',
      'devin',
      'muse',
      'omp',
      'zcode',
    ]);
    expect(asked.find((request) => request.kinds.includes('codex'))?.experimentRoots).toEqual({
      codex: ['/home/u/t3setup/retained-usage/run/codex'],
    });
    expect(localScannerOwned(cache).has('codex')).toBe(false);
    // The first complete read: owned from now on, its rows and cell served.
    complete = true;
    const second = await load();
    expect(second.local.owned.sort()).toEqual(['codex', 'cursor', 'devin']);
    expect(second.results.map((entry) => entry.tool)).toEqual(['codex']);
    expect(
      second.states.find((entry) => entry.host === 'ubuntu' && entry.tool === 'codex')
    ).toMatchObject({
      state: 'ok',
      rowCount: 3,
    });
    expect([...localScannerOwned(cache)].sort()).toEqual(['codex', 'cursor', 'devin']);
    // After a restart: the saved answer counts while the scanner can run, and not otherwise (the
    // collector reads Codex again then, so serving both would count it twice).
    const ready = loadAnalyticsRemoteCachedSources(MIN_DATE, {
      cacheDir: cache,
      localScanner: () => ({ ok: true }),
    });
    expect(ready.local.owned.sort()).toEqual(['codex', 'cursor', 'devin']);
    expect(ready.results.map((entry) => entry.tool)).toEqual(['codex']);
    const gone = loadAnalyticsRemoteCachedSources(MIN_DATE, {
      cacheDir: cache,
      localScanner: () => ({ ok: false, cause: 'binary_missing' }),
    });
    expect(gone.local.owned).toEqual([]);
    expect(gone.results).toEqual([]);
    // Without a usable binary the computer is not scanned at all, and the answer says why.
    const none = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: cache,
      hosts: noRemote,
      now: () => NOW,
      force: true,
      localScanner: () => ({ ok: false, cause: 'binary_not_built' }),
      runLocal: async () => {
        throw new Error('must not run');
      },
    });
    expect(none.local).toEqual({
      owned: [],
      catchingUp: {},
      unavailable: 'binary_not_built',
      freshness: {},
    });
    expect(none.results).toEqual([]);
  });
});

function data(input: number): UsageWorkerResult {
  return {
    daily: [],
    monthly: [],
    eventCount: 3,
    hourly: [
      {
        hour: '2026-11-13 15:00',
        source: 'native',
        inputTokens: input,
        outputTokens: 5,
        cacheCreationTokens: 0,
        cacheReadTokens: 0,
        cost: 0,
        totalCost: 0,
        modelsUsed: ['gpt-6.1-sol'],
        requestCount: 3,
        modelBreakdowns: [
          {
            modelName: 'gpt-6.1-sol',
            inputTokens: input,
            outputTokens: 5,
            cacheCreationTokens: 0,
            cacheReadTokens: 0,
            cost: 0,
          },
        ],
      },
    ],
    session: [],
  } as unknown as UsageWorkerResult;
}

const QUERY: AccountAnalyticsQuery = {
  platform: 'mac',
  range: '7d',
  provider: 'all',
  account: 'all',
};

describe('the switch-over in the activity service', () => {
  it('counts Codex once: the collector while the scanner catches up, the scanner from the step it owns it', async () => {
    const scope = path.join(dir, 'scope');
    fs.mkdirSync(scope, { recursive: true });
    let owned = false;
    const workers: string[] = [];
    const codex: AccountAnalyticsActivityRequest = {
      provider: 'codex',
      request: { kind: 'codex', codexHome: '/fixture-codex-home' },
    } as AccountAnalyticsActivityRequest;
    const service = new AccountAnalyticsActivityService({
      scope: () => scope,
      now: () => NOW,
      requests: () => [codex],
      loadWorker: async (request) => {
        workers.push(request.kind);
        return data(100);
      },
      localScannerOwned: () => (owned ? ['codex'] : []),
      remote: async () =>
        owned
          ? {
              fetchedAt: new Date(NOW).toISOString(),
              results: [{ tool: 'codex', data: data(150) }],
              states: [
                {
                  tool: 'codex',
                  host: 'ubuntu',
                  state: 'ok',
                  lastScanAt: null,
                  rowCount: 3,
                  detail: null,
                },
              ],
              local: { owned: ['codex'], catchingUp: {} },
            }
          : {
              fetchedAt: new Date(NOW).toISOString(),
              results: [],
              states: [],
              local: { owned: [], catchingUp: { codex: 'the usage scanner is taking over' } },
            },
    });
    const settled = async () => {
      for (let attempt = 0; attempt < 500; attempt++) {
        const answer = await service.get(QUERY, NOW - 86_400_000, NOW);
        if (answer.refreshing !== true) return answer;
        await new Promise((resolve) => setTimeout(resolve, 5));
      }
      throw new Error('collection did not settle');
    };
    const refresh = async () => {
      await service.get({ ...QUERY, refresh: true }, NOW - 86_400_000, NOW);
      return settled();
    };
    try {
      // Catching up: the collector's 100, its cell saying the scanner is taking over.
      const before = await settled();
      expect(before.totals?.inputTokens).toBe(100);
      const cells = before.sources.filter(
        (entry) => entry.tool === 'codex' && entry.host === 'ubuntu'
      );
      expect(cells).toHaveLength(1);
      expect(cells[0].detail).toContain('the usage scanner is taking over');
      // The step: the scanner's 150 replaces the collector's 100 (never 250), one cell.
      owned = true;
      workers.length = 0;
      const after = await refresh();
      expect(after.totals?.inputTokens).toBe(150);
      expect(
        after.sources.filter((entry) => entry.tool === 'codex' && entry.host === 'ubuntu')
      ).toHaveLength(1);
      // And the collector is no longer run for it.
      expect(workers).not.toContain('codex');
    } finally {
      await service.shutdown();
    }
  });
});
