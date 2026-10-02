import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  parseAnalyticsRemoteResponse,
  runAnalyticsRemoteHelper,
} from '../../../src/web-server/services/analytics-remote-transport';
import { loadAnalyticsRemoteSources } from '../../../src/web-server/services/analytics-remote-sources';

const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');
let cache: string;
beforeEach(() => {
  cache = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-remote-sources-'));
});
afterEach(() => fs.rmSync(cache, { recursive: true, force: true }));

function row(overrides: Record<string, unknown> = {}) {
  return {
    k: 'omp',
    f: 'file-1',
    m: 'deepseek-v4.1-flash',
    h: '2026-10-01 15:00',
    i: 100,
    o: 10,
    cr: 50,
    cw: 5,
    c: 0.01,
    n: 2,
    ...overrides,
  };
}

function response(overrides: Record<string, unknown> = {}) {
  return {
    version: 1,
    truncated: false,
    kinds: {
      omp: { state: 'ok', fingerprints: { 'file-1': { size: 10, mtimeMs: 20 } } },
      muse: { state: 'not_installed', fingerprints: {} },
      zcode: { state: 'ok', fingerprints: {} },
    },
    rows: [row()],
    ...overrides,
  };
}

describe('analytics remote transport', () => {
  it('parses bounded aggregate responses', () => {
    const parsed = parseAnalyticsRemoteResponse(JSON.stringify(response()));
    expect(parsed.rows).toHaveLength(1);
    expect(parsed.kinds.muse.state).toBe('not_installed');
  });

  it('rejects malformed, oversized or unknown-kind payloads', () => {
    expect(() => parseAnalyticsRemoteResponse('not json')).toThrow();
    expect(() => parseAnalyticsRemoteResponse(JSON.stringify(response({ version: 2 })))).toThrow();
    expect(() =>
      parseAnalyticsRemoteResponse(JSON.stringify(response({ rows: [{ ...row(), h: 'soon' }] })))
    ).toThrow();
    expect(() =>
      parseAnalyticsRemoteResponse(
        JSON.stringify(response({ kinds: { cursor: { state: 'ok', fingerprints: {} } } }))
      )
    ).toThrow();
    expect(() =>
      parseAnalyticsRemoteResponse(JSON.stringify(response({ rows: [row(), { ...row(), c: -1 }] })))
    ).toThrow();
  });

  it('refuses unsafe ssh aliases and bad requests without running ssh', async () => {
    await expect(
      runAnalyticsRemoteHelper('bad;alias', 'mac', {
        kinds: ['omp'],
        minDateMs: MIN_DATE,
        fingerprints: {},
      })
    ).rejects.toThrow();
    await expect(
      runAnalyticsRemoteHelper('fine-alias', 'mac', {
        kinds: [],
        minDateMs: MIN_DATE,
        fingerprints: {},
      })
    ).rejects.toThrow();
  });
});

describe('analytics remote sources', () => {
  const hosts = async () => ({ mac: 'mac-alias', windows: 'win-alias' });

  it('merges per-host aggregates and converts them to worker results', async () => {
    const { results, states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      now: () => Date.parse('2026-10-02T00:00:00Z'),
      runHelper: async (_alias, platform) =>
        parseAnalyticsRemoteResponse(
          JSON.stringify(
            response({
              kinds: {
                omp: { state: 'ok', fingerprints: { 'file-1': { size: 10, mtimeMs: 20 } } },
              },
              rows: [row({ f: `file-${platform}` })],
            })
          )
        ),
    });
    expect(results.map((entry) => entry.tool)).toEqual(['omp', 'omp']);
    expect(results[0].data.hourly[0].modelBreakdowns[0].modelName).toBe('deepseek-v4.1-flash');
    expect(results[0].data.eventCount).toBe(2);
    const ompMac = states.find((entry) => entry.tool === 'omp' && entry.host === 'mac');
    expect(ompMac).toMatchObject({ state: 'ok', rowCount: 2 });
    expect(ompMac?.lastScanAt).toBe('2026-10-02T00:00:00.000Z');
    // Muse and zcode are mac-only; windows carries omp alone.
    expect(states.filter((entry) => entry.host === 'windows').map((entry) => entry.tool)).toEqual([
      'omp',
    ]);
  });

  it('keeps changed-file rows incremental across scans', async () => {
    const runHelper = async () => parseAnalyticsRemoteResponse(JSON.stringify(response()));
    await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper });
    const changed = response({
      kinds: {
        omp: { state: 'ok', fingerprints: { 'file-1': { size: 11, mtimeMs: 21 } } },
      },
      rows: [row({ i: 7 })],
    });
    const { results } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(changed)),
    });
    const omp = results.find((entry) => entry.tool === 'omp');
    // The stale file-1 rows were replaced, not added to.
    expect(omp?.data.hourly[0].inputTokens).toBe(7);
  });

  it('keeps unvisited rows when a scan truncates, retrying them next time', async () => {
    const runHelper = async () => parseAnalyticsRemoteResponse(JSON.stringify(response()));
    await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper });
    const truncated = response({
      truncated: true,
      kinds: { omp: { state: 'ok', fingerprints: {} } },
      rows: [],
    });
    const { results, states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(truncated)),
    });
    const omp = results.find((entry) => entry.tool === 'omp');
    expect(omp?.data.hourly[0].inputTokens).toBe(100);
    expect(states.find((entry) => entry.tool === 'omp' && entry.host === 'mac')?.state).toBe(
      'cached'
    );
  });

  it('serves cached aggregates when the helper times out, without throwing', async () => {
    const runHelper = async () => parseAnalyticsRemoteResponse(JSON.stringify(response()));
    await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper });
    const failing = async (): Promise<never> => {
      throw new Error('timed out');
    };
    const { results, states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: failing,
    });
    expect(results.length).toBeGreaterThan(0);
    expect(states.find((entry) => entry.tool === 'omp' && entry.host === 'mac')?.state).toBe(
      'cached'
    );
  });

  it('reports unavailable with no cache and unconfigured hosts', async () => {
    const failing = async (): Promise<never> => {
      throw new Error('no route');
    };
    const { results, states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: failing,
    });
    expect(results).toEqual([]);
    expect(states.every((entry) => entry.state === 'unavailable')).toBe(true);
    const unconfigured = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts: async () => ({ mac: null, windows: null }),
      cacheDir: cache,
      runHelper: failing,
    });
    expect(unconfigured.results).toEqual([]);
    expect(
      unconfigured.states.every((entry) => entry.detail === 'remote host is not configured')
    ).toBe(true);
  });
});
