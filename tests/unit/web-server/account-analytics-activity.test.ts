import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AccountAnalyticsActivityService,
  coldScanningSourceStates,
  fixedAnalyticsSourceEntries,
  loadAccountAnalyticsWorker,
  projectAccountAnalyticsActivity,
} from '../../../src/web-server/services/account-analytics-activity';
import type { AccountAnalyticsActivityRequest } from '../../../src/web-server/services/account-analytics-activity';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';
import { createHash } from 'crypto';
import { collectAccountActivity } from '../../../src/web-server/usage/account-activity-collector';
import {
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
} from '../../../src/web-server/services/analytics-remote-sources';
import { parseAnalyticsRemoteResponse } from '../../../src/web-server/services/analytics-remote-transport';
import type { AccountAnalyticsQuery } from '../../../src/web-server/services/account-analytics-types';
import type {
  UsageWorkerRequest,
  UsageWorkerResult,
} from '../../../src/web-server/usage/worker-client';
import { codexFileRevision } from '../../../src/web-server/usage/worker-client';

const NOW = Date.parse('2026-10-01T16:30:00Z');
let fixtureConfig: string;
let previousConfig: Record<string, string | undefined>;
beforeEach(() => {
  previousConfig = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
  fixtureConfig = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-reporting-config-'));
  process.env.CCS_HOME = fixtureConfig;
  delete process.env.CCS_DIR;
});
afterEach(() => {
  for (const [key, value] of Object.entries(previousConfig)) {
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  fs.rmSync(fixtureConfig, { recursive: true, force: true });
});
const FROM = NOW - 86_400_000;
const QUERY: AccountAnalyticsQuery = {
  platform: 'mac',
  range: '24h',
  provider: 'all',
  account: 'all',
};
function data(model: string, input: number, cost: number, route?: string): UsageWorkerResult {
  return {
    daily: [],
    monthly: [],
    eventCount: 99,
    hourly: [
      {
        hour: '2026-10-01 15:00',
        source: 'native',
        inputTokens: input,
        outputTokens: 5,
        cacheCreationTokens: 2,
        cacheReadTokens: 3,
        cost,
        totalCost: cost,
        modelsUsed: [model],
        requestCount: 4,
        modelBreakdowns: [
          {
            modelName: model,
            inputTokens: input,
            outputTokens: 5,
            cacheCreationTokens: 2,
            cacheReadTokens: 3,
            cost,
            ...(route !== undefined && { provider: route }),
          },
        ],
      },
    ],
    session: [
      {
        sessionId: 'shared-id',
        projectPath: '/private/project',
        inputTokens: input,
        outputTokens: 5,
        cacheCreationTokens: 2,
        cacheReadTokens: 3,
        cost,
        totalCost: cost,
        lastActivity: '2026-10-01T15:45:00Z',
        versions: [],
        modelsUsed: [model],
        modelBreakdowns: [],
        source: 'native',
      },
    ],
  };
}
function sources() {
  return [
    {
      provider: 'claude' as const,
      data: [data('claude-sonnet-4-6', 100, 0.005)],
      fetchedAt: new Date(NOW).toISOString(),
    },
    {
      provider: 'codex' as const,
      data: [data('gpt-5.4', 200, 0.01)],
      fetchedAt: new Date(NOW).toISOString(),
    },
  ];
}

describe('native local analytics activity', () => {
  // A refresh answers instantly from the last snapshot and runs behind it; poll until the answer
  // is settled (refreshing is false) to assert what the refresh collected.
  async function settled(
    service: AccountAnalyticsActivityService,
    query: AccountAnalyticsQuery = QUERY,
    from: number = FROM,
    to: number = NOW
  ) {
    for (let attempt = 0; attempt < 500; attempt++) {
      const answer = await service.get(query, from, to);
      if (answer.refreshing !== true) return answer;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    throw new Error('background collection did not settle');
  }

  it.each([false, true])(
    'answers while a large compressed read runs, shows its detail, and cancels it on shutdown (empty: %s)',
    async (empty) => {
      const file = path.join(fixtureConfig, 'rollout-fixture.jsonl.zst');
      fs.writeFileSync(file, 'synthetic fixture');
      const spec = {
        file,
        checkpoint: path.join(fixtureConfig, 'checkpoint'),
        revision: codexFileRevision(fs.statSync(file)),
        minDate: FROM,
      };
      let running = false;
      let cancelled = false;
      const service = new AccountAnalyticsActivityService({
        persistSnapshot: true,
        remote: async () => ({ results: [], states: [] }),
        now: () => NOW,
        scope: () => fixtureConfig,
        requests: () => [
          {
            provider: 'codex',
            request: { kind: 'codex', codexHome: fixtureConfig, cacheDir: fixtureConfig },
          },
        ],
        loadWorker: async () => ({
          ...data('gpt-5.4', 100, 0),
          ...(empty ? { eventCount: 0, hourly: [], session: [] } : {}),
          scan: {
            complete: false,
            completedFiles: 0,
            totalFiles: 1,
            skippedLines: 0,
            failedFiles: 0,
            readBytes: 0,
            largeCodexReads: [spec],
          },
        }),
        largeCodexReader: async (_spec, signal) => {
          running = true;
          await new Promise<void>((resolve) =>
            signal.addEventListener(
              'abort',
              () => {
                cancelled = true;
                resolve();
              },
              { once: true }
            )
          );
          return undefined;
        },
      });
      try {
        await settled(service);
        for (let tries = 0; tries < 100 && !running; tries++)
          await new Promise((resolve) => setTimeout(resolve, 5));
        expect(running).toBe(true);
        // The background promise remains unresolved while Analytics serves its normal answer.
        const answer = await settled(service);
        expect(answer.status).toBe(empty ? 'unavailable' : 'cached');
        const snapshot = fs.readFileSync(
          path.join(
            fixtureConfig,
            'cache',
            'account-activity-v1',
            'analytics-activity-snapshot-v1.json'
          ),
          'utf8'
        );
        expect(snapshot).not.toContain(spec.file);
        expect(snapshot).not.toContain('largeCodexReads');
        expect(
          answer.sources.find((source) => source.tool === 'codex' && source.host === 'ubuntu')
            ?.detail
        ).toBe('reading 1 large compressed Codex log in the background');
        await service.shutdown();
        expect(cancelled).toBe(true);
        expect(
          (await settled(service)).sources.find(
            (source) => source.tool === 'codex' && source.host === 'ubuntu'
          )?.detail
        ).not.toContain('reading 1 large');
      } finally {
        await service.shutdown();
      }
    }
  );
  it('reports bounded root discovery as partial even when every found file was read', async () => {
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      now: () => NOW,
      scope: () => '/fixture-discovery-limited',
      requests: () => [
        {
          provider: 'omp',
          request: {
            kind: 'omp',
            roots: [],
            activity: { minDate: FROM, cacheDir: '/fixture/cache', discoveryTruncated: true },
          },
        },
      ],
      loadWorker: async () => data('k3', 100, 1),
    });
    const result = await settled(service);
    expect(result.status).toBe('cached');
    const source = result.sources.find((entry) => entry.tool === 'omp' && entry.host === 'ubuntu');
    expect(source?.state).toBe('cached');
    expect(source?.detail).toContain('usage-root discovery');
  });
  it("names a collector's reason for a lasting partial scan on the cell", async () => {
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      now: () => NOW,
      scope: () => '/fixture-compressed-stall',
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => ({
        ...data('gpt-5.4', 100, 0.01),
        scan: {
          complete: false,
          completedFiles: 1,
          totalFiles: 2,
          skippedLines: 0,
          failedFiles: 0,
          readBytes: 100,
          unfinishedFiles: 0,
          detail: '1 compressed Codex log is too large to finish within one analytics pass',
        } as NonNullable<ReturnType<typeof data>['scan']>,
      }),
    });
    const result = await settled(service);
    const source = result.sources.find(
      (entry) => entry.tool === 'codex' && entry.host === 'ubuntu'
    );
    expect(source?.state).toBe('cached');
    expect(source?.detail).toContain('too large to finish within one analytics pass');
  });

  it('retains each local request independently of successful siblings and remote rows', async () => {
    for (const outcome of ['reject', 'empty', 'partial-empty', 'skip'] as const) {
      let pass = 0;
      let wall = Date.now();
      const clock = spyOn(Date, 'now').mockImplementation(() => wall);
      try {
        const service = new AccountAnalyticsActivityService({
          now: () => NOW + pass * 1000,
          scope: () => `request-retention-${outcome}`,
          concurrency: 1,
          requests: () => [
            { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/a' } },
            { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/b' } },
          ],
          remote: async () => ({
            results: [{ tool: 'claude', data: data('claude-sonnet-4-6', 777, 0) }],
            states: [],
          }),
          loadWorker: async (request) => {
            const first = request.kind === 'claude' && request.projectsDir.endsWith('/a');
            if (pass && first && outcome === 'skip') wall += 60_001;
            if (pass && !first) {
              if (outcome === 'reject') throw new Error('fixture');
              return {
                daily: [],
                monthly: [],
                hourly: [],
                session: [],
                eventCount: 0,
                ...(outcome === 'partial-empty'
                  ? {
                      scan: {
                        complete: false,
                        completedFiles: 0,
                        totalFiles: 1,
                        failedFiles: 0,
                        skippedLines: 0,
                        readBytes: 0,
                      },
                    }
                  : {}),
              };
            }
            return data('claude-sonnet-4-6', first ? 50_000 : 30_000, 0);
          },
        });
        expect((await settled(service)).totals?.inputTokens).toBe(80_777);
        pass++;
        await service.get({ ...QUERY, refresh: true }, FROM, NOW);
        const answer = await settled(service);
        expect(answer.totals?.inputTokens).toBe(80_777);
        expect(
          answer.sources.find((entry) => entry.tool === 'claude' && entry.host === 'ubuntu')
            ?.rowCount
        ).toBe(198);
      } finally {
        clock.mockRestore();
      }
    }
  });

  it('keeps a single failed local request when a remote of the same tool succeeds', async () => {
    let fail = false;
    const service = new AccountAnalyticsActivityService({
      now: () => NOW,
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture' } },
      ],
      loadWorker: async () => {
        if (fail) throw new Error('fixture');
        return data('claude-sonnet-4-6', 100_000, 0);
      },
      remote: async () => ({
        results: [{ tool: 'claude', data: data('claude-sonnet-4-6', 777, 0) }],
        states: [],
      }),
    });
    expect((await settled(service)).totals?.inputTokens).toBe(100_777);
    fail = true;
    await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect((await settled(service)).totals?.inputTokens).toBe(100_777);
  });

  it('adopts growing readable files while retaining an unreadable file checkpoint', async () => {
    let now = NOW;
    const projectsDir = path.join(fixtureConfig, 'projects');
    fs.mkdirSync(projectsDir);
    const readable = path.join(projectsDir, 'readable.jsonl');
    const unreadable = path.join(projectsDir, 'unreadable.jsonl');
    const line = (id: string, input: number) =>
      JSON.stringify({
        type: 'assistant',
        timestamp: '2026-10-01T15:00:00Z',
        sessionId: id,
        message: {
          id,
          model: 'claude-sonnet-4-6',
          usage: { input_tokens: input, output_tokens: 1 },
        },
      }) + '\n';
    fs.writeFileSync(readable, line('readable-1', 100));
    fs.writeFileSync(unreadable, line('unreadable-1', 40));
    const request: UsageWorkerRequest = { kind: 'claude', projectsDir };
    const service = new AccountAnalyticsActivityService({
      now: () => now,
      requests: () => [{ provider: 'claude', request }],
      remote: async () => ({ results: [], states: [] }),
      loadWorker: async (source) =>
        collectAccountActivity(source, {
          minDate: FROM,
          cacheDir: path.join(fixtureConfig, 'cache'),
        }),
    });
    expect((await settled(service)).totals?.inputTokens).toBe(140);
    fs.appendFileSync(readable, line('readable-2', 100));
    const open = fs.openSync;
    const blocked = spyOn(fs, 'openSync').mockImplementation(((file, ...args) => {
      if (file === unreadable) throw new Error('fixture unreadable');
      return open(file, ...args);
    }) as typeof fs.openSync);
    try {
      for (let pass = 0; pass < 2; pass++) {
        now += 1000;
        await service.get({ ...QUERY, refresh: true }, FROM, NOW);
        const answer = await settled(service);
        expect(answer.totals?.inputTokens).toBe(240);
        expect(
          answer.sources.find((entry) => entry.tool === 'claude' && entry.host === 'ubuntu')
            ?.lastScanAt
        ).toBe(new Date(NOW + 1000).toISOString());
        expect(
          answer.sources.find((entry) => entry.tool === 'claude' && entry.host === 'ubuntu')?.detail
        ).toContain('local scan failed');
      }
    } finally {
      blocked.mockRestore();
    }
    await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect((await settled(service)).totals?.inputTokens).toBe(240);
  });

  it.each([false, true])(
    'restores a pruned original mirror exactly once (remote finishes early: %s)',
    async (early) => {
      let pruned = false;
      let pruneOnRead = false;
      const name = createHash('sha256').update('original.jsonl').digest('hex');
      const file = '11'.repeat(32);
      const remote = () =>
        loadAnalyticsRemoteSources(FROM, {
          cacheDir: path.join(fixtureConfig, 'remote'),
          hosts: async () => ({ mac: 'fixture-mac', windows: 'fixture-windows', nas1: null }),
          claudeLogNames: () => ({ originals: new Set(pruned ? [] : [name]), complete: true }),
          now: () => NOW,
          runHelper: async (_alias, _host, request) =>
            parseAnalyticsRemoteResponse(
              JSON.stringify({
                version: 2,
                truncated: false,
                kinds: Object.fromEntries(
                  request.kinds.map((kind) => [
                    kind,
                    {
                      state: kind === 'claude' ? 'ok' : 'not_installed',
                      fingerprints:
                        kind === 'claude'
                          ? {
                              [file]: {
                                size: 10,
                                mtimeMs: 20,
                                claudeName: name,
                                claudeMirror: true,
                                claudeGroup: name,
                              },
                            }
                          : {},
                      ...(kind === 'claude' && !request.fingerprints.claude?.[file]
                        ? { replacedFiles: [file] }
                        : {}),
                    },
                  ])
                ),
                rows:
                  request.kinds.includes('claude') && !request.fingerprints.claude?.[file]
                    ? [
                        {
                          k: 'claude',
                          f: file,
                          m: 'claude-sonnet-4-6',
                          h: '2026-10-01 15:00',
                          i: 100,
                          o: 0,
                          cr: 0,
                          cw: 0,
                          c: 0,
                          n: 1,
                        },
                      ]
                    : [],
              })
            ),
        });
      // Resolve remote ownership before the local result to reproduce the race.
      let ready: ReturnType<typeof remote>;
      const service = new AccountAnalyticsActivityService({
        now: () => NOW,
        requests: () => [
          { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/original' } },
        ],
        remote: () => {
          ready = remote();
          return ready;
        },
        loadWorker: async () => {
          if (early) await ready;
          if (pruneOnRead) pruned = true;
          if (!pruned) return data('claude-sonnet-4-6', 100, 0);
          return {
            daily: [],
            monthly: [],
            hourly: [],
            session: [],
            eventCount: 0,
            scan: {
              complete: true,
              totalFiles: 0,
              completedFiles: 0,
              failedFiles: 0,
              skippedLines: 0,
              readBytes: 0,
            },
          };
        },
      });
      expect((await settled(service)).totals?.inputTokens).toBe(100);
      // For the early-remote case, change the inventory only after its answer.
      pruneOnRead = true;
      if (!early) pruned = true;
      for (let pass = 0; pass < 2; pass++) {
        await service.get({ ...QUERY, refresh: true }, FROM, NOW);
        expect((await settled(service)).totals?.inputTokens).toBe(100);
      }
    }
  );

  it('retires superseded roots under the worker cap while keeping still-planned skipped shares', async () => {
    let pass = 0;
    const started: string[][] = [[], [], []];
    const service = new AccountAnalyticsActivityService({
      concurrency: 1,
      now: () => NOW,
      remote: async () => ({ results: [], states: [] }),
      requests: () => [
        {
          provider: 'omp',
          request: { kind: 'omp', roots: pass ? ['/fixture/A', '/fixture/B'] : ['/fixture/A'] },
        },
        ...Array.from(
          { length: 25 },
          (_, index): AccountAnalyticsActivityRequest => ({
            provider: 'claude',
            request: { kind: 'claude', projectsDir: `/fixture/${index}` },
          })
        ),
      ],
      loadWorker: async (request) => {
        started[pass].push(request.kind);
        return data(
          request.kind === 'omp' ? 'k3' : 'claude-sonnet-4-6',
          request.kind === 'omp' ? (pass ? 200 : 100) : 1,
          0
        );
      },
    });
    expect((await settled(service)).totals?.inputTokens).toBe(124);
    pass++;
    await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect((await settled(service)).totals?.inputTokens).toBe(225);
    pass++;
    await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect((await settled(service)).totals?.inputTokens).toBe(225);
    expect(started.map((entries) => entries.length)).toEqual([25, 25, 25]);
  });

  it.each([
    'pruned',
    'returned',
    'unchanged',
    'pruned-rejected',
    'returned-rejected',
    'pruned-sibling-rejected',
    'returned-sibling-rejected',
    'pruned-removed-sibling-rejected',
  ] as const)(
    'keeps a coherent saved Claude mirror election across refreshes and restarts (%s)',
    async (scenario) => {
      const siblingFailure = scenario.includes('sibling');
      const total = siblingFailure ? 125 : 100;
      const siblingRows = siblingFailure ? 99 : 0;
      const rejectFirst = scenario.endsWith('-rejected');
      const change = scenario.split('-')[0];
      const scope = path.join(fixtureConfig, 'mirror-snapshot');
      fs.mkdirSync(scope);
      const cacheDir = path.join(scope, 'cache');
      const name = createHash('sha256').update('original.jsonl').digest('hex');
      const file = '11'.repeat(32);
      let original = change !== 'returned';
      const claudeLogNames = () => ({ originals: new Set(original ? [name] : []), complete: true });
      const cached = () => loadAnalyticsRemoteCachedSources(FROM, { cacheDir, claudeLogNames });
      const remote = () =>
        loadAnalyticsRemoteSources(FROM, {
          cacheDir,
          claudeLogNames,
          now: () => NOW,
          hosts: async () => ({ mac: 'fixture-mac', windows: null, nas1: null }),
          runHelper: async (_alias, _host, request) =>
            parseAnalyticsRemoteResponse(
              JSON.stringify({
                version: 2,
                truncated: false,
                kinds: Object.fromEntries(
                  request.kinds.map((kind) => [
                    kind,
                    {
                      state: kind === 'claude' ? 'ok' : 'not_installed',
                      fingerprints:
                        kind === 'claude'
                          ? {
                              [file]: {
                                size: 10,
                                mtimeMs: 20,
                                claudeName: name,
                                claudeMirror: true,
                                claudeGroup: name,
                              },
                            }
                          : {},
                      ...(kind === 'claude' ? { replacedFiles: [file] } : {}),
                    },
                  ])
                ),
                rows: request.kinds.includes('claude')
                  ? [
                      {
                        k: 'claude',
                        f: file,
                        m: 'claude-sonnet-4-6',
                        h: '2026-10-01 15:00',
                        i: 100,
                        o: 0,
                        cr: 0,
                        cw: 0,
                        c: 0,
                        n: 1,
                      },
                    ]
                  : [],
              })
            ),
        });
      const deps = {
        now: () => NOW,
        scope: () => scope,
        persistSnapshot: true,
        requests: (): AccountAnalyticsActivityRequest[] => [
          ...(scenario.includes('removed') && !original
            ? []
            : [
                {
                  provider: 'claude' as const,
                  request: { kind: 'claude' as const, projectsDir: '/fixture/original' },
                },
              ]),
          ...(siblingFailure
            ? [
                {
                  provider: 'claude' as const,
                  request: { kind: 'claude' as const, projectsDir: '/fixture/independent' },
                },
              ]
            : []),
        ],
        loadWorker: async (request?: UsageWorkerRequest): Promise<UsageWorkerResult> =>
          request?.kind === 'claude' && request.projectsDir === '/fixture/independent'
            ? data('claude-sonnet-4-6', 25, 0)
            : original
              ? data('claude-sonnet-4-6', 100, 0)
              : {
                  daily: [],
                  monthly: [],
                  hourly: [],
                  session: [],
                  eventCount: 0,
                  scan: {
                    complete: true,
                    totalFiles: 0,
                    completedFiles: 0,
                    failedFiles: 0,
                    skippedLines: 0,
                    readBytes: 0,
                  },
                },
        remoteCached: cached,
      };
      const first = new AccountAnalyticsActivityService({ ...deps, remote });
      expect((await settled(first)).totals?.inputTokens).toBe(total);
      const savedMirrorRows = original ? 0 : 1;
      if (change !== 'unchanged') original = !original;
      let finishLocal!: () => void;
      const pendingLocal = new Promise<void>((resolve) => {
        finishLocal = resolve;
      });
      let finishRemote!: (answer: Awaited<ReturnType<typeof remote>>) => void;
      const pendingRemote = new Promise<Awaited<ReturnType<typeof remote>>>((resolve) => {
        finishRemote = resolve;
      });
      let rejectLocal = rejectFirst;
      let remoteCalls = 0;
      const restarted = new AccountAnalyticsActivityService({
        ...deps,
        remote: () => (remoteCalls++ === 0 ? pendingRemote : remote()),
        loadWorker: async (request) => {
          await pendingLocal;
          if (
            rejectLocal &&
            (!siblingFailure ||
              (request.kind === 'claude' && request.projectsDir === '/fixture/independent'))
          )
            throw new Error('fixture rejected Claude worker');
          return deps.loadWorker(request);
        },
      });
      // Even a recent snapshot must refresh, but its saved local rows and mirror
      // election stay together until that local pass is ready to publish.
      const instant = await restarted.get(QUERY, FROM, NOW);
      expect(instant.totals?.inputTokens).toBe(total);
      expect(instant.status).toBe('cached');
      expect(instant.refreshing).toBe(true);
      expect(instant.sources?.find((s) => s.tool === 'claude' && s.host === 'mac')?.rowCount).toBe(
        savedMirrorRows
      );
      expect((await restarted.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(total);
      finishLocal();
      await new Promise((resolve) => setTimeout(resolve, 10));
      const localPublished = await restarted.get(QUERY, FROM, NOW);
      expect(localPublished.totals?.inputTokens).toBe(total);
      expect(localPublished.refreshing).toBe(true);
      expect(
        localPublished.sources?.find((s) => s.tool === 'claude' && s.host === 'mac')?.rowCount
      ).toBe(rejectFirst ? savedMirrorRows : original ? 0 : 1);
      expect(
        localPublished.sources?.find((s) => s.tool === 'claude' && s.host === 'ubuntu')?.rowCount
      ).toBe(siblingRows + (rejectFirst ? (savedMirrorRows ? 0 : 99) : original ? 99 : 0));
      const lateRemote = cached();
      if (rejectFirst) lateRemote.results.push({ tool: 'codex', data: data('gpt-5.4', 7, 0) });
      finishRemote(lateRemote);
      const fresh = await settled(restarted);
      expect(fresh.totals?.inputTokens).toBe(total + (rejectFirst ? 7 : 0));
      expect(fresh.refreshing).toBe(false);
      if (rejectFirst) {
        // A late remote answer already elected against the changed inventory must
        // not replace the election paired with the retained local contribution.
        expect(fresh.sources?.find((s) => s.tool === 'claude' && s.host === 'mac')?.rowCount).toBe(
          savedMirrorRows
        );
        // A second failed pass and its remote answer must keep the same pair too.
        await restarted.get({ ...QUERY, refresh: true }, FROM, NOW);
        expect((await settled(restarted)).totals?.inputTokens).toBe(total);
        let rejectAfterRestart = true;
        const failedRestart = new AccountAnalyticsActivityService({
          ...deps,
          remote,
          loadWorker: async (request) => {
            if (
              rejectAfterRestart &&
              (!siblingFailure ||
                (request.kind === 'claude' && request.projectsDir === '/fixture/independent'))
            )
              throw new Error('fixture still rejected after another restart');
            return deps.loadWorker(request);
          },
        });
        expect((await settled(failedRestart)).totals?.inputTokens).toBe(total);
        await failedRestart.get({ ...QUERY, refresh: true }, FROM, NOW);
        expect((await settled(failedRestart)).totals?.inputTokens).toBe(total);
        rejectLocal = false;
        rejectAfterRestart = false;
        await failedRestart.get({ ...QUERY, refresh: true }, FROM, NOW);
        const recovered = await settled(failedRestart);
        expect(recovered.totals?.inputTokens).toBe(total);
        expect(
          recovered.sources?.find((s) => s.tool === 'claude' && s.host === 'mac')?.rowCount
        ).toBe(original ? 0 : 1);
        expect(
          recovered.sources?.find((s) => s.tool === 'claude' && s.host === 'ubuntu')?.rowCount
        ).toBe(siblingRows + (original ? 99 : 0));
      }

      // A serialized answer has no callback. The raw cache has an equal timestamp,
      // and must still replace it when an offline refresh changes mirror ownership.
      const serialized = JSON.parse(JSON.stringify(cached())) as Awaited<ReturnType<typeof remote>>;
      let offline = false;
      const fallback = new AccountAnalyticsActivityService({
        ...deps,
        persistSnapshot: false,
        remote: async () => {
          if (offline) throw new Error('fixture offline');
          return serialized;
        },
      });
      expect((await settled(fallback)).totals?.inputTokens).toBe(total);
      original = !original;
      offline = true;
      await fallback.get({ ...QUERY, refresh: true }, FROM, NOW);
      expect((await settled(fallback)).totals?.inputTokens).toBe(total);
    }
  );

  it.each([
    'success',
    'failure',
    'partial-empty',
    'failed-empty',
    'partial-positive',
    'complete-empty',
    'legacy',
  ] as const)(
    'keeps a restored predecessor until its capped replacement publishes (%s)',
    async (outcome) => {
      const scope = path.join(fixtureConfig, 'replacement-snapshot');
      fs.mkdirSync(scope);
      let restarted = false;
      let allowReplacement = false;
      const started: string[] = [];
      const omp = (): AccountAnalyticsActivityRequest => ({
        provider: 'omp',
        request: { kind: 'omp', roots: restarted ? ['/fixture/A', '/fixture/B'] : ['/fixture/A'] },
      });
      const claude = Array.from(
        { length: 25 },
        (_, index): AccountAnalyticsActivityRequest => ({
          provider: 'claude',
          request: { kind: 'claude', projectsDir: `/fixture/${index}` },
        })
      );
      const deps = {
        concurrency: 1,
        now: () => NOW + (restarted ? 61_000 : 0),
        scope: () => scope,
        persistSnapshot: true,
        remote: async () => ({ results: [], states: [] }),
        requests: () => (restarted ? [...claude, omp()] : [omp(), ...claude]),
        loadWorker: async (request: UsageWorkerRequest) => {
          started.push(request.kind);
          if (request.kind !== 'omp') return data('claude-sonnet-4-6', 1, 0);
          if (restarted && !allowReplacement) throw new Error('replacement should be capped');
          if (restarted && outcome === 'failure') throw new Error('fixture failure');
          if (
            restarted &&
            (outcome === 'partial-empty' ||
              outcome === 'failed-empty' ||
              outcome === 'complete-empty')
          )
            return {
              daily: [],
              monthly: [],
              hourly: [],
              session: [],
              eventCount: 0,
              scan: {
                complete: outcome === 'complete-empty',
                totalFiles: 0,
                completedFiles: 0,
                failedFiles: outcome === 'failed-empty' ? 1 : 0,
                skippedLines: 0,
                readBytes: 0,
              },
            };
          return {
            ...data('k3', restarted ? 200 : 100, 0),
            ...(restarted && outcome === 'partial-positive'
              ? {
                  scan: {
                    complete: false,
                    totalFiles: 2,
                    completedFiles: 1,
                    failedFiles: 0,
                    skippedLines: 0,
                    readBytes: 10,
                  },
                }
              : {}),
          };
        },
      };
      expect((await settled(new AccountAnalyticsActivityService(deps))).totals?.inputTokens).toBe(
        124
      );
      if (outcome === 'legacy') {
        const file = path.join(
          scope,
          'cache',
          'account-activity-v1',
          'analytics-activity-snapshot-v1.json'
        );
        const snapshot = JSON.parse(fs.readFileSync(file, 'utf8'));
        delete snapshot.identities;
        fs.writeFileSync(file, JSON.stringify(snapshot));
      }
      restarted = true;
      started.length = 0;
      const service = new AccountAnalyticsActivityService(deps);
      expect((await settled(service)).totals?.inputTokens).toBe(125);
      expect(started).toHaveLength(25);
      expect(started).not.toContain('omp');
      allowReplacement = true;
      await service.get({ ...QUERY, refresh: true }, FROM, NOW);
      expect((await settled(service)).totals?.inputTokens).toBe(
        ['success', 'partial-positive', 'legacy'].includes(outcome)
          ? 225
          : outcome === 'complete-empty'
            ? 25
            : 125
      );
      const serialized = fs.readFileSync(
        path.join(scope, 'cache', 'account-activity-v1', 'analytics-activity-snapshot-v1.json'),
        'utf8'
      );
      expect(serialized).not.toContain('/fixture/');
    }
  );

  it.each(['legacy', 'mixed', 'current', 'legacy-restart'] as const)(
    'keeps 524 tokens until the capped replacement can publish (%s snapshot)',
    async (format) => {
      const scope = path.join(fixtureConfig, 'legacy-replacements');
      fs.mkdirSync(scope);
      let expanded = false;
      const started: string[] = [];
      const omp = (name: string): AccountAnalyticsActivityRequest => ({
        provider: 'omp',
        request: {
          kind: 'omp',
          roots: [`/fixture/${name}`, ...(expanded ? [`/fixture/${name}-extra`] : [])],
        },
      });
      const siblings: AccountAnalyticsActivityRequest[] = Array.from({ length: 24 }, (_, i) => ({
        provider: 'claude',
        request: { kind: 'claude', projectsDir: `/fixture/claude-${i}` },
      }));
      const deps = {
        concurrency: 1,
        now: () => NOW + (expanded ? 1000 : 0),
        scope: () => scope,
        persistSnapshot: true,
        remote: async () => ({ results: [], states: [] }),
        requests: () =>
          expanded ? [omp('a'), ...siblings, omp('b')] : [omp('a'), omp('b'), ...siblings],
        loadWorker: async (request: UsageWorkerRequest) => {
          if (request.kind !== 'omp') return data('claude-sonnet-4-6', 1, 0);
          started.push(request.roots[0]);
          return data('k3', request.roots[0].endsWith('/a') ? 400 : expanded ? 200 : 100, 0);
        },
      };
      const previous = new AccountAnalyticsActivityService(deps);
      await settled(previous);
      await previous.get({ ...QUERY, refresh: true }, FROM, NOW);
      expect((await settled(previous)).totals?.inputTokens).toBe(524);
      const file = path.join(
        scope,
        'cache',
        'account-activity-v1',
        'analytics-activity-snapshot-v1.json'
      );
      const snapshot = JSON.parse(fs.readFileSync(file, 'utf8'));
      if (format === 'mixed') {
        const legacyKey = snapshot.local.find(
          ([, source]: [string, { provider: string; data: UsageWorkerResult[] }]) =>
            source.provider === 'omp' && source.data[0].hourly[0].inputTokens === 100
        )[0];
        snapshot.identities = snapshot.identities.filter(([key]: [string]) => key !== legacyKey);
      } else if (format !== 'current') delete snapshot.identities;
      fs.writeFileSync(file, JSON.stringify(snapshot));

      expanded = true;
      started.length = 0;
      let service = new AccountAnalyticsActivityService(deps);
      const capped = await settled(service);
      expect(started).toEqual(['/fixture/a']);
      expect(capped.totals?.inputTokens).toBe(524);
      expect(capped.sources.find((s) => s.tool === 'omp' && s.host === 'ubuntu')?.lastScanAt).toBe(
        new Date(format === 'current' ? NOW + 1000 : NOW).toISOString()
      );
      const saved = JSON.parse(fs.readFileSync(file, 'utf8'));
      expect(
        saved.local.filter(
          ([, source]: [string, { provider: string }]) => source.provider === 'omp'
        )
      ).toHaveLength(2);
      // A restart discards unpublished coverage and keeps the whole legacy unit.
      if (format === 'legacy-restart') {
        started.length = 0;
        service = new AccountAnalyticsActivityService(deps);
        expect((await settled(service)).totals?.inputTokens).toBe(524);
        expect(started).toEqual(['/fixture/a']);
      }
      await service.get({ ...QUERY, refresh: true }, FROM, NOW);
      expect((await settled(service)).totals?.inputTokens).toBe(624);
      expect(started).toContain('/fixture/b');
      const complete = JSON.parse(fs.readFileSync(file, 'utf8'));
      expect(
        complete.local.filter(
          ([, source]: [string, { provider: string }]) => source.provider === 'omp'
        )
      ).toHaveLength(2);
      expect(complete.identities).toHaveLength(complete.local.length);
      expect(JSON.stringify(complete)).not.toContain('/fixture/');
    }
  );

  it.each(['success', 'failed-pending', 'partial-empty', 'complete-empty'] as const)(
    'swaps a legacy provider as one unit across capped passes (%s)',
    async (outcome) => {
      const scope = path.join(fixtureConfig, 'legacy-unit');
      fs.mkdirSync(scope);
      let pass = 0;
      const started: string[][] = [];
      const deps = {
        concurrency: 1,
        now: () => NOW + pass * 1000,
        scope: () => scope,
        persistSnapshot: true,
        remote: async () => ({ results: [], states: [] }),
        requests: (): AccountAnalyticsActivityRequest[] =>
          Array.from({ length: 27 }, (_, i) => ({
            provider: 'omp',
            request: {
              kind: 'omp',
              roots: [`/fixture/${i}`, ...(pass ? [`/fixture/${i}-extra`] : [])],
            },
          })),
        loadWorker: async (request: UsageWorkerRequest) => {
          if (request.kind !== 'omp') throw new Error('unexpected fixture request');
          (started[pass] ??= []).push(request.roots[0]);
          if (outcome === 'failed-pending' && pass === 2 && request.roots[0] === '/fixture/0')
            throw new Error('fixture failure supersedes pending read');
          if (
            pass &&
            (outcome === 'complete-empty' ||
              (outcome === 'partial-empty' && pass === 2 && request.roots[0] === '/fixture/0'))
          )
            return {
              daily: [],
              monthly: [],
              hourly: [],
              session: [],
              eventCount: 0,
              scan: {
                complete: outcome === 'complete-empty',
                totalFiles: 0,
                completedFiles: 0,
                failedFiles: 0,
                skippedLines: 0,
                readBytes: 0,
              },
            };
          return data('k3', pass ? 2 : 1, 0);
        },
      };
      const previous = new AccountAnalyticsActivityService(deps);
      await settled(previous);
      await previous.get({ ...QUERY, refresh: true }, FROM, NOW);
      expect((await settled(previous)).totals?.inputTokens).toBe(27);
      const file = path.join(
        scope,
        'cache',
        'account-activity-v1',
        'analytics-activity-snapshot-v1.json'
      );
      const snapshot = JSON.parse(fs.readFileSync(file, 'utf8'));
      delete snapshot.identities;
      fs.writeFileSync(file, JSON.stringify(snapshot));
      const legacyKeys = new Set(snapshot.local.map(([key]: [string]) => key));
      pass = 1;
      const service = new AccountAnalyticsActivityService(deps);
      expect((await settled(service)).totals?.inputTokens).toBe(27);
      expect(started[1]).toHaveLength(25);
      const retained = JSON.parse(fs.readFileSync(file, 'utf8'));
      expect(retained.local.map(([key]: [string]) => key)).toEqual([...legacyKeys]);
      pass++;
      await service.get({ ...QUERY, refresh: true }, FROM, NOW);
      const second = await settled(service);
      expect(started[2]).toHaveLength(25);
      expect(started[2].slice(0, 2)).toEqual(['/fixture/25', '/fixture/26']);
      expect(second.totals?.inputTokens ?? 0).toBe(
        outcome === 'success' ? 54 : outcome === 'complete-empty' ? 0 : 27
      );
      if (outcome === 'failed-pending' || outcome === 'partial-empty') {
        expect(
          second.sources.find((s) => s.tool === 'omp' && s.host === 'ubuntu')?.lastScanAt
        ).toBe(new Date(NOW).toISOString());
        pass++;
        await service.get({ ...QUERY, refresh: true }, FROM, NOW);
        expect((await settled(service)).totals?.inputTokens).toBe(54);
      }
      const complete = JSON.parse(fs.readFileSync(file, 'utf8'));
      expect(complete.local).toHaveLength(27);
      expect(complete.local.some(([key]: [string]) => legacyKeys.has(key))).toBe(false);
      expect(complete.identities).toHaveLength(27);
    }
  );

  it('restores per-request local shares after a restart without doubling remote rows', async () => {
    const scope = path.join(fixtureConfig, 'persisted');
    fs.mkdirSync(scope);
    let restarted = false;
    const deps = {
      now: () => NOW + (restarted ? 61_000 : 0),
      scope: () => scope,
      persistSnapshot: true,
      requests: (): AccountAnalyticsActivityRequest[] =>
        ['a', 'b'].map((key) => ({
          provider: 'claude',
          request: { kind: 'claude', projectsDir: `/fixture/${key}` },
        })),
      remote: async () => ({
        results: [{ tool: 'claude' as const, data: data('claude-sonnet-4-6', 777, 0) }],
        states: [],
      }),
      loadWorker: async (request: UsageWorkerRequest) => {
        if (restarted && request.kind === 'claude' && request.projectsDir.endsWith('/b'))
          throw new Error('fixture');
        return data(
          'claude-sonnet-4-6',
          request.kind === 'claude' && request.projectsDir.endsWith('/a') ? 50_000 : 30_000,
          0
        );
      },
    };
    expect((await settled(new AccountAnalyticsActivityService(deps))).totals?.inputTokens).toBe(
      80_777
    );
    restarted = true;
    const restored = await settled(new AccountAnalyticsActivityService(deps));
    expect(restored.totals?.inputTokens).toBe(80_777);
    const serialized = fs.readFileSync(
      path.join(scope, 'cache', 'account-activity-v1', 'analytics-activity-snapshot-v1.json'),
      'utf8'
    );
    expect(serialized).not.toContain('/fixture/');
    expect(serialized).not.toContain('/private/project');
  });

  it('dates only productive partial reads and keeps failed reads at their previous time', async () => {
    for (const mode of ['empty', 'failure', 'fresh', 'replayed', 'cached'] as const) {
      let pass = 0;
      const service = new AccountAnalyticsActivityService({
        now: () => NOW + pass * 1000,
        requests: () => [{ provider: 'zcode', request: { kind: 'zcode', dbPath: '/fixture/db' } }],
        remote: async () => ({ results: [], states: [] }),
        loadWorker: async () => ({
          ...(pass && mode === 'empty'
            ? { daily: [], monthly: [], hourly: [], session: [], eventCount: 0 }
            : data('glm-5', 100, 0)),
          ...(pass && mode === 'replayed' ? { readAt: new Date(NOW - 1000).toISOString() } : {}),
          scan: {
            complete: pass === 0,
            completedFiles: mode === 'empty' || mode === 'cached' ? 0 : 1,
            totalFiles: 2,
            failedFiles: pass && mode === 'failure' ? 1 : 0,
            skippedLines: 0,
            readBytes: 0,
          },
        }),
      });
      await settled(service);
      pass++;
      await service.get({ ...QUERY, refresh: true }, FROM, NOW);
      const answer = await settled(service);
      const cell = answer.sources.find(
        (entry) => entry.tool === 'zcode' && entry.host === 'ubuntu'
      );
      expect(cell?.lastScanAt).toBe(
        new Date(
          mode === 'fresh' ? NOW + 1000 : mode === 'replayed' ? NOW - 1000 : NOW
        ).toISOString()
      );
      if (mode === 'failure') expect(cell?.detail).toContain('local scan failed');
    }
    const cold = new AccountAnalyticsActivityService({
      now: () => NOW,
      requests: () => [{ provider: 'muse', request: { kind: 'muse', sessionsDir: '/fixture' } }],
      remote: async () => ({ results: [], states: [] }),
      loadWorker: async () => ({
        daily: [],
        monthly: [],
        hourly: [],
        session: [],
        eventCount: 0,
        scan: {
          complete: false,
          completedFiles: 0,
          totalFiles: 1,
          failedFiles: 0,
          skippedLines: 0,
          readBytes: 0,
        },
      }),
    });
    expect(
      (await settled(cold)).sources.find(
        (entry) => entry.tool === 'muse' && entry.host === 'ubuntu'
      )?.lastScanAt
    ).toBeNull();
  });

  it('adopts remote answers that finish before or after an exhausted local deadline', async () => {
    for (const late of [false, true]) {
      let wall = Date.now();
      const clock = spyOn(Date, 'now').mockImplementation(() => wall);
      let release: (answer: {
        results: Array<{ tool: 'claude'; data: UsageWorkerResult }>;
        states: [];
      }) => void = () => {};
      try {
        const remoteAnswer = {
          results: [{ tool: 'claude' as const, data: data('claude-sonnet-4-6', 777, 0) }],
          states: [] as [],
        };
        const service = new AccountAnalyticsActivityService({
          now: () => NOW,
          requests: () => [
            { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture' } },
          ],
          loadWorker: async () => {
            wall += 60_001;
            return data('claude-sonnet-4-6', 100, 0);
          },
          remote: () =>
            late
              ? new Promise((resolve) => {
                  release = resolve;
                })
              : Promise.resolve(remoteAnswer),
        });
        await settled(service);
        if (late) {
          release(remoteAnswer);
          await new Promise((resolve) => setTimeout(resolve, 0));
        }
        expect((await settled(service)).totals?.inputTokens).toBe(877);
      } finally {
        clock.mockRestore();
      }
    }
  });

  it('uses a newer disk remote cache even with an older answer in memory', async () => {
    let pass = 0;
    let wall = Date.now();
    const clock = spyOn(Date, 'now').mockImplementation(() => wall);
    const answer = (input: number, at: number) => ({
      results: [{ tool: 'claude' as const, data: data('claude-sonnet-4-6', input, 0) }],
      states: [
        {
          tool: 'claude' as const,
          host: 'mac' as const,
          state: 'ok' as const,
          rowCount: 99,
          lastScanAt: new Date(at).toISOString(),
          detail: null,
        },
      ],
    });
    try {
      const service = new AccountAnalyticsActivityService({
        now: () => NOW,
        requests: () => [
          { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture' } },
        ],
        loadWorker: async () => {
          if (pass) wall += 60_001;
          return data('claude-sonnet-4-6', 100, 0);
        },
        remote: () => (pass ? new Promise(() => {}) : Promise.resolve(answer(10, NOW))),
        remoteCached: () => answer(777, NOW + 1000),
      });
      expect((await settled(service)).totals?.inputTokens).toBe(110);
      pass++;
      await service.get({ ...QUERY, refresh: true }, FROM, NOW);
      expect((await settled(service)).totals?.inputTokens).toBe(877);
    } finally {
      clock.mockRestore();
    }
  });

  it('reports failures without promising remaining files when all files were read', async () => {
    const service = new AccountAnalyticsActivityService({
      now: () => NOW,
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture' } },
      ],
      remote: async () => ({ results: [], states: [] }),
      loadWorker: async () => ({
        ...data('claude-sonnet-4-6', 100, 0),
        scan: {
          complete: false,
          completedFiles: 1008,
          totalFiles: 1008,
          failedFiles: 2,
          skippedLines: 0,
          readBytes: 0,
        },
      }),
    });
    const result = await settled(service);
    expect(result.message).toContain('2 local scan failures');
    expect(result.message).not.toContain('remaining files resume');
  });

  it.each(['cold', 'upgrade', 'legacy', 'failed-pending'] as const)(
    'converges capped Claude replacements with a real remote election (%s)',
    async (scenario) => {
      const scope = path.join(fixtureConfig, 'capped-claude');
      fs.mkdirSync(scope);
      let pass = 0;
      let upgrade = scenario !== 'cold';
      let input = 2;
      const started: string[][] = [];
      const cacheDir = path.join(scope, 'cache');
      // A real remote row with output usage, source metadata, timestamps, and
      // the production mirror reconciler. Its input is zero so all 54 input
      // tokens in the completed answer must come from the 27 local requests.
      const remote = () =>
        loadAnalyticsRemoteSources(FROM, {
          cacheDir,
          now: () => NOW,
          claudeLogNames: () => ({ originals: new Set(), complete: true }),
          hosts: async () => ({ mac: 'fixture-mac', windows: null, nas1: null }),
          runHelper: async (_alias, _host, request) =>
            parseAnalyticsRemoteResponse(
              JSON.stringify({
                version: 2,
                truncated: false,
                kinds: Object.fromEntries(
                  request.kinds.map((kind) => [
                    kind,
                    {
                      state: kind === 'claude' ? 'ok' : 'not_installed',
                      fingerprints:
                        kind === 'claude'
                          ? {
                              ['11'.repeat(32)]: {
                                size: 10,
                                mtimeMs: 20,
                                claudeGroup: '22'.repeat(32),
                              },
                            }
                          : {},
                      ...(kind === 'claude' ? { replacedFiles: ['11'.repeat(32)] } : {}),
                    },
                  ])
                ),
                rows: request.kinds.includes('claude')
                  ? [
                      {
                        k: 'claude',
                        f: '11'.repeat(32),
                        m: 'claude-sonnet-4-6',
                        h: '2026-10-01 15:00',
                        i: 0,
                        o: 1,
                        cr: 0,
                        cw: 0,
                        c: 0,
                        n: 1,
                      },
                    ]
                  : [],
              })
            ),
        });
      const deps = {
        concurrency: 1,
        now: () => NOW + pass * 1000,
        scope: () => scope,
        persistSnapshot: true,
        remote,
        remoteCached: () =>
          loadAnalyticsRemoteCachedSources(FROM, {
            cacheDir,
            claudeLogNames: () => ({ originals: new Set(), complete: true }),
          }),
        requests: (): AccountAnalyticsActivityRequest[] =>
          Array.from({ length: upgrade ? 1 : 27 }, (_, index) => ({
            provider: 'claude',
            request: {
              kind: 'claude',
              projectsDir:
                upgrade && scenario === 'legacy' ? '/fixture/previous' : `/fixture/${index}`,
            },
          })),
        loadWorker: async (request: UsageWorkerRequest) => {
          if (request.kind !== 'claude') throw new Error('unexpected fixture request');
          (started[pass] ??= []).push(request.projectsDir);
          if (scenario === 'failed-pending' && pass === 2 && request.projectsDir === '/fixture/0')
            throw new Error('fixture failure after a pending replacement');
          return data('claude-sonnet-4-6', upgrade ? 27 : input, 0);
        },
      };
      if (upgrade) {
        const previous = new AccountAnalyticsActivityService(deps);
        expect((await settled(previous)).totals?.inputTokens).toBe(27);
        if (scenario === 'legacy') {
          const file = path.join(
            scope,
            'cache',
            'account-activity-v1',
            'analytics-activity-snapshot-v1.json'
          );
          const snapshot = JSON.parse(fs.readFileSync(file, 'utf8'));
          delete snapshot.identities;
          fs.writeFileSync(file, JSON.stringify(snapshot));
        }
        upgrade = false;
        pass++;
      }
      const service = new AccountAnalyticsActivityService(deps);
      const first = await settled(service);
      expect(first.totals?.inputTokens).toBe(scenario === 'cold' ? 50 : 27);
      expect(started[pass]).toHaveLength(25);
      // All healthy requests have replacements within two passes, even though
      // no pass can run all of them. The saved snapshot remains paired until then.
      pass++;
      await service.get({ ...QUERY, refresh: true }, FROM, NOW);
      const second = await settled(service);
      expect(started[pass]).toHaveLength(25);
      expect(started[pass].slice(0, 2)).toEqual(['/fixture/25', '/fixture/26']);
      expect(second.totals?.inputTokens).toBe(scenario === 'failed-pending' ? 27 : 54);
      if (scenario === 'failed-pending') {
        expect(started[1]).toContain('/fixture/0');
        expect(started[2]).toContain('/fixture/0');
        expect(
          second.sources.find((s) => s.tool === 'claude' && s.host === 'ubuntu')?.lastScanAt
        ).toBe(new Date(NOW).toISOString());
        pass++;
        await service.get({ ...QUERY, refresh: true }, FROM, NOW);
        expect((await settled(service)).totals?.inputTokens).toBe(54);
        expect(started[pass]).toContain('/fixture/0');
      }
      const answer = await settled(service);
      expect(answer.sources.find((s) => s.tool === 'claude' && s.host === 'mac')).toMatchObject({
        state: 'ok',
        lastScanAt: new Date(NOW).toISOString(),
        rowCount: 1,
      });
      expect(answer.totals?.inputTokens).toBe(54);
      // A completed batch cannot supply stale coverage for the next batch.
      input = 3;
      pass++;
      await service.get({ ...QUERY, refresh: true }, FROM, NOW);
      expect((await settled(service)).totals?.inputTokens).toBe(54);
      pass++;
      await service.get({ ...QUERY, refresh: true }, FROM, NOW);
      expect((await settled(service)).totals?.inputTokens).toBe(81);
    }
  );

  it('rotates capped requests ahead of previously attempted roots and preserves a safe worker budget', async () => {
    let wall = Date.now();
    const clock = spyOn(Date, 'now').mockImplementation(() => wall);
    let pass = 0;
    const started: string[][] = [[], []];
    const budgets: number[] = [];
    try {
      const service = new AccountAnalyticsActivityService({
        now: () => NOW,
        concurrency: 1,
        requests: () =>
          Array.from({ length: 27 }, (_, index) => ({
            provider: 'claude',
            request: { kind: 'claude', projectsDir: `/fixture/${index}` },
          })),
        remote: async () => ({ results: [], states: [] }),
        loadWorker: async (request, budget) => {
          started[pass].push(request.kind === 'claude' ? request.projectsDir : '');
          budgets.push(budget ?? 0);
          if (started[pass].length === 1) wall += 48_500;
          return data('claude-sonnet-4-6', 1, 0);
        },
      });
      await settled(service);
      expect(started[0]).toHaveLength(25);
      pass++;
      await service.get({ ...QUERY, refresh: true }, FROM, NOW);
      const result = await settled(service);
      expect(started[1].slice(0, 2)).toEqual(['/fixture/25', '/fixture/26']);
      expect(budgets.every((budget) => budget >= 15_000)).toBe(true);
      expect(result.totals?.inputTokens).toBe(27);
    } finally {
      clock.mockRestore();
    }
  });

  it('publishes real bounded progress, then warm completion, while keeping shared logs unattributed', async () => {
    let now = NOW;
    let complete = false;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      now: () => now,
      scope: () => 'progress',
      responseBudgetMs: 100,
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => ({
        ...data('gpt-5.4', 100, 0.01),
        scan: {
          complete,
          completedFiles: complete ? 2 : 1,
          totalFiles: 2,
          skippedLines: 0,
          failedFiles: 0,
          readBytes: 100,
          unfinishedFiles: 0,
        },
      }),
    });
    const partial = await service.get(QUERY, FROM, NOW);
    expect(partial.status).toBe('cached');
    // A partial scan still read this pass: it is dated now, as a remote host's partial scan is,
    // so a tool that never finishes cannot read "Cached, 3h ago" while its records keep arriving.
    expect(partial.sources?.find((s) => s.tool === 'codex' && s.host === 'ubuntu')).toMatchObject({
      state: 'cached',
      lastScanAt: new Date(NOW).toISOString(),
      detail: expect.stringContaining('partial local scan'),
    });
    expect(partial.totals?.inputTokens).toBe(100);
    expect(partial.message).toContain('1 of 2 local log files');
    expect(partial.accountAttribution).toBe('unavailable');
    complete = true;
    now += 60000;
    const warm = await settled(service);
    expect(warm.status).toBe('ok');
    expect(warm.sources?.find((s) => s.tool === 'codex' && s.host === 'ubuntu')).toMatchObject({
      state: 'ok',
      lastScanAt: new Date(now).toISOString(),
    });
    expect(warm.totals).toEqual(partial.totals);
    expect(warm.message).not.toContain('remaining files');
  });
  it('labels old unfinished tails separately from remaining history and never invents cold partial zeros', async () => {
    let events = false;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      now: () => NOW,
      scope: () => 'unfinished',
      responseBudgetMs: 100,
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () =>
        events
          ? {
              ...data('gpt-5.4', 100, 0.01),
              scan: {
                complete: false,
                completedFiles: 1,
                totalFiles: 2,
                skippedLines: 0,
                failedFiles: 0,
                readBytes: 100,
                unfinishedFiles: 1,
              },
            }
          : {
              daily: [],
              hourly: [],
              monthly: [],
              session: [],
              eventCount: 0,
              scan: {
                complete: false,
                completedFiles: 0,
                totalFiles: 2,
                skippedLines: 0,
                failedFiles: 0,
                readBytes: 100,
              },
            },
    });
    const empty = await service.get(QUERY, FROM, NOW);
    expect(empty.status).toBe('unavailable');
    expect(empty.sources?.find((s) => s.tool === 'codex' && s.host === 'ubuntu')).toMatchObject({
      state: 'scanning',
      detail: expect.stringContaining('partial local scan'),
    });
    expect(empty.totals).toBeNull();
    expect(empty.providers).toEqual([]);
    events = true;
    const tail = await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect(tail.status).toBe('cached');
    expect(tail.totals?.inputTokens).toBe(100);
    expect(tail.message).toContain('1 local log files end with unfinished records');
    expect(tail.message).not.toContain('remaining files resume');
  });
  it('keeps a tool skipped by the deadline scanning while real failures stay unavailable', async () => {
    let wall = Date.now();
    const clock = spyOn(Date, 'now').mockImplementation(() => wall);
    const loaded: string[] = [];
    try {
      const service = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        now: () => NOW,
        scope: () => 'deadline-fixture',
        responseBudgetMs: 100,
        concurrency: 1,
        requests: () => [
          { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture' } },
          {
            provider: 'codex',
            request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
          },
        ],
        loadWorker: async (request) => {
          loaded.push(request.kind);
          wall += 60_001;
          throw new Error('fixture read failed');
        },
      });
      const result = await service.get(QUERY, FROM, NOW);
      expect(loaded).toEqual(['claude']);
      expect(result.status).toBe('unavailable');
      expect(result.sources?.find((s) => s.tool === 'codex' && s.host === 'ubuntu')).toMatchObject({
        state: 'scanning',
        detail: expect.stringContaining('ran out of time before it reached this tool'),
      });
      expect(result.sources?.find((s) => s.tool === 'claude' && s.host === 'ubuntu')).toMatchObject(
        { state: 'unavailable', detail: 'local scan failed' }
      );
    } finally {
      clock.mockRestore();
    }
  });
  it('reads a real bounded native worker and excludes proxy-backed Codex rollout records', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-analytics-worker-'));
    try {
      const sessions = path.join(root, 'codex', 'sessions');
      fs.mkdirSync(sessions, { recursive: true });
      for (const provider of ['openai', 'cliproxy']) {
        const input = provider === 'openai' ? 100 : 100_000;
        const lines = [
          {
            timestamp: '2026-10-01T15:00:00Z',
            type: 'session_meta',
            payload: { id: provider, model_provider: provider, cwd: '/private-project' },
          },
          {
            timestamp: '2026-10-01T15:00:01Z',
            type: 'turn_context',
            payload: { model: 'gpt-5.4' },
          },
          {
            timestamp: '2026-10-01T15:05:00Z',
            type: 'event_msg',
            payload: {
              type: 'token_count',
              info: {
                total_token_usage: {
                  input_tokens: input,
                  output_tokens: 5,
                  cached_input_tokens: 20,
                  reasoning_output_tokens: 2,
                },
              },
            },
          },
        ];
        fs.writeFileSync(
          path.join(sessions, `rollout-${provider}.jsonl`),
          lines.map((line) => JSON.stringify(line)).join('\n')
        );
      }
      const result = await runWithScopedCcsHome(root, () =>
        // A nearly exhausted outer budget must not kill the collector before its own deadline.
        loadAccountAnalyticsWorker(
          {
            kind: 'codex',
            codexHome: path.join(root, 'codex'),
            cacheDir: path.join(root, 'cache'),
          },
          1
        )
      );
      expect(result.eventCount).toBe(1);
      expect(result.hourly[0].inputTokens).toBe(80);
      expect(result.hourly[0].outputTokens).toBe(5);
      expect(result.hourly[0].cacheReadTokens).toBe(20);
      expect(result.hourly[0].source).toBe('codex-native');
      const projected = projectAccountAnalyticsActivity(
        [{ provider: 'codex', data: [result], fetchedAt: new Date(NOW).toISOString() }],
        QUERY,
        FROM,
        NOW,
        'ok',
        'Native fixture'
      );
      expect(projected.totals?.inputTokens).toBe(80);
      expect(
        (projected.totals?.inputTokens ?? 0) +
          (projected.totals?.outputTokens ?? 0) +
          (projected.totals?.cacheReadTokens ?? 0)
      ).toBe(105);
      expect(JSON.stringify(projected)).not.toContain('/private-project');
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
  it('retains measured native tool dimensions, token/model breakdowns, UTC buckets and estimate labels', () => {
    const result = projectAccountAnalyticsActivity(
      sources(),
      QUERY,
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(result.totals).toMatchObject({
      inputTokens: 300,
      outputTokens: 10,
      cacheCreationTokens: 4,
      cacheReadTokens: 6,
      estimatedCostUsd: 0.015,
    });
    // Claude has a built-in rate. Without a provider, `gpt-5.4` is priced only
    // by the unknown-model fallback, which never yields a cost split.
    expect(result.providers.map((row) => [row.provider, row.totals.costByType !== null])).toEqual([
      ['claude', true],
      ['codex', false],
    ]);
    expect(result.totals?.costByType).toBeNull();
    expect(result.models.find((row) => row.model === 'gpt-5.4')?.rates?.source).toBe('fallback');
    expect(
      result.providers.map((row) => [
        row.provider,
        row.totals.inputTokens,
        row.usageEvents,
        row.sessionCount,
      ])
    ).toEqual([
      ['claude', 100, 4, 1],
      ['codex', 200, 4, 1],
    ]);
    expect(result.byDay.map((row) => [row.date, row.provider, row.inputTokens])).toEqual([
      ['2026-10-01', 'claude', 100],
      ['2026-10-01', 'codex', 200],
    ]);
    expect(result.byHour[0].hour).toBe('2026-10-01T15:00:00Z');
    expect(result.models.map((row) => row.model)).toEqual(['gpt-5.4', 'claude-sonnet-4-6']);
    expect(result.costBasis).toBe('estimated-api-equivalent');
    expect(result.timezone).toBe('UTC');
    expect(result.accountAttribution).toBe('unavailable');
    expect(result.scope).toBe('multi-host-cli');
    expect(JSON.stringify(result)).not.toContain('/private/project');
    expect(JSON.stringify(result)).not.toContain('shared-id');
  });

  it('filters provider before adding totals and never assigns shared local logs to an active account', () => {
    const provider = projectAccountAnalyticsActivity(
      sources(),
      { ...QUERY, provider: 'codex' },
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(provider.totals?.inputTokens).toBe(200);
    expect(provider.providers).toHaveLength(1);
    const account = projectAccountAnalyticsActivity(
      sources(),
      { ...QUERY, account: 'codex:active' },
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(account.status).toBe('unavailable');
    expect(account.totals).toBeNull();
    expect(account.byDay).toEqual([]);
    expect(account.message).toContain('do not reliably identify');
    const qwen = projectAccountAnalyticsActivity(
      sources(),
      { ...QUERY, provider: 'qwen' },
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(qwen.status).toBe('unavailable');
    expect(qwen.totals).toBeNull();
    expect(qwen.message).toContain('quota and balance');
  });

  it('excludes old/future or invalid hourly keys instead of inventing filled daily records', () => {
    const fixture = sources();
    fixture[0].data[0].hourly.push({
      ...fixture[0].data[0].hourly[0],
      hour: '2026-10-03 00:00',
      inputTokens: 1e6,
    });
    fixture[0].data[0].hourly.push({
      ...fixture[0].data[0].hourly[0],
      hour: '2026-09-01 00:00',
      inputTokens: 1e6,
    });
    fixture[0].data[0].hourly.push({
      ...fixture[0].data[0].hourly[0],
      hour: 'invalid',
      inputTokens: 1e6,
    });
    const result = projectAccountAnalyticsActivity(
      fixture,
      QUERY,
      FROM,
      NOW,
      'ok',
      'Native fixture'
    );
    expect(result.totals?.inputTokens).toBe(300);
    expect(result.byDay).toHaveLength(2);
    expect(result.byHour).toHaveLength(2);
  });

  it('coalesces background reads and returns loading within the response budget', async () => {
    let resolve!: (result: UsageWorkerResult) => void;
    const pending = new Promise<UsageWorkerResult>((complete) => {
      resolve = complete;
    });
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        return pending;
      },
      now: () => NOW,
      scope: () => '/fixture',
      responseBudgetMs: 5,
    });
    const [first, second] = await Promise.all([
      service.get(QUERY, FROM, NOW),
      service.get(QUERY, FROM, NOW),
    ]);
    expect(first.status).toBe('loading');
    expect(second.status).toBe('loading');
    expect(calls).toBe(1);
    expect(first.totals).toBeNull();
    resolve(data('gpt-5.4', 20, 0.001));
    await pending;
    await new Promise((complete) => setTimeout(complete, 0));
    const completed = await service.get(QUERY, FROM, NOW);
    expect(completed.status).toBe('ok');
    expect(completed.totals?.inputTokens).toBe(20);
    expect(calls).toBe(1);
  });

  it('refreshes both native providers before the normal cache expires, while ordinary reads stay cached', async () => {
    let input = 10;
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        return data('fixture-model', input, 0);
      },
      now: () => NOW,
      scope: () => '/fixture',
    });
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(20);
    input = 25;
    expect((await service.get({ ...QUERY, refresh: false }, FROM, NOW)).totals?.inputTokens).toBe(
      20
    );
    expect(calls).toBe(2);
    const refreshing = await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect(refreshing.status).toBe('cached');
    expect(refreshing.refreshing).toBe(true);
    expect(refreshing.totals?.inputTokens).toBe(20);
    const refreshed = await settled(service);
    expect(refreshed.status).toBe('ok');
    expect(refreshed.totals?.inputTokens).toBe(50);
    expect(refreshed.providers.map((row) => row.provider)).toEqual(['claude', 'codex']);
    expect(calls).toBe(4);
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(50);
    expect(calls).toBe(4);
  });

  it('uses the saved refresh interval for ordinary reads and picks up an interval change immediately', async () => {
    let now = NOW;
    let refreshIntervalSeconds = 120;
    let input = 10;
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        return data('gpt-5.4', input, 0);
      },
      now: () => now,
      refreshIntervalSeconds: () => refreshIntervalSeconds,
      scope: () => '/fixture',
    });
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(10);
    input = 20;
    now += 119_999;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(10);
    expect(calls).toBe(1);
    now++;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(10);
    expect((await settled(service, QUERY, FROM, now)).totals?.inputTokens).toBe(20);
    expect(calls).toBe(2);
    input = 30;
    now += 30_000;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(20);
    expect(calls).toBe(2);
    refreshIntervalSeconds = 30;
    expect((await service.get(QUERY, FROM, now)).totals?.inputTokens).toBe(20);
    expect((await settled(service, QUERY, FROM, now)).totals?.inputTokens).toBe(30);
    expect(calls).toBe(3);
  });

  it('serves the cached snapshot instantly while a refresh runs, spending the budget only with nothing to serve', async () => {
    let calls = 0;
    let elapsed = 0;
    const requests = () => [
      {
        provider: 'codex' as const,
        request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
      },
    ];
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests,
      loadWorker: async () => {
        calls++;
        if (calls === 1) return data('gpt-5.4', 10, 0);
        return new Promise<UsageWorkerResult>(() => {});
      },
      now: () => NOW,
      scope: () => '/fixture-budget',
      responseBudgetMs: 1000,
      // Every projection appears to cost 900 ms of the 1,000 ms budget.
      elapsedMs: () => (elapsed += 900),
    });
    expect((await service.get(QUERY, FROM, NOW)).status).toBe('ok');
    const started = performance.now();
    const refreshing = await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    const waited = performance.now() - started;
    expect(refreshing.status).toBe('cached');
    expect(refreshing.refreshing).toBe(true);
    expect(refreshing.totals?.inputTokens).toBe(10);
    // The cached snapshot answers at once; the hung refresh never holds it.
    expect(waited).toBeLessThan(600);
    // With nothing to serve, a first run still waits, bounded by the budget minus recent
    // projection time: about 50 ms here (200 ms budget, 150 ms projections), never the whole
    // budget on top of the projection.
    let coldElapsed = 0;
    const cold = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests,
      loadWorker: () => new Promise<UsageWorkerResult>(() => {}),
      now: () => NOW,
      scope: () => '/fixture-budget-cold',
      responseBudgetMs: 200,
      elapsedMs: () => (coldElapsed += 150),
    });
    expect((await cold.get(QUERY, FROM, NOW)).status).toBe('loading');
    const coldStarted = performance.now();
    const loading = await cold.get(QUERY, FROM, NOW);
    const coldWaited = performance.now() - coldStarted;
    expect(loading.status).toBe('loading');
    expect(loading.totals).toBeNull();
    expect(coldWaited).toBeGreaterThanOrEqual(40);
    expect(coldWaited).toBeLessThan(600);
  });

  it('coalesces concurrent manual refreshes into one bounded scan and exposes cached data while it runs', async () => {
    let pending = false;
    let calls = 0;
    let live = 0;
    let maximum = 0;
    const resolvers: Array<(value: UsageWorkerResult) => void> = [];
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        if (!pending) return data('fixture-model', 10, 0);
        live++;
        maximum = Math.max(maximum, live);
        return new Promise((resolve) => {
          resolvers.push((value) => {
            live--;
            resolve(value);
          });
        });
      },
      now: () => NOW,
      scope: () => '/fixture',
      responseBudgetMs: 5,
    });
    await service.get(QUERY, FROM, NOW);
    pending = true;
    const reads = await Promise.all(
      Array.from({ length: 40 }, () => service.get({ ...QUERY, refresh: true }, FROM, NOW))
    );
    expect(calls).toBe(4);
    expect(maximum).toBe(2);
    expect(reads.every((row) => row.status === 'cached' && row.totals?.inputTokens === 20)).toBe(
      true
    );
    expect(reads[0].message).toContain('refreshing');
    resolvers.forEach((resolve) => resolve(data('fixture-model', 30, 0)));
    await new Promise((complete) => setTimeout(complete, 0));
    const completed = await service.get(QUERY, FROM, NOW);
    expect(completed.status).toBe('ok');
    expect(completed.totals?.inputTokens).toBe(60);
    expect(calls).toBe(4);
  });

  it('queues one fresh generation behind an old scan and prevents its obsolete result from overwriting the cache', async () => {
    let now = NOW;
    let calls = 0;
    const resolvers: Array<(value: UsageWorkerResult) => void> = [];
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        if (calls === 1) return data('gpt-5.4', 10, 0);
        return new Promise((resolve) => resolvers.push(resolve));
      },
      now: () => now,
      scope: () => '/fixture',
      responseBudgetMs: 5,
    });
    await service.get(QUERY, FROM, now);
    now += 6 * 60_000;
    await service.get(QUERY, FROM, now);
    expect(calls).toBe(2);
    await Promise.all(
      Array.from({ length: 20 }, () => service.get({ ...QUERY, refresh: true }, FROM, now))
    );
    expect(calls).toBe(2);
    resolvers[0](data('gpt-5.4', 999, 0));
    await new Promise((complete) => setTimeout(complete, 0));
    expect(calls).toBe(3);
    const during = await service.get(QUERY, FROM, now);
    expect(during.status).toBe('cached');
    expect(during.totals?.inputTokens).toBe(10);
    expect(during.fetchedAt).toBe(new Date(NOW).toISOString());
    await service.get({ ...QUERY, refresh: true }, FROM, now);
    expect(calls).toBe(3);
    resolvers[1](data('gpt-5.4', 30, 0));
    await new Promise((complete) => setTimeout(complete, 0));
    const completed = await service.get(QUERY, FROM, now);
    expect(completed.status).toBe('ok');
    expect(completed.totals?.inputTokens).toBe(30);
    expect(completed.fetchedAt).toBe(new Date(now).toISOString());
    expect(calls).toBe(3);
  });

  it('refreshes all native sources even when the selected account cannot show attributable activity', async () => {
    let input = 10;
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
        {
          provider: 'codex',
          request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => {
        calls++;
        return data('fixture-model', input, 0);
      },
      now: () => NOW,
      scope: () => '/fixture',
    });
    await service.get(QUERY, FROM, NOW);
    input = 40;
    const selected = await service.get(
      { ...QUERY, provider: 'qwen', account: 'qwen:first', refresh: true },
      FROM,
      NOW
    );
    expect(selected.status).toBe('unavailable');
    expect(selected.totals).toBeNull();
    expect((await settled(service)).totals?.inputTokens).toBe(80);
    expect(calls).toBe(4);
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(80);
    expect(calls).toBe(4);
  });

  it('keeps last usable records and their original timestamp when an explicit refresh fails', async () => {
    let now = NOW;
    let fail = false;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
      ],
      loadWorker: async () => {
        if (fail) throw new Error('raw secret source sentinel');
        return data('claude-sonnet-4-6', 10, 0.002);
      },
      now: () => now,
      scope: () => '/fixture',
    });
    await service.get(QUERY, FROM, now);
    now += 1000;
    fail = true;
    const retained = await service.get({ ...QUERY, refresh: true }, FROM, now);
    expect(retained.status).toBe('cached');
    expect(retained.totals?.inputTokens).toBe(10);
    expect(retained.fetchedAt).toBe(new Date(NOW).toISOString());
    expect(JSON.stringify(retained)).not.toContain('sentinel');
  });

  it('retains the last usable native history on later worker failures without exposing raw error details', async () => {
    let now = NOW;
    let fail = false;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [
        { provider: 'claude', request: { kind: 'claude', projectsDir: '/fixture/projects' } },
      ],
      loadWorker: async () => {
        if (fail) throw new Error('raw secret source sentinel');
        return data('claude-sonnet-4-6', 10, 0.002);
      },
      now: () => now,
      scope: () => '/fixture',
      responseBudgetMs: 20,
    });
    expect((await service.get(QUERY, FROM, NOW)).status).toBe('ok');
    now += 6 * 60_000;
    fail = true;
    const retained = await service.get(QUERY, FROM, now);
    expect(retained.status).toBe('cached');
    expect(retained.totals?.inputTokens).toBe(10);
    expect(retained.fetchedAt).toBe(new Date(NOW).toISOString());
    expect(JSON.stringify(retained)).not.toContain('sentinel');
  });

  it('starts no worker for an exact-account filter or a bare tool id, and filters every usage provider in the projection', async () => {
    let calls = 0;
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => {
        calls++;
        return [{ provider: 'omp', request: { kind: 'omp', roots: ['/fixture/omp'] } }];
      },
      loadWorker: async () => data('qwen3.8-max', 10, 0.002, 'alibaba-token-plan'),
      now: () => NOW,
    });
    // Tool ids are not usage providers; only routed providers and `all` select.
    const tool = await service.get({ ...QUERY, provider: 'omp' as 'qwen' }, FROM, NOW);
    const account = await service.get({ ...QUERY, account: 'codex:active' }, FROM, NOW);
    expect(calls).toBe(0);
    expect(tool.totals).toBeNull();
    expect(account.totals).toBeNull();
    // Every provider the route validates reaches the projection, which keeps the usage it served.
    const qwen = await service.get({ ...QUERY, provider: 'qwen' }, FROM, NOW);
    expect(calls).toBe(1);
    expect(qwen.providers.map((row) => row.provider)).toEqual(['qwen']);
    expect(qwen.totals?.inputTokens).toBe(10);
    // A provider with nothing in the range says so, instead of reading as a broken filter.
    const empty = await service.get({ ...QUERY, provider: 'zai' }, FROM, NOW);
    expect(empty.totals).toBeNull();
    expect(empty.message).toContain('No CLI usage log in this range was served by this provider');
  });

  it('projects activity for routed providers served by other tools', async () => {
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => [],
      now: () => NOW,
    });
    const qwen = await service.get({ ...QUERY, provider: 'qwen' }, FROM, NOW);
    // A routed provider with no usage in range reports unavailable with a
    // provider-specific message, not the unsupported-selection refusal.
    expect(qwen.status).toBe('unavailable');
    expect(qwen.message).toContain('served by this provider');
  });

  it('merges remote sessions into the snapshot like local ones', async () => {
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({
        results: [{ tool: 'omp' as const, data: data('some-model', 100, 0.005) }],
        states: [],
      }),
      requests: () => [],
      now: () => NOW,
    });
    const answer = await settled(service);
    expect(answer.status).toBe('ok');
    expect(answer.totals?.inputTokens).toBe(100);
    expect(answer.sessions?.total).toBe(1);
    expect(answer.sessions?.sample).toHaveLength(1);
    expect(answer.sessions?.sample[0].provider).toBe('other');
  });

  it('lists every remote kind when the remote scans never answer', async () => {
    const service = new AccountAnalyticsActivityService({
      remote: async () => {
        throw new Error('no route to host');
      },
      remoteCached: () => null,
      requests: () => [],
      now: () => NOW,
    });
    const answer = await settled(service);
    expect(answer.status).toBe('unavailable');
    const remote = answer.sources.filter(
      (entry) =>
        entry.host !== 'ubuntu' && ['claude', 'codex', 'omp', 'muse', 'zcode'].includes(entry.tool)
    );
    // Five tools on each of the three remote computers.
    expect(remote).toHaveLength(15);
    for (const tool of ['claude', 'codex', 'omp', 'muse', 'zcode'] as const) {
      for (const host of ['mac', 'windows', 'nas1'] as const) {
        expect(remote).toContainEqual(
          expect.objectContaining({ tool, host, state: 'unavailable' })
        );
      }
    }
  });

  it('persists generic jsonl sources in the snapshot', async () => {
    const scope = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activity-snapshot-jsonl-'));
    try {
      const requests = () => [
        {
          provider: 'jsonl' as const,
          request: {
            kind: 'jsonl' as const,
            roots: ['/fixture'],
            mapping: { timestamp: 'ts' },
          },
        },
      ];
      const first = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests,
        loadWorker: async () => data('generic-model', 10, 0),
        now: () => NOW,
        scope: () => scope,
        persistSnapshot: true,
      });
      expect((await first.get(QUERY, FROM, NOW)).status).toBe('ok');
      const file = path.join(
        scope,
        'cache',
        'account-activity-v1',
        'analytics-activity-snapshot-v1.json'
      );
      expect(JSON.parse(fs.readFileSync(file, 'utf8')).sources).toHaveLength(1);
      const second = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests,
        loadWorker: async () => data('generic-model', 20, 0),
        now: () => NOW + 61_000,
        scope: () => scope,
        persistSnapshot: true,
      });
      const instant = await second.get(QUERY, FROM, NOW);
      expect(instant.status).toBe('cached');
      expect(instant.totals?.inputTokens).toBe(10);
    } finally {
      fs.rmSync(scope, { recursive: true, force: true });
    }
  });

  it('persists the snapshot and serves it instantly after a restart while the first scan runs', async () => {
    const scope = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activity-snapshot-'));
    try {
      const requests = () => [
        {
          provider: 'codex' as const,
          request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ];
      const first = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests,
        loadWorker: async () => data('gpt-5.4', 10, 0),
        now: () => NOW,
        scope: () => scope,
        persistSnapshot: true,
      });
      expect((await first.get(QUERY, FROM, NOW)).status).toBe('ok');
      const file = path.join(
        scope,
        'cache',
        'account-activity-v1',
        'analytics-activity-snapshot-v1.json'
      );
      expect(fs.existsSync(file)).toBe(true);
      expect(JSON.parse(fs.readFileSync(file, 'utf8')).sources).toHaveLength(1);
      // A restarted server (a new instance on the same scope) serves the snapshot at once, with
      // the first scan running behind it, instead of answering 'loading' with no data.
      const second = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests,
        loadWorker: async () => data('gpt-5.4', 20, 0),
        now: () => NOW + 61_000,
        scope: () => scope,
        persistSnapshot: true,
      });
      const instant = await second.get(QUERY, FROM, NOW);
      expect(instant.status).toBe('cached');
      expect(instant.refreshing).toBe(true);
      expect(instant.totals?.inputTokens).toBe(10);
      const fresh = await settled(second);
      expect(fresh.status).toBe('ok');
      expect(fresh.refreshing).toBe(false);
      expect(fresh.totals?.inputTokens).toBe(20);
    } finally {
      fs.rmSync(scope, { recursive: true, force: true });
    }
  });

  it('ignores a missing or invalid persisted snapshot like a first run', async () => {
    const scope = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activity-snapshot-'));
    try {
      const dir = path.join(scope, 'cache', 'account-activity-v1');
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, 'analytics-activity-snapshot-v1.json'), 'not json{');
      const service = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: [] }),
        requests: () => [
          {
            provider: 'codex' as const,
            request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
          },
        ],
        loadWorker: async () => data('gpt-5.4', 10, 0),
        now: () => NOW,
        scope: () => scope,
        persistSnapshot: true,
        responseBudgetMs: 50,
      });
      // The corrupt file is not served and does not fail the collection; the scan fills it in.
      expect((await service.get(QUERY, FROM, NOW)).status).toBe('ok');
      expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(10);
    } finally {
      fs.rmSync(scope, { recursive: true, force: true });
    }
  });

  it('publishes fresh local rows before a slow remote scan answers, keeping carried remote states', async () => {
    let input = 10;
    let releaseRemote: ((value: { results: []; states: [] }) => void) | null = null;
    let remoteCalls = 0;
    const remoteData = data('remote-model', 70, 0);
    const service = new AccountAnalyticsActivityService({
      remote: async () => {
        remoteCalls++;
        if (remoteCalls === 1)
          return { results: [{ tool: 'omp' as const, data: remoteData }], states: [] };
        return new Promise<{ results: []; states: [] }>((resolve) => {
          releaseRemote = resolve;
        });
      },
      requests: () => [
        {
          provider: 'codex' as const,
          request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => data('gpt-5.4', input, 0),
      now: () => NOW,
      scope: () => '/fixture-phased',
    });
    expect((await service.get(QUERY, FROM, NOW)).totals?.inputTokens).toBe(80);
    input = 20;
    await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    // The local phase publishes while the remote scan is still gated: fresh local rows (20)
    // plus the carried remote rows (70), with the refresh still running behind the answer.
    let phased = await service.get(QUERY, FROM, NOW);
    for (let attempt = 0; attempt < 500 && phased.totals?.inputTokens !== 90; attempt++) {
      await new Promise((resolve) => setTimeout(resolve, 5));
      phased = await service.get(QUERY, FROM, NOW);
    }
    expect(phased.totals?.inputTokens).toBe(90);
    expect(phased.refreshing).toBe(true);
    expect(releaseRemote).not.toBeNull();
    releaseRemote?.({ results: [], states: [] });
    const done = await settled(service);
    expect(done.refreshing).toBe(false);
    // The answered (empty) remote scan replaces the carried rows; only fresh local rows remain.
    expect(done.totals?.inputTokens).toBe(20);
  });

  it('starts every collector at once up to its bound, and a slow one never holds back the rest', async () => {
    const releases = new Map<string, (data: UsageWorkerResult) => void>();
    const started: string[] = [];
    let live = 0;
    let maximum = 0;
    const budgets: number[] = [];
    const requests: Array<{ provider: 'claude'; request: UsageWorkerRequest }> = Array.from(
      { length: 5 },
      (_, index) => ({
        provider: 'claude',
        request: { kind: 'claude', projectsDir: `/fixture/${index}` },
      })
    );
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () => requests,
      now: () => NOW,
      responseBudgetMs: 5,
      concurrency: 3,
      loadWorker: async (request, budgetMs) => {
        const root = request.kind === 'claude' ? request.projectsDir : '';
        started.push(root);
        budgets.push(budgetMs ?? -1);
        live++;
        maximum = Math.max(maximum, live);
        return new Promise((resolve) => {
          releases.set(root, (result) => {
            live--;
            resolve(result);
          });
        });
      },
    });
    await service.get(QUERY, FROM, NOW);
    const tick = () => new Promise((complete) => setTimeout(complete, 0));
    await tick();
    expect(started).toEqual(['/fixture/0', '/fixture/1', '/fixture/2']);
    // /fixture/0 stays slow: every other finish starts the next root at once, no batch waits.
    releases.get('/fixture/1')?.(data('claude-sonnet-4-6', 1, 0));
    await tick();
    expect(started).toEqual(['/fixture/0', '/fixture/1', '/fixture/2', '/fixture/3']);
    releases.get('/fixture/2')?.(data('claude-sonnet-4-6', 1, 0));
    await tick();
    expect(started).toHaveLength(5);
    expect(maximum).toBe(3);
    releases.get('/fixture/3')?.(data('claude-sonnet-4-6', 1, 0));
    releases.get('/fixture/4')?.(data('claude-sonnet-4-6', 1, 0));
    await tick();
    // Nothing publishes until the slow root has answered too.
    expect((await service.get(QUERY, FROM, NOW)).totals).toBeNull();
    releases.get('/fixture/0')?.(data('claude-sonnet-4-6', 1, 0));
    expect((await settled(service)).totals?.inputTokens).toBe(5);
    // Each collector keeps its own budget: what is left of the collection deadline.
    for (const budget of budgets) {
      expect(budget).toBeGreaterThan(0);
      expect(budget).toBeLessThanOrEqual(60_000);
    }
  });

  it('runs all local collectors together by default and never fewer than two', async () => {
    let live = 0;
    let maximum = 0;
    const releases: Array<() => void> = [];
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [] }),
      requests: () =>
        (['claude', 'codex', 'omp', 'muse', 'zcode'] as const).map((provider) => ({
          provider,
          request: { kind: 'claude', projectsDir: `/fixture/${provider}` } as UsageWorkerRequest,
        })),
      now: () => NOW,
      responseBudgetMs: 5,
      concurrency: 8,
      loadWorker: async () => {
        live++;
        maximum = Math.max(maximum, live);
        await new Promise<void>((resolve) => releases.push(resolve));
        live--;
        return data('claude-sonnet-4-6', 1, 0);
      },
    });
    await service.get(QUERY, FROM, NOW);
    await new Promise((complete) => setTimeout(complete, 0));
    expect(maximum).toBe(5);
    for (const release of releases) release();
    expect((await settled(service)).totals?.inputTokens).toBe(5);
  });

  it('starts the remote scans before local discovery has finished', async () => {
    const order: string[] = [];
    let finishDiscovery: () => void = () => {};
    const service = new AccountAnalyticsActivityService({
      remote: async () => {
        order.push('remote');
        return { results: [], states: [] };
      },
      requests: async () => {
        await new Promise<void>((resolve) => {
          finishDiscovery = resolve;
        });
        order.push('discovered');
        return [
          {
            provider: 'codex',
            request: { kind: 'codex', codexHome: '/fixture', cacheDir: '/fixture/cache' },
          },
        ];
      },
      now: () => NOW,
      responseBudgetMs: 5,
      loadWorker: async () => {
        order.push('local');
        return data('gpt-5.4', 7, 0.01);
      },
    });
    await service.get(QUERY, FROM, NOW);
    await new Promise((complete) => setTimeout(complete, 0));
    expect(order).toEqual(['remote']);
    finishDiscovery();
    expect((await settled(service)).totals?.inputTokens).toBe(7);
    expect(order).toEqual(['remote', 'discovered', 'local']);
  });

  it('rescans remotes on manual refresh and names the hosts it waits on while the scan runs', async () => {
    let remoteCalls = 0;
    let release = () => {};
    const service = new AccountAnalyticsActivityService({
      remote: async (_minDateMs, opts) => {
        remoteCalls++;
        opts?.onHostScan?.('mac', 'start');
        opts?.onHostScan?.('windows', 'start');
        opts?.onHostScan?.('nas1', 'start');
        await new Promise<void>((resolve) => {
          release = resolve;
        });
        opts?.onHostScan?.('mac', 'done');
        opts?.onHostScan?.('windows', 'done');
        opts?.onHostScan?.('nas1', 'done');
        return { results: [], states: [] };
      },
      requests: () => [],
      now: () => NOW,
      responseBudgetMs: 20,
      scope: () => '/fixture-refresh-remote',
    });
    // The first answer arrives while the remote scan is still running: the
    // page can say which hosts the refresh waits on.
    const flying = await service.get(QUERY, FROM, NOW);
    expect(flying.refreshing).toBe(true);
    expect(flying.refreshingRemote).toEqual(['mac', 'windows', 'nas1']);
    release();
    const first = await settled(service);
    expect(first.refreshing).toBe(false);
    expect(first.refreshingRemote).toEqual([]);
    expect(remoteCalls).toBe(1);
    // A manual refresh runs the remote scan again instead of serving the cache.
    const refreshing = await service.get({ ...QUERY, refresh: true }, FROM, NOW);
    expect(refreshing.refreshing).toBe(true);
    expect(refreshing.refreshingRemote).toEqual(['mac', 'windows', 'nas1']);
    release();
    const second = await settled(service);
    expect(second.refreshing).toBe(false);
    expect(second.refreshingRemote).toEqual([]);
    expect(remoteCalls).toBe(2);
  });

  it('answers a cold first run with an all-scanning grid while the collection starts', async () => {
    const remote = Promise.withResolvers<{ results: []; states: [] }>();
    const gate = Promise.withResolvers<AccountAnalyticsActivityRequest[]>();
    const service = new AccountAnalyticsActivityService({
      remote: async () => remote.promise,
      // Held open until after the cold answer: the first response is the synthesized grid.
      requests: async () => gate.promise,
      now: () => NOW,
      responseBudgetMs: 0,
      scope: () => '/fixture-cold-grid',
    });
    const cold = await service.get(QUERY, FROM, NOW);
    expect(cold.status).toBe('loading');
    // Every scanned tool on every host says scanning from the first byte, so the
    // loading page can show per-host progress; the fixed entries ride along.
    const scanning = cold.sources.filter((source) => source.state === 'scanning');
    // Six tools on each of Ubuntu, Mac, Windows and Nas1.
    expect(scanning).toHaveLength(24);
    expect(scanning.every((source) => source.detail === 'the first scan is running')).toBe(true);
    // Antigravity is scanned on every host now; Cursor stays a fixed no-local-log entry.
    expect(
      cold.sources.filter((source) => source.tool === 'antigravity' && source.state === 'scanning')
    ).toHaveLength(4);
    expect(cold.sources.filter((source) => source.tool === 'cursor')).toHaveLength(4);
    gate.resolve([]);
    remote.resolve({ results: [], states: [] });
    await settled(service);
  });

  it('lists the fixed Cursor row on every computer, Nas1 included', () => {
    const fixed = fixedAnalyticsSourceEntries();
    expect(fixed.map((entry) => `${entry.tool}:${entry.host}`)).toEqual([
      'cursor:ubuntu',
      'cursor:mac',
      'cursor:windows',
      'cursor:nas1',
    ]);
    expect(fixed.every((entry) => entry.state === 'unavailable' && entry.rowCount === 0)).toBe(
      true
    );
    expect(fixed.every((entry) => entry.detail?.includes('usage is server-side'))).toBe(true);
  });

  it('orders the cold grid by tool, then Ubuntu, Mac, Windows and Nas1', () => {
    const cold = coldScanningSourceStates();
    // Six scanned tools on four computers, plus the four fixed Cursor rows.
    expect(cold).toHaveLength(28);
    const hostsOf = (tool: string) =>
      cold.filter((entry) => entry.tool === tool).map((entry) => entry.host);
    for (const tool of ['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity', 'cursor'])
      expect(hostsOf(tool)).toEqual(['ubuntu', 'mac', 'windows', 'nas1']);
    // Generic JSONL sources are local to Ubuntu: never a cold cell, on Nas1 or anywhere else.
    expect(hostsOf('jsonl')).toEqual([]);
    const tools = cold.map((entry) => entry.tool);
    expect(tools.indexOf('claude')).toBeLessThan(tools.indexOf('antigravity'));
    expect(tools.indexOf('antigravity')).toBeLessThan(tools.indexOf('cursor'));
  });

  it('shows every remote computer scanning, Nas1 included, until the first remote answer', async () => {
    const remote = Promise.withResolvers<{ results: []; states: [] }>();
    const service = new AccountAnalyticsActivityService({
      remote: async () => remote.promise,
      requests: () => [
        {
          provider: 'codex' as const,
          request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ],
      loadWorker: async () => data('gpt-5.4', 10, 0),
      now: () => NOW,
      responseBudgetMs: 200,
      scope: () => '/fixture-first-remote-answer',
    });
    // The local rows publish while the remote scan is still running behind them.
    const answer = await service.get(QUERY, FROM, NOW);
    expect(answer.refreshing).toBe(true);
    for (const host of ['mac', 'windows', 'nas1'] as const) {
      const cells = answer.sources.filter(
        (entry) => entry.host === host && entry.tool !== 'cursor'
      );
      expect(cells.map((entry) => entry.tool).sort()).toEqual([
        'antigravity',
        'claude',
        'codex',
        'muse',
        'omp',
        'zcode',
      ]);
      expect(cells.every((entry) => entry.state === 'scanning')).toBe(true);
    }
    remote.resolve({ results: [], states: [] });
    await settled(service);
  });

  it('keeps the four-computer grid inside the snapshot state cap and serves it after a restart', async () => {
    const scope = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activity-snapshot-nas1-'));
    try {
      const remoteStates = (['mac', 'windows', 'nas1'] as const).flatMap((host) =>
        (['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity'] as const).map((tool) => ({
          tool,
          host,
          state: 'no_usage' as const,
          lastScanAt: new Date(NOW).toISOString(),
          rowCount: 0,
          detail: 'no usage recorded in the last 31 days',
        }))
      );
      const requests = () => [
        {
          provider: 'codex' as const,
          request: { kind: 'codex' as const, codexHome: '/fixture', cacheDir: '/fixture/cache' },
        },
      ];
      const first = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: remoteStates }),
        requests,
        loadWorker: async () => data('gpt-5.4', 10, 0),
        now: () => NOW,
        scope: () => scope,
        persistSnapshot: true,
      });
      expect((await first.get(QUERY, FROM, NOW)).status).toBe('ok');
      const file = path.join(
        scope,
        'cache',
        'account-activity-v1',
        'analytics-activity-snapshot-v1.json'
      );
      const saved = JSON.parse(fs.readFileSync(file, 'utf8')) as {
        sourceStates: Array<{ tool: string; host: string }>;
      };
      // Seven local tool cells (Generic JSONL included), six tools on each of the three remote
      // computers and four Cursor rows: 29 states against the validator's cap of 64.
      expect(saved.sourceStates).toHaveLength(29);
      expect(saved.sourceStates.length).toBeLessThanOrEqual(64);
      expect(new Set(saved.sourceStates.map((entry) => entry.host))).toEqual(
        new Set(['ubuntu', 'mac', 'windows', 'nas1'])
      );
      // A restarted server accepts the snapshot and serves the Nas1 cells from it at once.
      const second = new AccountAnalyticsActivityService({
        remote: async () => ({ results: [], states: remoteStates }),
        requests,
        loadWorker: async () => data('gpt-5.4', 20, 0),
        now: () => NOW + 61_000,
        scope: () => scope,
        persistSnapshot: true,
      });
      const instant = await second.get(QUERY, FROM, NOW);
      expect(instant.status).toBe('cached');
      expect(instant.sources.filter((entry) => entry.host === 'nas1')).toHaveLength(7);
      await settled(second);
    } finally {
      fs.rmSync(scope, { recursive: true, force: true });
    }
  });

  it('re-collects on the converge cadence while the grid holds scanning cells, then falls back', async () => {
    let clock = NOW;
    let requestCalls = 0;
    const scanningStates = (['claude', 'codex', 'omp', 'muse', 'zcode'] as const).map((tool) => ({
      tool,
      host: 'mac' as const,
      state: 'scanning' as const,
      lastScanAt: null,
      rowCount: 0,
      detail: 'the scan ran out of time before it reached this tool; the next scan continues',
    }));
    const service = new AccountAnalyticsActivityService({
      remote: async () => ({ results: [], states: [...scanningStates] }),
      requests: () => {
        requestCalls++;
        return [];
      },
      now: () => clock,
      refreshIntervalSeconds: () => 60,
      responseBudgetMs: 0,
      scope: () => '/fixture-converge-cadence',
    });
    await service.get(QUERY, FROM, NOW);
    await settled(service);
    expect(requestCalls).toBe(1);
    // 25 s after the first publish: inside the configured 60 s interval, but the
    // bounded converge window lowers the floor to 20 s, so a collection runs.
    clock = NOW + 25_000;
    await service.get(QUERY, FROM, NOW);
    await settled(service);
    expect(requestCalls).toBe(2);
    // Past 10 min from the first scanning cell the floor expires: the configured
    // interval applies again (25 s after the last publish collects nothing).
    clock = NOW + 11 * 60_000;
    await service.get(QUERY, FROM, NOW);
    await settled(service);
    expect(requestCalls).toBe(3);
    clock = NOW + 11 * 60_000 + 25_000;
    await service.get(QUERY, FROM, NOW);
    await settled(service);
    expect(requestCalls).toBe(3);
  });
});
