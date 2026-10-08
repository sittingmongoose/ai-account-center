import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  ANALYTICS_HELPER_SHA256,
  REMOTE_ANALYTICS_HOSTS,
  analyticsHelperCommand,
  analyticsHelperPath,
  parseAnalyticsRemoteResponse,
  runAnalyticsRemoteHelper,
  type AnalyticsRemoteHost,
} from '../../../src/web-server/services/analytics-remote-transport';
import {
  analyticsRemoteTargets,
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
} from '../../../src/web-server/services/analytics-remote-sources';
import { analyticsSessionKey } from '../../../src/web-server/usage/analytics-session-key';
import {
  clearModelsDevRegistryCache,
  setCachedModelsDevRegistry,
} from '../../../src/web-server/models-dev/registry-cache';
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
    // A kind the deadline skipped reports pending; a cut kind reports partial.
    const pending = parseAnalyticsRemoteResponse(
      JSON.stringify(
        response({
          truncated: true,
          kinds: {
            omp: { state: 'ok', partial: true, fingerprints: {} },
            muse: { state: 'pending', fingerprints: {} },
          },
        })
      )
    );
    expect(pending.kinds.omp).toMatchObject({ state: 'ok', partial: true });
    expect(pending.kinds.muse.state).toBe('pending');
    expect(pending.kinds.muse.partial).toBeUndefined();
    expect(() =>
      parseAnalyticsRemoteResponse(
        JSON.stringify(response({ kinds: { omp: { state: 'bogus', fingerprints: {} } } }))
      )
    ).toThrow();
    expect(() =>
      parseAnalyticsRemoteResponse(
        JSON.stringify(
          response({ kinds: { omp: { state: 'ok', partial: 'yes', fingerprints: {} } } })
        )
      )
    ).toThrow();
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

  it('lets Python read the request from stdin itself, never PowerShell', () => {
    const encoded = analyticsHelperCommand('windows').match(
      /^powershell\.exe -NoProfile -NonInteractive -EncodedCommand ([A-Za-z0-9+/=]+)$/
    );
    expect(encoded).not.toBeNull();
    const script = Buffer.from(encoded?.[1] ?? '', 'base64').toString('utf16le');
    // Reading a helper-sized request through [Console]::In hung about half the time over
    // Windows OpenSSH; with no pipeline input, Python inherits stdin and reads it.
    expect(script).not.toContain('[Console]::In');
    expect(script).not.toContain('$request |');
    expect(script).toContain("& $python -c 'import sys,json,io;");
    expect(script).toContain('sys.stdin.buffer.read()');
    expect(analyticsHelperCommand('mac')).toStartWith("/usr/bin/python3 -c 'import sys,json,io;");
  });

  it('runs the helper with python3 directly on Nas1, the same command as the Mac', () => {
    const command = analyticsHelperCommand('nas1');
    expect(command).toStartWith("/usr/bin/python3 -c 'import sys,json,io;");
    // Nas1 is a second Ubuntu computer: any POSIX computer takes the python3 form, only Windows
    // goes through PowerShell.
    expect(command).toBe(analyticsHelperCommand('mac'));
    expect(command).not.toContain('powershell');
    expect(analyticsHelperCommand('windows')).toStartWith('powershell.exe ');
  });

  it('scans the Mac, Windows and Nas1 for the same six tools', () => {
    expect([...REMOTE_ANALYTICS_HOSTS]).toEqual(['mac', 'windows', 'nas1']);
    for (const host of REMOTE_ANALYTICS_HOSTS)
      expect(analyticsRemoteTargets(host)).toEqual([
        'claude',
        'codex',
        'omp',
        'muse',
        'zcode',
        'antigravity',
      ]);
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

  it('keeps the helper bytes unchanged by the Nas1 host', () => {
    // Nas1 streams the same packaged helper as the Mac and Windows; adding the host changed no byte
    // of it. Change this value only together with the helper itself.
    expect(ANALYTICS_HELPER_SHA256).toBe(
      'da65d7d1a771d7cd63f82d090a797dce3bc6aa41c2acbdc5ab7317493125e9a6'
    );
  });
});

describe('analytics remote sources', () => {
  const hosts = async () => ({ mac: 'mac-alias', windows: 'win-alias', nas1: 'nas1-alias' });

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
    expect(results.map((entry) => entry.tool)).toEqual(['omp', 'omp', 'omp']);
    expect(results[0].data.hourly[0].modelBreakdowns[0].modelName).toBe('deepseek-v4.1-flash');
    expect(results[0].data.eventCount).toBe(2);
    const ompMac = states.find((entry) => entry.tool === 'omp' && entry.host === 'mac');
    expect(ompMac).toMatchObject({ state: 'ok', rowCount: 2 });
    expect(ompMac?.lastScanAt).toBe('2026-10-02T00:00:00.000Z');
    // Every host is asked about every kind; a tool that is not installed there
    // answers not_installed from the host itself, never from a fixed claim here.
    for (const host of REMOTE_ANALYTICS_HOSTS)
      expect(states.filter((entry) => entry.host === host).map((entry) => entry.tool)).toEqual([
        'antigravity',
        'claude',
        'codex',
        'muse',
        'omp',
        'zcode',
      ]);
  });

  it('scans Nas1 over its own alias and keeps its aggregates in its own cache file', async () => {
    const aliases = new Map<AnalyticsRemoteHost, string>();
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      now: () => Date.parse('2026-10-02T00:00:00Z'),
      runHelper: async (alias, platform) => {
        aliases.set(platform, alias);
        return parseAnalyticsRemoteResponse(JSON.stringify(response()));
      },
    });
    expect(Object.fromEntries(aliases)).toEqual({
      mac: 'mac-alias',
      windows: 'win-alias',
      nas1: 'nas1-alias',
    });
    for (const host of REMOTE_ANALYTICS_HOSTS)
      expect(fs.existsSync(path.join(cache, 'analytics-remote-v1', `${host}.json`))).toBe(true);
    expect(states.find((entry) => entry.tool === 'omp' && entry.host === 'nas1')).toMatchObject({
      state: 'ok',
      rowCount: 2,
      lastScanAt: '2026-10-02T00:00:00.000Z',
    });
  });

  it('keeps a failing Nas1 from touching the other hosts, and the other way round', async () => {
    const onlyNas1Fails = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async (_alias, platform) => {
        if (platform === 'nas1') throw new Error('no route');
        return parseAnalyticsRemoteResponse(JSON.stringify(response()));
      },
    });
    const state = (host: AnalyticsRemoteHost) =>
      onlyNas1Fails.states.find((entry) => entry.tool === 'omp' && entry.host === host)?.state;
    expect(state('mac')).toBe('ok');
    expect(state('windows')).toBe('ok');
    expect(state('nas1')).toBe('unavailable');
    expect(onlyNas1Fails.results.filter((entry) => entry.tool === 'omp')).toHaveLength(2);
    // The other way round: only Nas1 answers, and the Mac and Windows fall back to what they read
    // before, marked cached.
    const failsEverywhereButNas1 = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async (_alias, platform) => {
        if (platform !== 'nas1') throw new Error('no route');
        return parseAnalyticsRemoteResponse(JSON.stringify(response()));
      },
    });
    const next = (host: AnalyticsRemoteHost) =>
      failsEverywhereButNas1.states.find((entry) => entry.tool === 'omp' && entry.host === host)
        ?.state;
    expect(next('nas1')).toBe('ok');
    expect(next('mac')).toBe('cached');
    expect(next('windows')).toBe('cached');
  });

  it('scans only Nas1, over its fixed alias, when the launcher aliases cannot be resolved', async () => {
    const calls: string[] = [];
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts: async () => {
        throw new Error('launcher list unreadable');
      },
      cacheDir: cache,
      runHelper: async (alias, platform) => {
        calls.push(`${platform}:${alias}`);
        return parseAnalyticsRemoteResponse(JSON.stringify(response()));
      },
    });
    expect([...new Set(calls)]).toEqual(['nas1:nas1-agent']);
    const unconfigured = states.filter((entry) => entry.host !== 'nas1');
    expect(unconfigured).toHaveLength(12);
    expect(unconfigured.every((entry) => entry.detail === 'remote host is not configured')).toBe(
      true
    );
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
    // Every host answered, so every cache must read as stale: a cache saved before keys existed
    // holds raw session ids this build must never serve again.
    for (const host of REMOTE_ANALYTICS_HOSTS) {
      const file = path.join(cache, 'analytics-remote-v1', `${host}.json`);
      const saved = JSON.parse(fs.readFileSync(file, 'utf8')) as {
        version: number;
        srows: unknown[];
      };
      expect(saved.version).toBe(4);
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
            { id: 'nas1-omp', tool: 'omp', host: 'nas1', path: '/data/nas1/extra-omp' },
            {
              id: 'nas1-claude',
              tool: 'claude-code',
              host: 'nas1',
              path: '/data/nas1/extra-projects',
            },
            { id: 'nas1-zcode', tool: 'zcode', host: 'nas1', path: '/data/nas1/zcode/db.sqlite' },
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
      const runHelper = async (_alias: string, platform: AnalyticsRemoteHost, request: unknown) => {
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
      // Nas1's own roots travel to Nas1 alone, as POSIX paths; no other host's roots reach it.
      expect((seen.get('nas1') as { extraRoots: unknown }).extraRoots).toEqual({
        omp: ['/data/nas1/extra-omp'],
        claude: ['/data/nas1/extra-projects'],
        zcode: ['/data/nas1/zcode/db.sqlite'],
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
      kinds: { omp: { state: 'pending', fingerprints: {} } },
      rows: [],
    });
    const { results, states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(truncated)),
    });
    const omp = results.find((entry) => entry.tool === 'omp');
    expect(omp?.data.hourly[0].inputTokens).toBe(100);
    // A tool the scan did not reach is still being scanned, never a failure,
    // and the records from earlier scans stay in the totals.
    const mac = states.find((entry) => entry.tool === 'omp' && entry.host === 'mac');
    expect(mac?.state).toBe('scanning');
    expect(mac?.rowCount).toBe(2);
    expect(mac?.detail).toContain('stay included');
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
      hosts: async () => ({ mac: null, windows: null, nas1: null }),
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

  it('never calls an unreached tool unavailable or cached', async () => {
    const truncated = response({
      truncated: true,
      kinds: { omp: { state: 'pending', fingerprints: {} } },
      rows: [],
    });
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(truncated)),
    });
    const mac = states.find((entry) => entry.tool === 'omp' && entry.host === 'mac');
    expect(mac).toMatchObject({ state: 'scanning', rowCount: 0 });
    expect(mac?.detail).toContain('the next scan continues');
  });

  it('marks a kind its own scan cut short: cached with rows, scanning without', async () => {
    const cut = response({
      truncated: true,
      kinds: {
        omp: { state: 'ok', partial: true, fingerprints: { [FILE_1]: { size: 11, mtimeMs: 21 } } },
        zcode: { state: 'ok', partial: true, fingerprints: {} },
      },
      rows: [row({ i: 40 })],
    });
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(cut)),
    });
    const omp = states.find((entry) => entry.tool === 'omp' && entry.host === 'mac');
    expect(omp?.state).toBe('cached');
    expect(omp?.detail).toContain('partway through this tool');
    // The same cut with nothing read yet is still scanning, not a failure.
    const zcode = states.find((entry) => entry.tool === 'zcode' && entry.host === 'mac');
    expect(zcode?.state).toBe('scanning');
    expect(zcode?.detail).toContain('the next scan continues');
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
    // Nas1's saved aggregates come back like the other hosts', from its own cache file.
    const nas1 = states.find((entry) => entry.tool === 'omp' && entry.host === 'nas1');
    expect(nas1).toMatchObject({ state: 'cached', rowCount: 2 });
    expect(nas1?.detail).toContain('timed out');
    expect(states.filter((entry) => entry.host === 'nas1')).toHaveLength(6);
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
      state: 'no_usage',
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
    for (const host of ['mac', 'windows', 'nas1']) {
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
    for (const host of ['mac', 'windows', 'nas1']) {
      expect(calls.filter((call) => call.host === host).map((call) => call.phase)).toEqual([
        'start',
        'done',
      ]);
    }
  });
});

describe('analytics remote scans per kind', () => {
  const hosts = async () => ({ mac: 'mac-alias', windows: 'win-alias', nas1: 'nas1-alias' });
  const ALL = ['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity'];
  const FILE_2 = 'a2'.repeat(32);
  /** A clean answer for exactly the kinds asked, with an OMP row when OMP is asked. */
  const answer = (kinds: string[], extra: Record<string, unknown> = {}) =>
    parseAnalyticsRemoteResponse(
      JSON.stringify({
        version: 1,
        truncated: false,
        kinds: Object.fromEntries(
          kinds.map((kind) => [
            kind,
            {
              state: 'ok',
              fingerprints: kind === 'omp' ? { [FILE_1]: { size: 10, mtimeMs: 20 } } : {},
            },
          ])
        ),
        rows: kinds.includes('omp') ? [row()] : [],
        ...extra,
      })
    );

  it('scans a cold host with one call per kind, all at once, so a slow kind holds up none', async () => {
    const calls: Array<{ host: string; kinds: string[]; prints: string[] }> = [];
    let releaseClaude: () => void = () => {};
    const slowClaude = new Promise<void>((resolve) => {
      releaseClaude = resolve;
    });
    const loading = loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async (_alias, platform, request) => {
        calls.push({
          host: platform,
          kinds: request.kinds,
          prints: Object.keys(request.fingerprints),
        });
        if (request.kinds.includes('claude')) await slowClaude;
        return answer(request.kinds);
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    // Every kind on every host is in flight together while Claude Code is still reading.
    for (const host of ['mac', 'windows', 'nas1'])
      expect(
        calls
          .filter((call) => call.host === host)
          .map((call) => call.kinds.join(','))
          .sort()
      ).toEqual([...ALL].sort());
    releaseClaude();
    const { results, states } = await loading;
    expect(results.filter((entry) => entry.tool === 'omp')).toHaveLength(3);
    expect(states.filter((entry) => entry.state === 'ok')).toHaveLength(3);
    // A settled host's next scan is one call for every kind.
    calls.length = 0;
    await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async (_alias, platform, request) => {
        calls.push({
          host: platform,
          kinds: request.kinds,
          prints: Object.keys(request.fingerprints),
        });
        return answer(request.kinds);
      },
    });
    expect(calls.map((call) => call.host).sort()).toEqual(['mac', 'nas1', 'windows']);
    for (const call of calls) {
      expect(call.kinds).toEqual(ALL);
      expect([...call.prints].sort()).toEqual([...ALL].sort());
    }
  });

  it('goes back to one call per kind after a scan a kind did not finish', async () => {
    const seen: string[][] = [];
    const prints: string[][] = [];
    const runHelper = async (
      _alias: string,
      platform: AnalyticsRemoteHost,
      request: { kinds: string[]; fingerprints: Record<string, unknown> }
    ) => {
      if (platform === 'mac') {
        seen.push(request.kinds);
        prints.push(Object.keys(request.fingerprints));
      }
      return request.kinds.includes('codex')
        ? answer(request.kinds, {
            truncated: true,
            kinds: { codex: { state: 'ok', partial: true, fingerprints: {} } },
          })
        : answer(request.kinds);
    };
    await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper });
    seen.length = 0;
    prints.length = 0;
    await loadAnalyticsRemoteSources(MIN_DATE, { hosts, cacheDir: cache, runHelper });
    expect(seen).toHaveLength(6);
    // Each call carries only its own kind's fingerprints.
    seen.forEach((kinds, index) => expect(prints[index]).toEqual(kinds));
  });

  it('keeps a failed kind on its earlier rows while the other kinds update', async () => {
    // Scan 1 leaves Codex unfinished, so the host is still catching up and scan 2 fans out.
    await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async (_alias, _platform, request) =>
        request.kinds.includes('codex')
          ? answer(request.kinds, {
              kinds: { codex: { state: 'pending', fingerprints: {} } },
            })
          : answer(request.kinds),
    });
    const fresh = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async (_alias, _platform, request) => {
        if (request.kinds.includes('omp')) throw new Error('ssh dropped');
        return answer(request.kinds);
      },
    });
    const omp = fresh.states.find((entry) => entry.tool === 'omp' && entry.host === 'mac');
    expect(omp).toMatchObject({ state: 'cached', rowCount: 2 });
    expect(omp?.detail).toBe('remote scan failed; showing previously read aggregates');
    expect(fresh.results.filter((entry) => entry.tool === 'omp')).toHaveLength(3);
    expect(
      fresh.states.find((entry) => entry.tool === 'claude' && entry.host === 'mac')?.state
    ).toBe('no_usage');
  });

  it('keeps unvisited files only for the kind whose own scan was cut', async () => {
    const prints = (kind: string) =>
      kind === 'omp'
        ? { [FILE_1]: { size: 10, mtimeMs: 20 } }
        : kind === 'claude'
          ? { [FILE_2]: { size: 10, mtimeMs: 20 } }
          : {};
    // Scan 1: a Claude Code file and an OMP file; zcode is left pending, so scan 2 fans out.
    await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async (_alias, _platform, request) => {
        const kind = request.kinds[0];
        return parseAnalyticsRemoteResponse(
          JSON.stringify({
            version: 1,
            truncated: kind === 'zcode',
            kinds: {
              [kind]: { state: kind === 'zcode' ? 'pending' : 'ok', fingerprints: prints(kind) },
            },
            rows:
              kind === 'omp'
                ? [row()]
                : kind === 'claude'
                  ? [row({ k: 'claude', f: FILE_2, m: 'claude-sonnet-4-6' })]
                  : [],
          })
        );
      },
    });
    // Scan 2: Claude Code's own call is cut before it reaches its file; OMP's call finishes and
    // its file is gone. Only Claude Code keeps its unvisited file.
    const { results } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async (_alias, _platform, request) => {
        expect(request.kinds).toHaveLength(1);
        const kind = request.kinds[0];
        return parseAnalyticsRemoteResponse(
          JSON.stringify({
            version: 1,
            truncated: kind === 'claude',
            kinds: { [kind]: { state: 'ok', fingerprints: {} } },
            rows: [],
          })
        );
      },
    });
    expect(results.filter((entry) => entry.tool === 'omp')).toHaveLength(0);
    expect(results.filter((entry) => entry.tool === 'claude')).toHaveLength(3);
  });
});

describe('remote rows price like the local rows of their tool', () => {
  const hosts = async () => ({ mac: 'mac-alias', windows: null, nas1: null });
  let tempRoot = '';
  let originalCcsHome: string | undefined;
  let originalCcsDir: string | undefined;
  beforeEach(() => {
    tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-remote-pricing-'));
    originalCcsHome = process.env.CCS_HOME;
    originalCcsDir = process.env.CCS_DIR;
    process.env.CCS_HOME = tempRoot;
    delete process.env.CCS_DIR;
    clearModelsDevRegistryCache();
    // Two providers list the Codex model at different prices, so by name alone it is ambiguous.
    setCachedModelsDevRegistry({
      openai: {
        id: 'openai',
        name: 'OpenAI',
        models: {
          'gpt-test-sol': {
            id: 'gpt-test-sol',
            name: 'GPT test',
            cost: { input: 2, output: 10, cache_read: 0.2 },
          },
        },
      },
      azure: {
        id: 'azure',
        name: 'Azure',
        models: {
          'gpt-test-sol': {
            id: 'gpt-test-sol',
            name: 'GPT test',
            cost: { input: 2.5, output: 12, cache_read: 0.25 },
          },
        },
      },
    } as unknown as Parameters<typeof setCachedModelsDevRegistry>[0]);
  });
  afterEach(() => {
    clearModelsDevRegistryCache();
    if (originalCcsHome === undefined) delete process.env.CCS_HOME;
    else process.env.CCS_HOME = originalCcsHome;
    if (originalCcsDir === undefined) delete process.env.CCS_DIR;
    else process.env.CCS_DIR = originalCcsDir;
    fs.rmSync(tempRoot, { recursive: true, force: true });
  });

  it('prices a Codex row without a route at OpenAI list rates, and leaves a model with no rate not logged', async () => {
    const codexRow = (model: string) =>
      row({ k: 'codex', m: model, i: 1_000_000, o: 100_000, cr: 2_000_000, cw: 0, c: 0, n: 3 });
    const { results } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async (_alias, _platform, request) =>
        parseAnalyticsRemoteResponse(
          JSON.stringify({
            version: 1,
            truncated: false,
            kinds: Object.fromEntries(
              request.kinds.map((k) => [k, { state: 'ok', fingerprints: {} }])
            ),
            rows: request.kinds.includes('codex')
              ? [codexRow('gpt-test-sol'), codexRow('gpt-unlisted-model')]
              : request.kinds.includes('muse')
                ? [row({ k: 'muse', m: 'gpt-test-sol', c: 0 })]
                : [],
            srows: request.kinds.includes('codex')
              ? [
                  srow({
                    k: 'codex',
                    m: 'gpt-test-sol',
                    i: 1_000_000,
                    o: 100_000,
                    cr: 2_000_000,
                    cw: 0,
                    c: 0,
                  }),
                ]
              : [],
          })
        ),
    });
    const codex = results.find((entry) => entry.tool === 'codex')?.data;
    const breakdowns = codex?.hourly[0].modelBreakdowns ?? [];
    const priced = breakdowns.find((item) => item.modelName === 'gpt-test-sol');
    // input, cached input and output priced separately at the model's own rates: 2 + 0.4 + 1.
    expect(priced?.provider).toBe('openai');
    expect(priced?.cost).toBeCloseTo(3.4, 9);
    expect(priced?.fallbackCost).toBeUndefined();
    // No listed rate anywhere: an estimate is never invented; that part reads as not logged.
    const unlisted = breakdowns.find((item) => item.modelName === 'gpt-unlisted-model');
    expect(unlisted?.fallbackCost).toBeGreaterThan(0);
    expect(unlisted?.fallbackCost).toBeCloseTo(unlisted?.cost ?? -1, 9);
    expect(codex?.session[0].cost).toBeCloseTo(3.4, 9);
    expect(codex?.session[0].fallbackCost).toBeUndefined();
    // Muse logs no route; like the local Muse reader it prices by model name alone.
    const muse = results.find((entry) => entry.tool === 'muse')?.data.hourly[0].modelBreakdowns[0];
    expect(muse?.provider).toBeUndefined();
  });
});
