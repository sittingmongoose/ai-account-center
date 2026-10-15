import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as childProcess from 'child_process';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  ANALYTICS_HELPER_SHA256,
  AnalyticsHelperIntegrityError,
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
import { withLfLineEndings } from '../../../src/utils/packaged-source';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';
import { localClaudeLogNames } from '../../../src/web-server/services/analytics-claude-log-names';

const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');
let cache: string;
let previousConfig: Record<string, string | undefined>;
beforeEach(() => {
  previousConfig = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
  cache = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-remote-sources-'));
  process.env.CCS_HOME = cache;
  delete process.env.CCS_DIR;
});
afterEach(() => {
  for (const [key, value] of Object.entries(previousConfig)) {
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  fs.rmSync(cache, { recursive: true, force: true });
});

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
    const bytes = withLfLineEndings(fs.readFileSync(analyticsHelperPath()));
    expect(createHash('sha256').update(bytes).digest('hex')).toBe(ANALYTICS_HELPER_SHA256);
  });

  /** Records ssh starts; the helper's answer is an empty scan. */
  function captureSsh(): Array<{ args: string[]; input: string }> {
    const calls: Array<{ args: string[]; input: string }> = [];
    spyOn(childProcess, 'execFile').mockImplementation(((
      _file: string,
      args: string[],
      _options: unknown,
      callback: (error: Error | null, stdout: string, stderr: string) => void
    ) => {
      const call = { args: [...args], input: '' };
      calls.push(call);
      return {
        stdin: {
          on: () => undefined,
          end: (data?: Buffer) => {
            call.input = data ? data.toString('utf8') : '';
            setImmediate(() =>
              callback(
                null,
                JSON.stringify({ version: 1, truncated: false, kinds: {}, rows: [] }),
                ''
              )
            );
          },
        },
      };
    }) as never);
    return calls;
  }

  it('runs a helper packaged with CRLF line endings as its pinned LF text', async () => {
    // A package built from a Windows checkout carried CRLF helpers, failed this pin and stopped
    // every remote scan (2026-10-13).
    const lf = withLfLineEndings(fs.readFileSync(analyticsHelperPath())).toString('latin1');
    const copy = path.join(cache, 'analytics_usage_remote.py');
    fs.writeFileSync(copy, Buffer.from(lf.replace(/\n/g, '\r\n'), 'latin1'));
    const calls = captureSsh();
    try {
      await runAnalyticsRemoteHelper(
        'mac-alias',
        'mac',
        { kinds: ['omp'], minDateMs: MIN_DATE, fingerprints: {} },
        { helperPath: copy }
      );
      expect(calls).toHaveLength(1);
      const sent = (JSON.parse(calls[0].input) as { helperSource: string }).helperSource;
      expect(sent).not.toContain('\r');
      expect(createHash('sha256').update(Buffer.from(sent, 'utf8')).digest('hex')).toBe(
        ANALYTICS_HELPER_SHA256
      );
    } finally {
      mock.restore();
    }
  });

  it('refuses a changed or missing helper before any ssh session', async () => {
    const calls = captureSsh();
    const changed = path.join(cache, 'changed.py');
    fs.writeFileSync(
      changed,
      Buffer.concat([fs.readFileSync(analyticsHelperPath()), Buffer.from('\n# changed\n')])
    );
    // A lone carriage return is a change, not a line ending.
    const strayCr = path.join(cache, 'stray-cr.py');
    fs.writeFileSync(
      strayCr,
      Buffer.concat([fs.readFileSync(analyticsHelperPath()), Buffer.from('\r')])
    );
    try {
      for (const helperPath of [changed, strayCr, path.join(cache, 'missing.py')])
        await expect(
          runAnalyticsRemoteHelper(
            'mac-alias',
            'mac',
            { kinds: ['omp'], minDateMs: MIN_DATE, fingerprints: {} },
            { helperPath }
          )
        ).rejects.toBeInstanceOf(AnalyticsHelperIntegrityError);
      expect(calls).toHaveLength(0);
    } finally {
      mock.restore();
    }
  });

  it('keeps the helper bytes unchanged by the Nas1 host', () => {
    // Nas1 streams the same packaged helper as the Mac and Windows; adding the host changed no byte
    // of it. Change this value only together with the helper itself.
    expect(ANALYTICS_HELPER_SHA256).toBe(
      '8441469245b3392a025c1f50d7d4914a43345aa3d3d0f14ab67aedc9427349d0'
    );
  });
});

describe('analytics remote sources', () => {
  it.each([350, 150, 200])(
    'keeps both Claude aggregates at %i when an incomplete group emits no replacement',
    async (total) => {
      const other = 'b2'.repeat(32);
      const print = { size: 10, mtimeMs: 20, claudeGroup: FILE_1 };
      let pass = 0;
      const deps = {
        cacheDir: cache,
        hosts: async () => ({ mac: 'fixture', windows: null, nas1: null }),
        claudeLogNames: () => ({ originals: new Set<string>(), complete: true }),
        runHelper: async () => {
          const broken = pass === 1 || pass === 2;
          const owner = pass === 3 ? other : FILE_1;
          return parseAnalyticsRemoteResponse(
            JSON.stringify(
              response({
                truncated: broken,
                kinds: {
                  claude: {
                    state: 'ok',
                    fingerprints: broken ? {} : { [FILE_1]: print, [other]: print },
                    ...(broken
                      ? { partial: true, readFailures: 1 }
                      : { replacedFiles: [FILE_1, other] }),
                  },
                },
                rows: broken ? [] : [row({ k: 'claude', f: owner, i: total })],
                srows: broken ? [] : [srow({ k: 'claude', f: owner, i: total })],
              })
            )
          );
        },
      };
      for (pass = 0; pass < 4; pass++) {
        const answer = await loadAnalyticsRemoteSources(MIN_DATE, deps);
        const data = answer.results.find((entry) => entry.tool === 'claude')?.data;
        expect(data?.hourly.reduce((sum, row) => sum + row.inputTokens, 0)).toBe(total);
        expect(data?.session.reduce((sum, row) => sum + row.inputTokens, 0)).toBe(total);
        const saved = JSON.parse(
          fs.readFileSync(path.join(cache, 'analytics-remote-v1/mac.json'), 'utf8')
        );
        expect(saved.rows).toHaveLength(1);
        expect(saved.srows).toHaveLength(1);
        expect(Object.keys(saved.fingerprints.claude)).toHaveLength(2);
      }
    }
  );

  it.each([3, 4, 5])(
    'migrates v%i non-Claude history and dates while offline, then rebuilds Claude',
    async (version) => {
      const tools = ['codex', 'omp', 'muse', 'zcode', 'antigravity'] as const;
      const rows = tools.map((k) => row({ k, i: 100 }));
      const srows = tools.map((k) => srow({ k, i: 100 }));
      const fingerprints = Object.fromEntries(
        [...tools, 'claude'].map((k) => [
          k,
          {
            [FILE_1]: { size: 10, mtimeMs: 20, ...(k === 'claude' ? { claudeGroup: FILE_1 } : {}) },
          },
        ])
      );
      const kinds = Object.fromEntries(
        [...tools, 'claude'].map((k, index) => [
          k,
          { state: 'ok', lastScanAt: `2026-10-02T16:0${index}:00.000Z` },
        ])
      );
      const file = path.join(cache, 'analytics-remote-v1/mac.json');
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(
        file,
        JSON.stringify({
          version,
          fingerprints,
          rows: [...rows, row({ k: 'claude', i: 999 })],
          srows: [...srows, srow({ k: 'claude', i: 999 })],
          kinds,
          lastScanAt: '2026-10-02T16:06:00.000Z',
          settled: true,
        })
      );
      const deps = {
        cacheDir: cache,
        hosts: async () => ({ mac: 'fixture', windows: null, nas1: null }),
        claudeLogNames: () => ({ originals: new Set<string>(), complete: true }),
      };
      const offline = await loadAnalyticsRemoteSources(MIN_DATE, {
        ...deps,
        runHelper: async () => {
          throw new Error('fixture host offline');
        },
      });
      for (const answer of [loadAnalyticsRemoteCachedSources(MIN_DATE, deps), offline]) {
        expect(answer.results.map((entry) => entry.tool).sort()).toEqual([...tools].sort());
        for (const tool of tools) {
          const data = answer.results.find((entry) => entry.tool === tool)?.data;
          expect(data?.hourly[0].inputTokens).toBe(100);
          expect(data?.session[0].inputTokens).toBe(100);
          expect(
            answer.states.find((state) => state.host === 'mac' && state.tool === tool)
          ).toMatchObject({ state: 'cached', lastScanAt: kinds[tool].lastScanAt });
        }
        expect(
          answer.states.find((state) => state.host === 'mac' && state.tool === 'claude')?.lastScanAt
        ).toBeNull();
      }
      const requested = new Set<string>();
      const rebuilt = await loadAnalyticsRemoteSources(MIN_DATE, {
        ...deps,
        runHelper: async (_alias, _host, request) => {
          expect(request.kinds).toHaveLength(1);
          const kind = request.kinds[0];
          requested.add(kind);
          expect(request.fingerprints[kind] ?? {}).toEqual(
            kind === 'claude' ? {} : fingerprints[kind]
          );
          return parseAnalyticsRemoteResponse(
            JSON.stringify(
              response({
                kinds: {
                  [kind]: {
                    state: 'ok',
                    fingerprints: fingerprints[kind],
                    ...(kind === 'claude' ? { replacedFiles: [FILE_1] } : {}),
                  },
                },
                rows: kind === 'claude' ? [row({ k: 'claude', i: 150 })] : [],
                srows: kind === 'claude' ? [srow({ k: 'claude', i: 150 })] : [],
              })
            )
          );
        },
      });
      expect(requested.size).toBe(6);
      expect(
        rebuilt.results.find((entry) => entry.tool === 'claude')?.data.hourly[0].inputTokens
      ).toBe(150);
      expect(
        rebuilt.results.find((entry) => entry.tool === 'claude')?.data.session[0].inputTokens
      ).toBe(150);
      const saved = JSON.parse(fs.readFileSync(file, 'utf8'));
      expect(saved.version).toBe(6);
      expect(saved.rows.filter((row: { k: string }) => row.k !== 'claude')).toEqual(rows);
      expect(saved.srows.filter((row: { k: string }) => row.k !== 'claude')).toEqual(srows);
    }
  );

  it('replaces every Claude copy owner atomically on repeated partial reads and recovery', async () => {
    const other = 'b2'.repeat(32);
    const print = { size: 10, mtimeMs: 20, claudeGroup: FILE_1 };
    let pass = 0;
    const deps = {
      cacheDir: cache,
      hosts: async () => ({ mac: 'mac-alias', windows: null, nas1: null }),
      runHelper: async () => {
        const owner = pass === 0 || pass === 3 ? FILE_1 : other;
        return parseAnalyticsRemoteResponse(
          JSON.stringify(
            response({
              truncated: pass === 1 || pass === 2,
              kinds: {
                claude: {
                  state: 'ok',
                  ...(pass === 1 || pass === 2 ? { partial: true, readFailures: 1 } : {}),
                  fingerprints:
                    pass === 1 || pass === 2
                      ? { [other]: print }
                      : { [FILE_1]: print, [other]: print },
                  replacedFiles: [FILE_1, other],
                },
              },
              rows: [row({ k: 'claude', f: owner, i: 200 })],
              srows: [srow({ k: 'claude', f: owner, i: 200 })],
            })
          )
        );
      },
    };
    for (pass = 0; pass < 4; pass++) {
      const answer = await loadAnalyticsRemoteSources(MIN_DATE, deps);
      const data = answer.results.find((entry) => entry.tool === 'claude')?.data;
      expect(data?.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(200);
      expect(data?.session.reduce((sum, session) => sum + session.inputTokens, 0)).toBe(200);
      const saved = JSON.parse(
        fs.readFileSync(path.join(cache, 'analytics-remote-v1/mac.json'), 'utf8')
      );
      expect(saved.rows).toHaveLength(1);
      expect(saved.srows).toHaveLength(1);
    }
    // Version 5 could contain accumulated Claude owners; its rows/prints must rebuild.
    const file = path.join(cache, 'analytics-remote-v1/mac.json');
    const saved = JSON.parse(fs.readFileSync(file, 'utf8'));
    expect(saved.version).toBe(6);
    fs.writeFileSync(
      file,
      JSON.stringify({ ...saved, version: 5, rows: [...saved.rows, ...saved.rows] })
    );
    expect(loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache }).results).toEqual([]);
    pass = 0;
    let sawEmptyPrints = false;
    await loadAnalyticsRemoteSources(MIN_DATE, {
      ...deps,
      runHelper: async (...args) => {
        if (args[2].kinds.includes('claude'))
          sawEmptyPrints = Object.keys(args[2].fingerprints.claude ?? {}).length === 0;
        return deps.runHelper();
      },
    });
    expect(sawEmptyPrints).toBe(true);
  });

  it('keeps the last confirmed per-kind date through repeated failed reads and cache fallback', async () => {
    let pass = 0;
    const started = Date.parse('2026-10-02T16:01:00Z');
    const deps = {
      cacheDir: cache,
      hosts: async () => ({ mac: 'mac-alias', windows: null, nas1: null }),
      now: () => started + pass * 60_000,
      runHelper: async () =>
        parseAnalyticsRemoteResponse(
          JSON.stringify(
            response({
              truncated: pass > 0,
              kinds: {
                claude: {
                  state: 'ok',
                  ...(pass > 0 ? { partial: true, readFailures: 2 } : {}),
                  fingerprints: pass > 0 ? {} : { [FILE_1]: { size: 10, mtimeMs: 20 } },
                },
              },
              rows: pass > 0 ? [] : [row({ k: 'claude', i: 200 })],
            })
          )
        ),
    };
    for (pass = 0; pass < 3; pass++) {
      const answer = await loadAnalyticsRemoteSources(MIN_DATE, deps);
      const state = answer.states.find((entry) => entry.host === 'mac' && entry.tool === 'claude');
      expect(state?.lastScanAt).toBe('2026-10-02T16:01:00.000Z');
      expect(
        answer.results.find((entry) => entry.tool === 'claude')?.data.hourly[0].inputTokens
      ).toBe(200);
      if (pass > 0) {
        expect(state?.state).toBe('cached');
        expect(state?.detail).toBe(
          '2 logs could not be read on this computer; showing previously read usage'
        );
      }
    }
    expect(
      loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache }).states.find(
        (entry) => entry.host === 'mac' && entry.tool === 'claude'
      )?.lastScanAt
    ).toBe('2026-10-02T16:01:00.000Z');
  });

  it('validates copy-group replacement keys and read failure counts strictly', () => {
    const parse = (fields: Record<string, unknown>, tool = 'claude') =>
      parseAnalyticsRemoteResponse(
        JSON.stringify(
          response({
            kinds: { [tool]: { state: 'ok', partial: true, fingerprints: {}, ...fields } },
            rows: [],
          })
        )
      );
    expect(parse({ replacedFiles: [FILE_1], readFailures: 2 }).kinds.claude.readFailures).toBe(2);
    for (const value of [null, '1', 0, -1, 1.5, 20_001])
      expect(() => parse({ readFailures: value })).toThrow();
    expect(() => parse({ readFailures: 1, partial: false })).toThrow();
    for (const value of [
      null,
      FILE_1,
      ['raw-path'],
      [1],
      [FILE_1, FILE_1],
      Array(40_001).fill(FILE_1),
    ])
      expect(() => parse({ replacedFiles: value })).toThrow();
    expect(() => parse({ replacedFiles: [FILE_1] }, 'codex')).toThrow();
    expect(() => parse({ replacedFiles: [FILE_1], state: 'pending' })).toThrow();
    for (const value of [null, 1, 'raw-path'])
      expect(() =>
        parse({
          fingerprints: {
            [FILE_1]: { size: 10, mtimeMs: 20, claudeGroup: value },
          },
        })
      ).toThrow();
  });

  it('keeps per-kind dates and measured empty states through pending, errors and cache fallback', async () => {
    let now = Date.parse('2026-10-02T00:00:00Z');
    let pass = 0;
    const deps = {
      cacheDir: cache,
      now: () => now,
      hosts: async () => ({ mac: 'fixture', windows: '', nas1: '' }),
      runHelper: async () =>
        parseAnalyticsRemoteResponse(
          JSON.stringify(
            response({
              kinds: {
                omp: {
                  state: pass ? 'pending' : 'ok',
                  fingerprints: pass ? {} : { [FILE_1]: { size: 10, mtimeMs: 20 } },
                },
                muse: { state: pass ? 'error' : 'ok', fingerprints: {} },
                zcode: { state: 'ok', fingerprints: {} },
              },
              rows: pass ? [] : [row()],
            })
          )
        ),
    };
    await loadAnalyticsRemoteSources(MIN_DATE, deps);
    const first = new Date(now).toISOString();
    pass++;
    now += 3 * 3_600_000;
    const next = await loadAnalyticsRemoteSources(MIN_DATE, deps);
    expect(next.states.find((entry) => entry.host === 'mac' && entry.tool === 'omp')).toMatchObject(
      { state: 'scanning', lastScanAt: first, rowCount: 2 }
    );
    expect(
      next.states.find((entry) => entry.host === 'mac' && entry.tool === 'muse')
    ).toMatchObject({ state: 'unavailable', lastScanAt: first });
    const cached = loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache });
    expect(
      cached.states.find((entry) => entry.host === 'mac' && entry.tool === 'omp')
    ).toMatchObject({ state: 'cached', lastScanAt: first });
    expect(
      cached.states.find((entry) => entry.host === 'mac' && entry.tool === 'muse')
    ).toMatchObject({ state: 'no_usage', lastScanAt: first });
    expect(
      cached.states.find((entry) => entry.host === 'mac' && entry.tool === 'zcode')
    ).toMatchObject({ state: 'no_usage', lastScanAt: new Date(now).toISOString() });
    expect(
      cached.states.find((entry) => entry.host === 'mac' && entry.tool === 'claude')
    ).toMatchObject({ state: 'unavailable', lastScanAt: null });
  });

  it('treats an omitted requested kind as a failed read and retains its earlier rows and time', async () => {
    let pass = 0;
    const at = Date.parse('2026-10-02T00:00:00Z');
    const deps = {
      cacheDir: cache,
      now: () => at + pass * 3_600_000,
      hosts: async () => ({ mac: 'fixture', windows: '', nas1: '' }),
      runHelper: async () =>
        parseAnalyticsRemoteResponse(JSON.stringify(response(pass ? { kinds: {}, rows: [] } : {}))),
    };
    await loadAnalyticsRemoteSources(MIN_DATE, deps);
    pass++;
    const next = await loadAnalyticsRemoteSources(MIN_DATE, deps);
    expect(next.results.find((entry) => entry.tool === 'omp')?.data.eventCount).toBe(2);
    expect(next.states.find((entry) => entry.host === 'mac' && entry.tool === 'omp')).toMatchObject(
      { state: 'cached', lastScanAt: new Date(at).toISOString() }
    );
  });

  it('never saves fresh fingerprints for zcode rows discarded after a database error', async () => {
    let pass = 0;
    let retried = false;
    const deps = {
      cacheDir: cache,
      hosts: async () => ({ mac: 'fixture', windows: '', nas1: '' }),
      runHelper: async (
        _alias: string,
        _host: AnalyticsRemoteHost,
        request: import('../../../src/web-server/services/analytics-remote-transport').AnalyticsRemoteRequest
      ) => {
        if (pass === 2 && request.kinds.includes('zcode')) {
          expect(request.fingerprints.zcode?.[FILE_1]).toBeUndefined();
          expect(request.fingerprints.zcode?.[DB_1]).toEqual({ size: 10, mtimeMs: 20 });
          retried = true;
        }
        return parseAnalyticsRemoteResponse(
          JSON.stringify(
            response({
              kinds: {
                zcode: {
                  state: pass === 1 ? 'error' : 'ok',
                  fingerprints: pass
                    ? { [DB_1]: { size: 10, mtimeMs: 20 }, [FILE_1]: { size: 30, mtimeMs: 40 } }
                    : { [DB_1]: { size: 10, mtimeMs: 20 } },
                },
              },
              rows: pass ? [row({ k: 'zcode', n: 12 })] : [row({ k: 'zcode', f: DB_1, n: 3 })],
            })
          )
        );
      },
    };
    expect(
      (await loadAnalyticsRemoteSources(MIN_DATE, deps)).results.find(
        (entry) => entry.tool === 'zcode'
      )?.data.eventCount
    ).toBe(3);
    pass++;
    expect(
      (await loadAnalyticsRemoteSources(MIN_DATE, deps)).results.find(
        (entry) => entry.tool === 'zcode'
      )?.data.eventCount
    ).toBe(3);
    pass++;
    expect(
      (await loadAnalyticsRemoteSources(MIN_DATE, deps)).results.find(
        (entry) => entry.tool === 'zcode'
      )?.data.eventCount
    ).toBe(15);
    expect(retried).toBe(true);
  });

  it('retains Antigravity store rows, sessions and prints while a conversation is unreadable', async () => {
    let pass = 0;
    const at = Date.parse('2026-10-02T00:00:00Z');
    const deps = {
      cacheDir: cache,
      now: () => at + pass * 3_600_000,
      hosts: async () => ({ mac: 'fixture', windows: '', nas1: '' }),
      runHelper: async () =>
        parseAnalyticsRemoteResponse(
          JSON.stringify(
            response({
              kinds: {
                antigravity: {
                  state: 'ok',
                  unreadable: pass > 0,
                  fingerprints: pass ? {} : { [FILE_1]: { size: 10, mtimeMs: 20 } },
                },
              },
              rows: pass ? [] : [row({ k: 'antigravity' })],
              srows: pass ? [] : [srow({ k: 'antigravity' })],
            })
          )
        ),
    };
    await loadAnalyticsRemoteSources(MIN_DATE, deps);
    pass++;
    const next = await loadAnalyticsRemoteSources(MIN_DATE, deps);
    expect(next.results.find((entry) => entry.tool === 'antigravity')?.data.eventCount).toBe(2);
    expect(next.results.find((entry) => entry.tool === 'antigravity')?.data.session).toHaveLength(
      1
    );
    expect(
      next.states.find((entry) => entry.host === 'mac' && entry.tool === 'antigravity')
    ).toMatchObject({
      state: 'cached',
      lastScanAt: new Date(at).toISOString(),
      detail: expect.stringContaining('could not be read'),
    });
    const saved = JSON.parse(
      fs.readFileSync(path.join(cache, 'analytics-remote-v1', 'mac.json'), 'utf8')
    );
    expect(saved.fingerprints.antigravity).toEqual({ [FILE_1]: { size: 10, mtimeMs: 20 } });
  });

  const hosts = async () => ({ mac: 'mac-alias', windows: 'win-alias', nas1: 'nas1-alias' });

  it('names skipped oversized usage lines in the partial cell state', async () => {
    const answer = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () =>
        parseAnalyticsRemoteResponse(
          JSON.stringify(
            response({
              kinds: {
                claude: {
                  state: 'ok',
                  partial: true,
                  fingerprints: { [FILE_1]: { size: 10, mtimeMs: 20, usageSkipped: true } },
                },
              },
              rows: [row({ k: 'claude' })],
            })
          )
        ),
    });
    const cells = answer.states.filter((state) => state.tool === 'claude');
    expect(cells).toHaveLength(3);
    expect(
      cells.every(
        (state) => state.state === 'cached' && state.detail?.includes('oversized log lines')
      )
    ).toBe(true);
  });

  it('inventories current VM filenames without transcript contents and notices pruning', () => {
    runWithScopedCcsHome(cache, () => {
      const name = 'a0b63bac-f4aa-4346-b817-096dc2c15c7f.jsonl';
      const original = path.join(cache, '.claude', 'projects', 'project', name);
      const mirror = path.join(
        cache,
        '.claude',
        'projects',
        `ssh-${name.slice(0, -6)}`,
        'agent-only.jsonl'
      );
      for (const file of [original, mirror]) {
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, 'metadata only; deliberately not JSON');
      }
      expect(localClaudeLogNames(cache, MIN_DATE)).toEqual({
        originals: new Set([createHash('sha256').update(name).digest('hex')]),
        complete: true,
      });
      fs.unlinkSync(original);
      expect(localClaudeLogNames(cache, MIN_DATE).originals.size).toBe(0);
    });
  });

  it.each([false, true])(
    'selects SSH mirrors after all hosts settle (Mac first: %s)',
    async (macFirst) => {
      const original = createHash('sha256').update('session-original.jsonl').digest('hex');
      const orphan = createHash('sha256').update('agent-orphan.jsonl').digest('hex');
      const remoteOriginal = createHash('sha256').update('session-remote.jsonl').digest('hex');
      const mirrorFiles = ['11', '22', '33'].map((prefix) => prefix.repeat(32));
      const originalFile = '44'.repeat(32);
      let localOriginals = new Set([original]);
      let pruned = false;
      const claudeLogNames = () => ({ originals: localOriginals, complete: true });
      const options = {
        hosts,
        cacheDir: cache,
        claudeLogNames,
        runHelper: async (
          _alias: string,
          host: AnalyticsRemoteHost,
          request: { kinds: string[]; fingerprints: Record<string, Record<string, unknown>> }
        ) => {
          if (!request.kinds.includes('claude'))
            return parseAnalyticsRemoteResponse(JSON.stringify(response({ rows: [], kinds: {} })));
          await new Promise((resolve) =>
            setTimeout(resolve, (host === 'mac') === macFirst ? 1 : 10)
          );
          const prints: Record<string, unknown> = {};
          if (host === 'mac' || host === 'windows')
            [original, orphan, remoteOriginal].forEach((name, index) => {
              prints[mirrorFiles[index]] = {
                size: 10,
                mtimeMs: 20,
                claudeName: name,
                claudeMirror: true,
              };
            });
          if (host === 'nas1' && !pruned)
            prints[originalFile] = {
              size: 10,
              mtimeMs: 20,
              claudeName: remoteOriginal,
              claudeMirror: false,
            };
          const fresh = Object.keys(prints).filter((key) => !request.fingerprints.claude?.[key]);
          return parseAnalyticsRemoteResponse(
            JSON.stringify(
              response({
                kinds: { claude: { state: 'ok', fingerprints: prints } },
                rows: fresh.map((f) => row({ k: 'claude', f, n: 1, c: 0 })),
                srows: fresh.map((f) => srow({ k: 'claude', f, n: 1, c: 0 })),
              })
            )
          );
        },
      };
      const count = (answer: Awaited<ReturnType<typeof loadAnalyticsRemoteSources>>) =>
        answer.results
          .filter((r) => r.tool === 'claude')
          .reduce((sum, r) => sum + (r.data.eventCount ?? 0), 0);
      const first = await loadAnalyticsRemoteSources(MIN_DATE, options);
      expect(count(first)).toBe(2); // One original on Nas1, one elected orphan mirror on Mac.
      expect(first.states.find((s) => s.host === 'mac' && s.tool === 'claude')?.rowCount).toBe(1);
      expect(first.states.find((s) => s.host === 'windows' && s.tool === 'claude')).toMatchObject({
        state: 'no_usage',
        rowCount: 0,
      });
      expect(count(await loadAnalyticsRemoteSources(MIN_DATE, options))).toBe(2);
      // Cached mirror rows survive suppression. Local pruning needs no mirror re-read.
      localOriginals = new Set();
      expect(
        count(loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache, claudeLogNames }))
      ).toBe(3);
      pruned = true;
      const after = await loadAnalyticsRemoteSources(MIN_DATE, options);
      expect(count(after)).toBe(3); // Both pruned originals are now represented by Mac mirrors.
      expect(
        after.results.filter((r) => r.tool === 'claude').flatMap((r) => r.data.session)
      ).toHaveLength(1);
      expect(
        count(loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache, claudeLogNames }))
      ).toBe(3);
    }
  );

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
      expect(saved.version).toBe(6);
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
              host: 'ubuntu',
              path: '/home/u/extra-jsonl',
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

  it('keeps Codex rows and explains unread compressed logs, or reports scanning on a cold read', async () => {
    const first = response({
      kinds: { codex: { state: 'ok', fingerprints: { [FILE_1]: { size: 10, mtimeMs: 20 } } } },
      rows: [row({ k: 'codex', m: 'gpt-5.4' })],
      srows: [srow({ k: 'codex' })],
    });
    await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(first)),
    });
    const skipped = response({
      kinds: { codex: { state: 'ok', partial: true, compressedUnread: 7, fingerprints: {} } },
      rows: [],
      srows: [],
    });
    const read = (cacheDir: string) =>
      loadAnalyticsRemoteSources(MIN_DATE, {
        hosts,
        cacheDir,
        runHelper: async () => parseAnalyticsRemoteResponse(JSON.stringify(skipped)),
      });
    const cached = await read(cache);
    expect(
      cached.states.find((entry) => entry.tool === 'codex' && entry.host === 'mac')
    ).toMatchObject({ state: 'cached', rowCount: 2 });
    expect(
      cached.states.find((entry) => entry.tool === 'codex' && entry.host === 'mac')?.detail
    ).toContain('7 compressed Codex logs');
    expect(cached.results.find((entry) => entry.tool === 'codex')?.data.session).toHaveLength(1);
    const cold = await read(path.join(cache, 'cold'));
    const state = cold.states.find((entry) => entry.tool === 'codex' && entry.host === 'mac');
    expect(state).toMatchObject({ state: 'scanning', rowCount: 0 });
    expect(state?.detail).toContain('needs Python 3.14 or the zstd tool');
    expect(cold.results).toEqual([]);
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

  it('names a damaged packaged helper instead of a host that did not answer', async () => {
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      hosts,
      cacheDir: cache,
      runHelper: async () => {
        throw new AnalyticsHelperIntegrityError();
      },
    });
    expect(states.length).toBeGreaterThan(0);
    for (const entry of states) {
      expect(entry.state).toBe('unavailable');
      expect(entry.detail).toContain('failed its integrity check');
    }
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

  it('prices route-less Claude hours and sessions like native Claude, keeping explicit routes', async () => {
    setCachedModelsDevRegistry({
      anthropic: {
        id: 'anthropic',
        name: 'Anthropic',
        models: {
          'claude-sonnet-4-6': {
            id: 'claude-sonnet-4-6',
            name: 'Sonnet',
            cost: { input: 9, output: 45 },
          },
        },
      },
    } as unknown as Parameters<typeof setCachedModelsDevRegistry>[0]);
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
            rows: request.kinds.includes('claude')
              ? [
                  row({
                    k: 'claude',
                    m: 'claude-sonnet-4-6',
                    i: 1_000_000,
                    o: 0,
                    cw: 0,
                    cr: 0,
                    c: 0,
                  }),
                  row({
                    k: 'claude',
                    p: 'anthropic',
                    m: 'claude-sonnet-4-6',
                    i: 1_000_000,
                    o: 0,
                    cw: 0,
                    cr: 0,
                    c: 0,
                  }),
                ]
              : [],
            srows: request.kinds.includes('claude')
              ? [
                  srow({
                    k: 'claude',
                    m: 'claude-sonnet-4-6',
                    i: 1_000_000,
                    o: 0,
                    cw: 0,
                    cr: 0,
                    c: 0,
                  }),
                ]
              : [],
          })
        ),
    });
    const claude = results.find((entry) => entry.tool === 'claude')?.data;
    const models = claude?.hourly[0].modelBreakdowns ?? [];
    expect(models.find((m) => m.provider === undefined)?.cost).toBeCloseTo(3, 9);
    expect(models.find((m) => m.provider === 'anthropic')?.cost).toBeCloseTo(9, 9);
    expect(claude?.session[0].cost).toBeCloseTo(3, 9);
    expect(claude?.session[0].target).toBe('claude');
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
