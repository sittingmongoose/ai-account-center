/** Service-level coverage evidence must survive numeric ownership, fallback and restart. */
import { afterEach, beforeEach, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  AccountAnalyticsActivityService,
  type AccountAnalyticsActivityRequest,
} from '../../../src/web-server/services/account-analytics-activity';
import {
  loadAnalyticsRemoteSources,
  loadAnalyticsRemoteCachedSources,
} from '../../../src/web-server/services/analytics-remote-sources';
import { parseAnalyticsRemoteResponse } from '../../../src/web-server/services/analytics-remote-transport';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';

const NOW = Date.parse('2026-11-15T23:00:00Z');
const OLD = NOW - 86400000;
const FILE = 'a'.repeat(64);
const query = { platform: 'ubuntu', range: '7d', provider: 'all', account: 'all' } as const;
let root: string;
let savedCcs: string | undefined;
beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'ancur-local-fresh-'));
  savedCcs = process.env.CCS_HOME;
  process.env.CCS_HOME = root;
});
afterEach(() => {
  if (savedCcs === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = savedCcs;
  fs.rmSync(root, { recursive: true, force: true });
});

function response(activityComplete = true) {
  return parseAnalyticsRemoteResponse(
    JSON.stringify({
      version: 2,
      truncated: false,
      kinds: Object.fromEntries(
        ['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity', 'cursor', 'devin'].map((k) => [
          k,
          k === 'omp'
            ? {
                state: 'ok',
                partial: true,
                optionalSearchLimited: true,
                unconfirmedFiles: 1,
                fingerprints: { [FILE]: { size: 10, mtimeMs: NOW } },
              }
            : { state: 'not_installed', fingerprints: {} },
        ])
      ),
      rows: [
        {
          k: 'omp',
          f: FILE,
          m: 'fixture-model',
          h: '2026-11-14 23:00',
          i: 999,
          o: 0,
          cr: 0,
          cw: 0,
          c: 0,
          n: 3,
        },
      ],
      srows: [],
      discovery: {
        kinds: {
          omp: {
            installed: true,
            signals: ['home'],
            roots: [],
            counts: { roots: 0, filesFound: 2, filesRead: 1, filesFailed: 0, newestMtime: NOW },
            freshness: {
              state: 'stale',
              newestDataMs: OLD,
              newestActivityMs: NOW,
              activityComplete,
            },
          },
        },
      },
    })
  );
}

function data(at?: number): UsageWorkerResult {
  return {
    daily: [],
    monthly: [],
    eventCount: 3,
    hourly: [
      {
        hour: new Date(at ?? OLD).toISOString().slice(0, 13).replace('T', ' ') + ':00',
        source: 'omp-native',
        inputTokens: 100,
        outputTokens: 0,
        cacheCreationTokens: 0,
        cacheReadTokens: 0,
        cost: 0,
        totalCost: 0,
        modelsUsed: ['fixture-model'],
        requestCount: 3,
        modelBreakdowns: [
          {
            modelName: 'fixture-model',
            inputTokens: 100,
            outputTokens: 0,
            cacheCreationTokens: 0,
            cacheReadTokens: 0,
            cost: 0,
          },
        ],
      },
    ],
    session:
      at === undefined
        ? []
        : [
            {
              sessionId: 'fixture-session',
              projectPath: '',
              source: 'omp-native',
              inputTokens: 100,
              outputTokens: 0,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              cost: 0,
              modelsUsed: ['fixture-model'],
              modelBreakdowns: [],
              firstActivity: new Date(at).toISOString(),
              lastActivity: new Date(at).toISOString(),
              versions: [],
            },
          ],
  } as UsageWorkerResult;
}

async function settle(service: AccountAnalyticsActivityService) {
  for (let n = 0; n < 500; n++) {
    const result = await service.get(query, NOW - 2 * 86400000, NOW);
    if (result.refreshing !== true) return result;
    await new Promise((r) => setTimeout(r, 5));
  }
  throw Error('fixture did not settle');
}

it.each([false, true])(
  'publishes stale dates through partial takeover, restart and fallback (retained Claude: %s)',
  async (retainClaude) => {
    const cacheDir = path.join(root, 'cache');
    const partial = await loadAnalyticsRemoteSources(OLD - 86400000, {
      cacheDir,
      force: true,
      now: () => NOW,
      hosts: async () => ({ mac: null, windows: null, nas1: null, alexis: null, odroid: null, '1070ti': null }),
      localScanner: () => ({ ok: true }),
      runLocal: async () => response(),
    });
    expect(partial.local.owned).not.toContain('omp');
    const make = (remote: typeof partial) =>
      new AccountAnalyticsActivityService({
        scope: () => root,
        now: () => NOW,
        requests: () => [
          {
            provider: 'omp',
            request: { kind: 'omp', sessionRoots: [] },
          } as unknown as AccountAnalyticsActivityRequest,
          ...(retainClaude
            ? [
                {
                  provider: 'claude',
                  request: { kind: 'claude', projectsDirs: [] },
                } as unknown as AccountAnalyticsActivityRequest,
              ]
            : []),
        ],
        loadWorker: async (request) => {
          if (request.kind === 'claude')
            throw Error('fixture collector failure retains the Claude pair');
          return data();
        },
        localScannerOwned: () => [],
        remote: async () => remote,
        remoteCached: () => remote,
        persistSnapshot: true,
      });
    const assertCell = (answer: Awaited<ReturnType<typeof settle>>) => {
      expect(answer.totals?.inputTokens).toBe(100);
      const cells = answer.sources.filter((s) => s.host === 'ubuntu' && s.tool === 'omp');
      expect(cells).toHaveLength(1);
      expect(cells[0]).toMatchObject({ state: 'stale', cause: 'newer_activity', rowCount: 3 });
      expect(cells[0].detail).toContain(new Date(OLD).toISOString());
      expect(cells[0].detail).toContain(new Date(NOW).toISOString());
    };
    let service = make(partial);
    try {
      assertCell(await settle(service));
    } finally {
      await service.shutdown();
    }
    // Cache-only restart must carry evidence even while no numeric tool is owned.
    const cached = loadAnalyticsRemoteCachedSources(OLD - 86400000, {
      cacheDir,
      localScanner: () => ({ ok: false, cause: 'binary_missing' }),
    });
    expect(cached.local.owned).toEqual([]);
    service = make({
      ...cached,
      fetchedAt: cached.fetchedAt ?? new Date(NOW).toISOString(),
    } as typeof partial);
    try {
      assertCell(await settle(service));
    } finally {
      await service.shutdown();
    }
    const unavailable = await loadAnalyticsRemoteSources(OLD - 86400000, {
      cacheDir,
      force: true,
      now: () => NOW,
      hosts: async () => ({ mac: null, windows: null, nas1: null, alexis: null, odroid: null, '1070ti': null }),
      localScanner: () => ({ ok: false, cause: 'binary_missing' }),
      runLocal: async () => {
        throw Error('unavailable scanner must not run');
      },
    });
    service = make(unavailable);
    try {
      assertCell(await settle(service));
    } finally {
      await service.shutdown();
    }
  }
);

it.each([
  [NOW, true, 'ok'],
  [NOW - 3600000, true, 'stale'],
  [NOW, false, 'cached'],
] as const)(
  'reconciles collector event %d with complete activity %s as %s',
  async (at, complete, expected) => {
    const partial = await loadAnalyticsRemoteSources(OLD - 86400000, {
      cacheDir: path.join(root, 'cache'),
      force: true,
      now: () => NOW,
      hosts: async () => ({ mac: null, windows: null, nas1: null, alexis: null, odroid: null, '1070ti': null }),
      localScanner: () => ({ ok: true }),
      runLocal: async () => response(complete),
    });
    const service = new AccountAnalyticsActivityService({
      scope: () => root,
      now: () => NOW,
      requests: () => [
        {
          provider: 'omp',
          request: { kind: 'omp', sessionRoots: [] },
        } as unknown as AccountAnalyticsActivityRequest,
      ],
      loadWorker: async () => data(at),
      localScannerOwned: () => [],
      remote: async () => partial,
    });
    try {
      const answer = await settle(service);
      expect(answer.totals?.inputTokens).toBe(100);
      const cell = answer.sources.find((s) => s.host === 'ubuntu' && s.tool === 'omp');
      expect(cell?.state).toBe(expected);
      if (expected === 'stale') {
        expect(cell?.detail).toContain(new Date(at).toISOString());
        expect(cell?.detail).toContain(new Date(NOW).toISOString());
      } else {
        expect(cell?.detail).not.toContain('usage is stale');
        if (!complete) expect(cell?.cause).toBe('freshness_unverified');
      }
    } finally {
      await service.shutdown();
    }
  }
);
