import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  ANALYTICS_HELPER_SHA256,
  analyticsHelperPath,
  parseAnalyticsRemoteResponse,
  runAnalyticsRemoteHelper,
} from '../../../src/web-server/services/analytics-remote-transport';
import {
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
} from '../../../src/web-server/services/analytics-remote-sources';
import { analyticsSessionKey } from '../../../src/web-server/usage/analytics-session-key';
import {
  defaultDashboardPreferences,
  writeDashboardPreferences,
} from '../../../src/web-server/services/dashboard-preferences';

const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');
let cache: string;
beforeEach(() => {
  cache = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-remote-sources-'));
});
afterEach(() => fs.rmSync(cache, { recursive: true, force: true }));

// The helper's file keys are SHA-256 hex digests; the server refuses anything else.
const FILE_1 = 'a1'.repeat(32);
const DB_1 = 'd1'.repeat(32);
// A session key is the helper's truncated digest of the log's own session id; the id never travels.
const SESSION_1 = analyticsSessionKey('omp', '2026-10-01T15-00_uuid');
const OTHER_SESSION = analyticsSessionKey('omp', '2026-10-01T16-00_uuid2');

function row(overrides: Record<string, unknown> = {}) {
  return {
    k: 'omp',
    f: FILE_1,
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
      omp: { state: 'ok', fingerprints: { [FILE_1]: { size: 10, mtimeMs: 20 } } },
      muse: { state: 'not_installed', fingerprints: {} },
      zcode: { state: 'ok', fingerprints: {} },
    },
    rows: [row()],
    ...overrides,
  };
}

function srow(overrides: Record<string, unknown> = {}) {
  return {
    k: 'omp',
    f: FILE_1,
    s: SESSION_1,
    m: 'deepseek-v4.1-flash',
    a: Date.parse('2026-10-01T15:05:00Z'),
    z: Date.parse('2026-10-01T15:35:00Z'),
    i: 100,
    o: 10,
    cr: 50,
    cw: 5,
    c: 0.01,
    n: 2,
    ...overrides,
  };
}

describe('analytics remote transport', () => {
  it('parses bounded aggregate responses', () => {
    const parsed = parseAnalyticsRemoteResponse(JSON.stringify(response()));
    expect(parsed.rows).toHaveLength(1);
    expect(parsed.srows).toEqual([]);
    expect(parsed.kinds.muse.state).toBe('not_installed');
  });

  it('parses session aggregates and rejects malformed ones', () => {
    const parsed = parseAnalyticsRemoteResponse(
      JSON.stringify(response({ srows: [srow(), srow({ s: OTHER_SESSION })] }))
    );
    expect(parsed.srows).toHaveLength(2);
    expect(parsed.srows[0].s).toBe(SESSION_1);
    const bad = (overrides: Record<string, unknown>) => () =>
      parseAnalyticsRemoteResponse(JSON.stringify(response({ srows: [srow(overrides)] })));
    expect(bad({ s: '' })).toThrow();
    // Only the host's truncated digest travels: a raw id, slashed or not, is refused.
    expect(bad({ s: 'has/slash' })).toThrow();
    expect(bad({ s: '2026-10-01T15-00_uuid' })).toThrow();
    expect(bad({ z: Date.parse('2026-10-01T15:00:00Z') })).toThrow();
    expect(bad({ f: 'not-a-hash' })).toThrow();
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

  it('refuses raw paths, control characters and non-hash fingerprints from the helper', () => {
    const bad = (overrides: Record<string, unknown>) => () =>
      parseAnalyticsRemoteResponse(JSON.stringify(response(overrides)));
    expect(bad({ rows: [row({ f: '/Users/someone/.omp/session.jsonl' })] })).toThrow();
    expect(bad({ rows: [row({ m: 'model\u001b[31m' })] })).toThrow();
    expect(bad({ rows: [row({ p: 'provider\n' })] })).toThrow();
    const prints = (key: string, print: Record<string, unknown>) => ({
      kinds: {
        omp: { state: 'ok', fingerprints: { [key]: print } },
        muse: { state: 'not_installed', fingerprints: {} },
        zcode: { state: 'ok', fingerprints: {} },
      },
    });
    expect(bad(prints('/tmp/raw-path', { size: 1, mtimeMs: 2 }))).toThrow();
    expect(bad(prints(FILE_1, { size: 1, mtimeMs: 2, head: 'raw text' }))).toThrow();
    expect(bad(prints(FILE_1, { size: 1, mtimeMs: 2, tail: 'raw text' }))).toThrow();
    expect(() =>
      parseAnalyticsRemoteResponse(
        JSON.stringify(response(prints(FILE_1, { size: 1, mtimeMs: 2, head: FILE_1, tail: DB_1 })))
      )
    ).not.toThrow();
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

  it('refuses malformed extra roots without running ssh', async () => {
    const bad = (extraRoots: unknown) =>
      runAnalyticsRemoteHelper('fine-alias', 'mac', {
        kinds: ['omp'],
        minDateMs: MIN_DATE,
        fingerprints: {},
        extraRoots: extraRoots as Record<'omp', string[]>,
      });
    await expect(bad({ cursor: ['/x'] })).rejects.toThrow('Analytics remote request is invalid.');
    await expect(bad({ omp: ['relative/path'] })).rejects.toThrow(
      'Analytics remote request is invalid.'
    );
    await expect(bad({ omp: ['/x/../y'] })).rejects.toThrow('Analytics remote request is invalid.');
    await expect(bad({ omp: ['/x\0y'] })).rejects.toThrow('Analytics remote request is invalid.');
    await expect(bad({ omp: new Array(17).fill('/x') })).rejects.toThrow(
      'Analytics remote request is invalid.'
    );
  });
});

describe('analytics remote helper integrity', () => {
  it('pins the packaged helper by SHA-256', () => {
    const bytes = fs.readFileSync(analyticsHelperPath());
    expect(createHash('sha256').update(bytes).digest('hex')).toBe(ANALYTICS_HELPER_SHA256);
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
                omp: { state: 'ok', fingerprints: { [FILE_1]: { size: 10, mtimeMs: 20 } } },
              },
              rows: [row({ f: (platform === 'mac' ? 'ac' : 'bc').repeat(32) })],
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
    // Both hosts are asked about every kind; a tool that is not installed there
    // answers not_installed from the host itself, never from a fixed claim here.
    expect(
      states.filter((entry) => entry.host === 'windows').map((entry) => entry.tool)
    ).toEqual(['claude', 'codex', 'muse', 'omp', 'zcode']);
    expect(states.filter((entry) => entry.host === 'mac').map((entry) => entry.tool)).toEqual([
      'claude',
      'codex',
      'muse',
      'omp',
      'zcode',
    ]);
  });

  it('converts session aggregates to worker sessions without doubling hourly tokens', async () => {
    const { results } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      now: () => Date.parse('2026-10-02T00:00:00Z'),
      runHelper: async () =>
        parseAnalyticsRemoteResponse(
          JSON.stringify(response({ srows: [srow(), srow({ s: OTHER_SESSION, i: 50 })] }))
        ),
    });
    const omp = results.find((entry) => entry.tool === 'omp');
    expect(omp?.data.session).toHaveLength(2);
    // The key the host derived is the session's published id; no raw id ever travels.
    expect(omp?.data.session[0].sessionId).toBe(SESSION_1);
    expect(omp?.data.session[0].lastActivity).toBe('2026-10-01T15:35:00.000Z');
    expect(omp?.data.session[0].target).toBe('omp');
    // Hourly rows carry the tokens; sessions carry the keys. The totals equal
    // the hourly rows alone.
    expect(omp?.data.hourly).toHaveLength(1);
    expect(omp?.data.eventCount).toBe(2);
    // A later scan keeps the cached sessions while their files are unchanged.
    const cached = loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache });
    const cachedOmp = cached.results.find((entry) => entry.tool === 'omp');
    expect(cachedOmp?.data.session).toHaveLength(2);
  });

  it('re-reads a cache whose session aggregates predate the keys', async () => {
    const runHelper = async () =>
      parseAnalyticsRemoteResponse(JSON.stringify(response({ srows: [srow()] })));
    await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper });
    // Both hosts answered, so both caches must read as stale: a cache saved before keys existed
    // holds raw session ids this build must never serve again.
    for (const host of ['mac', 'windows']) {
      const file = path.join(cache, 'analytics-remote-v1', `${host}.json`);
      const saved = JSON.parse(fs.readFileSync(file, 'utf8')) as {
        version: number;
        srows: unknown[];
      };
      expect(saved.version).toBe(3);
      expect(saved.srows).toHaveLength(1);
      fs.writeFileSync(file, JSON.stringify({ ...saved, version: 2, srows: [] }));
    }
    expect(loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache }).results).toEqual([]);
  });

  it('refuses a session aggregate whose key is not a truncated digest', () => {
    expect(() =>
      parseAnalyticsRemoteResponse(
        JSON.stringify(response({ srows: [srow({ s: 'raw-session-id' })] }))
      )
    ).toThrow();
  });

  it('sends the saved extra roots for each target host', async () => {
    const ccsHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-remote-extras-'));
    const previous = process.env.CCS_HOME;
    process.env.CCS_HOME = ccsHome;
    try {
      writeDashboardPreferences(
        {
          ...defaultDashboardPreferences(),
          usageLogSources: [
            { id: 'a', tool: 'omp', host: 'mac', path: '/Users/u/extra-omp' },
            { id: 'b', tool: 'omp', host: 'windows', path: 'C:\\extra\\omp' },
            { id: 'c', tool: 'omp', host: 'ubuntu', path: '/home/u/extra' },
            { id: 'd', tool: 'claude-code', host: 'mac', path: '/Users/u/extra-projects' },
            { id: 'e', tool: 'codex', host: 'windows', path: 'C:\\extra\\.codex' },
            {
              id: 'f',
              tool: 'jsonl',
              host: 'mac',
              path: '/Users/u/extra-jsonl',
              fieldMapping: { timestamp: 'ts', model: 'model' },
            },
          ],
        },
        path.join(ccsHome, '.ccs')
      );
      const seen = new Map<string, unknown>();
      const runHelper = async (_alias: string, platform: 'mac' | 'windows', request: unknown) => {
        seen.set(platform, request);
        return parseAnalyticsRemoteResponse(JSON.stringify(response()));
      };
      await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper });
      expect((seen.get('mac') as { extraRoots: unknown }).extraRoots).toEqual({
        omp: ['/Users/u/extra-omp'],
        claude: ['/Users/u/extra-projects'],
      });
      expect((seen.get('windows') as { extraRoots: unknown }).extraRoots).toEqual({
        omp: ['C:\\extra\\omp'],
        codex: ['C:\\extra\\.codex'],
      });
    } finally {
      if (previous === undefined) delete process.env.CCS_HOME;
      else process.env.CCS_HOME = previous;
      fs.rmSync(ccsHome, { recursive: true, force: true });
    }
  });

  it('keeps changed-file rows incremental across scans', async () => {
    const runHelper = async () => parseAnalyticsRemoteResponse(JSON.stringify(response()));
    await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper });
    const changed = response({
      kinds: {
        omp: { state: 'ok', fingerprints: { [FILE_1]: { size: 11, mtimeMs: 21 } } },
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

  it('keeps a complete scan ok when only the custom-root search hit its bounds', async () => {
    const runHelper = async () =>
      parseAnalyticsRemoteResponse(JSON.stringify(response({ discoveryTruncated: true })));
    const { results, states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper,
    });
    expect(results.find((entry) => entry.tool === 'omp')?.data.hourly[0].inputTokens).toBe(100);
    const mac = states.find((entry) => entry.tool === 'omp' && entry.host === 'mac');
    expect(mac?.state).toBe('ok');
    expect(mac?.detail).toContain('custom OMP session folders');
  });

  it('never calls an empty partial scan cached', async () => {
    const truncated = response({
      truncated: true,
      kinds: { omp: { state: 'ok', fingerprints: {} } },
      rows: [],
    });
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(truncated)),
    });
    const mac = states.find((entry) => entry.tool === 'omp' && entry.host === 'mac');
    expect(mac).toMatchObject({ state: 'unavailable', rowCount: 0 });
  });

  it('keeps the rows of a kind the host could not read', async () => {
    const zcodeRow = row({ k: 'zcode', f: DB_1, m: 'GLM-5.3-Flash', c: 0, i: 40 });
    const first = response({
      kinds: {
        omp: { state: 'ok', fingerprints: { [FILE_1]: { size: 10, mtimeMs: 20 } } },
        zcode: { state: 'ok', fingerprints: { [DB_1]: { size: 5, mtimeMs: 6 } } },
      },
      rows: [row(), zcodeRow],
    });
    await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(first)),
    });
    const failed = response({
      kinds: {
        omp: { state: 'ok', fingerprints: { [FILE_1]: { size: 10, mtimeMs: 20 } } },
        zcode: { state: 'error', fingerprints: {} },
      },
      rows: [],
    });
    const { results, states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(failed)),
    });
    expect(results.find((entry) => entry.tool === 'zcode')?.data.hourly[0].inputTokens).toBe(40);
    const zcode = states.find((entry) => entry.tool === 'zcode' && entry.host === 'mac');
    expect(zcode?.state).toBe('cached');
    expect(zcode?.detail).toContain('read failed');
  });

  it('replaces zcode rows when only its write-ahead log changed', async () => {
    const at = (walSize: number, i: number) =>
      response({
        kinds: {
          zcode: {
            state: 'ok',
            fingerprints: { [DB_1]: { size: 5, mtimeMs: 6, walSize, walMtimeMs: walSize } },
          },
        },
        rows: [row({ k: 'zcode', f: DB_1, m: 'GLM-5.3-Flash', c: 0, i })],
      });
    await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(at(0, 40))),
    });
    const { results } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(at(4096, 55))),
    });
    expect(results.find((entry) => entry.tool === 'zcode')?.data.hourly[0].inputTokens).toBe(55);
  });

  it('prices remote rows under their routing provider and keeps the tool apart', async () => {
    const { results } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () =>
        parseAnalyticsRemoteResponse(JSON.stringify(response({ rows: [row({ p: 'anthropic' })] }))),
    });
    expect(results[0].data.hourly[0].modelBreakdowns[0].provider).toBe('anthropic');
  });

  it('serves the saved aggregates without contacting a host', async () => {
    const runHelper = async () => parseAnalyticsRemoteResponse(JSON.stringify(response()));
    await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper });
    const { results, states } = loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache });
    expect(results.find((entry) => entry.tool === 'omp')?.data.hourly[0].inputTokens).toBe(100);
    const mac = states.find((entry) => entry.tool === 'omp' && entry.host === 'mac');
    expect(mac?.state).toBe('cached');
    expect(mac?.detail).toContain('timed out');
  });

  it('says when logs were read but hold no usage in the last 31 days', async () => {
    const stale = response({
      kinds: {
        muse: { state: 'ok', fingerprints: { [FILE_1]: { size: 10, mtimeMs: 20 } } },
      },
      rows: [],
    });
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(stale)),
    });
    expect(states.find((entry) => entry.tool === 'muse' && entry.host === 'mac')).toMatchObject({
      state: 'ok',
      rowCount: 0,
      detail: 'usage logs found but no usage in the last 31 days',
    });
  });

  it('reports each host scan start and settle, on success and on failure', async () => {
    const calls: Array<{ host: string; phase: string }> = [];
    const onHostScan = (host: string, phase: 'start' | 'done') => {
      calls.push({ host, phase });
    };
    const runHelper = async () => parseAnalyticsRemoteResponse(JSON.stringify(response()));
    await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper, onHostScan });
    for (const host of ['mac', 'windows']) {
      expect(calls.filter((call) => call.host === host).map((call) => call.phase)).toEqual([
        'start',
        'done',
      ]);
    }
    calls.length = 0;
    const failing = async (): Promise<never> => {
      throw new Error('timed out');
    };
    await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: failing,
      onHostScan,
    });
    for (const host of ['mac', 'windows']) {
      expect(calls.filter((call) => call.host === host).map((call) => call.phase)).toEqual([
        'start',
        'done',
      ]);
    }
  });
});
