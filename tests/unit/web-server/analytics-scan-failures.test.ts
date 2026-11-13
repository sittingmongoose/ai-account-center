/**
 * Why a computer's Analytics scan failed, named instead of hidden (2026-11-13, Alexis PC):
 * - the launcher finds a working Python instead of trusting the first name on PATH (the Microsoft
 *   Store alias stub was first on Alexis PC, so every scan failed there for a day);
 * - each failure has one fixed cause, kept with the host until a scan answers again, so the page says
 *   "Scan failed: no working Python" instead of showing the last good scan's "no usage" as current.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { execFileSync, spawnSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AnalyticsRemoteTransportError,
  HELPER_RUNTIME_MISSING_EXIT,
  analyticsHelperCommand,
  classifyAnalyticsScanFailure,
  parseAnalyticsRemoteResponse,
  runAnalyticsRemoteHelper,
  type AnalyticsRemoteKind,
} from '../../../src/web-server/services/analytics-remote-transport';
import {
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
} from '../../../src/web-server/services/analytics-remote-sources';

const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');
const FILE_1 = 'a1'.repeat(32);
const KINDS: AnalyticsRemoteKind[] = ['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity'];

let dir: string;
let savedPath: string | undefined;
let savedCcs: string | undefined;
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-scan-failures-'));
  savedPath = process.env.PATH;
  savedCcs = process.env.CCS_HOME;
  process.env.CCS_HOME = path.join(dir, 'ccs');
});
afterEach(() => {
  process.env.PATH = savedPath;
  if (savedCcs === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = savedCcs;
  fs.rmSync(dir, { recursive: true, force: true });
});

function script(name: string, body: string): string {
  const file = path.join(dir, name);
  fs.writeFileSync(file, `#!/bin/sh\n${body}\n`, { mode: 0o755 });
  return file;
}

const realPython = (() => {
  try {
    return execFileSync('sh', ['-c', 'command -v python3'], { encoding: 'utf8' }).trim();
  } catch {
    return '';
  }
})();

describe.skipIf(process.platform === 'win32' || !realPython)('the POSIX launcher', () => {
  // The request the launcher hands Python: `helperSource` is executed with stdin replaced by `request`.
  const input = JSON.stringify({
    helperSource: 'import sys\nsys.stdout.write("ran:" + sys.stdin.read())',
    request: { kinds: ['omp'] },
  });

  it('skips a python3 that fails the probe and runs the helper with the next one', () => {
    // A Mac without the Command Line Tools: /usr/bin/python3 prints a note and exits 1.
    const stub = script(
      'python3-clt-stub',
      'echo "xcode-select: note: No developer tools were found" >&2\nexit 1'
    );
    const run = spawnSync('sh', ['-c', analyticsHelperCommand('nas1', [stub, realPython])], {
      input,
      encoding: 'utf8',
    });
    expect(run.status).toBe(0);
    expect(run.stdout).toBe('ran:{"kinds": ["omp"]}');
  });

  it('never lets a probe swallow the request', () => {
    // A candidate that reads stdin before failing would eat the request if the probe inherited it.
    const greedy = script('python3-greedy', 'cat >/dev/null\nexit 1');
    const run = spawnSync('sh', ['-c', analyticsHelperCommand('mac', [greedy, realPython])], {
      input,
      encoding: 'utf8',
    });
    expect(run.status).toBe(0);
    expect(run.stdout).toBe('ran:{"kinds": ["omp"]}');
  });

  it('exits with the fixed code and marker when no candidate is Python 3.9 or later', () => {
    const old = script('python3-old', 'exit 3');
    const run = spawnSync('sh', ['-c', analyticsHelperCommand('nas1', [old, '""'])], {
      input,
      encoding: 'utf8',
    });
    expect(run.status).toBe(HELPER_RUNTIME_MISSING_EXIT);
    expect(run.stderr).toContain('aac-helper: runtime_missing');
    expect(run.stdout).toBe('');
  });
});

describe('the Windows launcher', () => {
  const decoded = (): string => {
    const encoded = analyticsHelperCommand('alexis').match(
      /^powershell\.exe -NoProfile -NonInteractive -EncodedCommand ([A-Za-z0-9+/=]+)$/
    );
    expect(encoded).not.toBeNull();
    return Buffer.from(encoded?.[1] ?? '', 'base64').toString('utf16le');
  };

  it('tries py -3, then every python.exe and python3.exe on PATH, and probes each before running', () => {
    const text = decoded();
    // Before: the first of python3.exe/python.exe, unprobed (the Store stub on Alexis PC).
    expect(text).not.toContain(
      'Get-Command python3.exe,python.exe -ErrorAction SilentlyContinue | Select-Object -First 1'
    );
    const py = text.indexOf('Get-Command py.exe');
    const all = text.indexOf('Get-Command python.exe,python3.exe -All');
    const probe = text.indexOf('-c $probe');
    const run = text.indexOf('& $python @prefix -c ');
    expect(py).toBeGreaterThan(-1);
    expect(all).toBeGreaterThan(py);
    expect(probe).toBeGreaterThan(all);
    expect(run).toBeGreaterThan(probe);
    expect(text).toContain("'-3'");
  });

  it('keeps going past a stub that writes to stderr, and names a Store-only computer', () => {
    const text = decoded();
    // Under 'Stop', Windows PowerShell 5.1 ends the script at a probed stub's redirected stderr.
    expect(text).toStartWith("$ErrorActionPreference = 'Continue'");
    expect(text).toContain("'Python was not found'");
    expect(text).toContain('WindowsApps');
    expect(text).toContain('aac-helper: runtime_missing:store_alias');
    expect(text).toContain(`exit ${HELPER_RUNTIME_MISSING_EXIT}`);
    // The request still reaches Python only, never PowerShell's stdin.
    expect(text).not.toContain('[Console]::In');
  });
});

describe('scan failure causes', () => {
  it('classifies ssh, runtime and helper failures from exit codes and fixed markers only', () => {
    expect(
      classifyAnalyticsScanFailure({
        exitCode: 255,
        stderr: 'ssh: connect to host x port 22: No route to host',
      })
    ).toBe('ssh_unreachable');
    expect(
      classifyAnalyticsScanFailure({
        exitCode: 255,
        stderr: 'user@x: Permission denied (publickey).',
      })
    ).toBe('ssh_auth');
    expect(
      classifyAnalyticsScanFailure({ exitCode: 86, stderr: 'aac-helper: runtime_missing\n' })
    ).toBe('runtime_missing');
    expect(
      classifyAnalyticsScanFailure({
        exitCode: 86,
        stderr: 'aac-helper: runtime_missing:store_alias\n',
      })
    ).toBe('runtime_missing:store_alias');
    // What Alexis PC answered the old launcher: the Store alias stub's own words.
    expect(
      classifyAnalyticsScanFailure({
        exitCode: 49,
        stderr:
          'Python was not found; run without arguments to install from the Microsoft Store, or disable this shortcut from Settings > Apps > Advanced app settings > App execution aliases.',
      })
    ).toBe('runtime_missing:store_alias');
    expect(classifyAnalyticsScanFailure({ exitCode: null, killed: true, signal: 'SIGTERM' })).toBe(
      'timeout'
    );
    expect(
      classifyAnalyticsScanFailure({ exitCode: 1, stderr: 'Traceback (most recent call last):' })
    ).toBe('helper_failed');
  });

  describe.skipIf(process.platform === 'win32')('through the real transport', () => {
    // A fake `ssh` first on PATH: the transport's own process start and error handling run unchanged.
    const fakeSsh = (code: number, stderr: string) => {
      fs.mkdirSync(path.join(dir, 'bin'), { recursive: true });
      fs.writeFileSync(
        path.join(dir, 'bin', 'ssh'),
        `#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '${stderr}' >&2\nexit ${code}\n`,
        { mode: 0o755 }
      );
      process.env.PATH = `${path.join(dir, 'bin')}${path.delimiter}${savedPath ?? ''}`;
    };
    const run = () =>
      runAnalyticsRemoteHelper('alexis-windows', 'alexis', {
        kinds: ['codex'],
        minDateMs: MIN_DATE,
        fingerprints: {},
      }).then(
        () => null,
        (error: unknown) => error
      );

    it('names a computer with only the Store alias', async () => {
      fakeSsh(86, 'aac-helper: runtime_missing:store_alias');
      const error = await run();
      expect(error).toBeInstanceOf(AnalyticsRemoteTransportError);
      expect((error as AnalyticsRemoteTransportError).cause).toBe('runtime_missing:store_alias');
      expect((error as AnalyticsRemoteTransportError).exitCode).toBe(86);
    });

    it('names an unreachable computer and a refused key apart', async () => {
      fakeSsh(255, 'ssh: connect to host alexis port 22: Connection timed out');
      expect(((await run()) as AnalyticsRemoteTransportError).cause).toBe('ssh_unreachable');
      fakeSsh(255, 'user@alexis: Permission denied (publickey).');
      expect(((await run()) as AnalyticsRemoteTransportError).cause).toBe('ssh_auth');
    });
  });
});

describe('a standing scan failure (the Alexis PC case)', () => {
  const answer = (rows: boolean) =>
    parseAnalyticsRemoteResponse(
      JSON.stringify({
        version: 2,
        truncated: false,
        kinds: Object.fromEntries(
          KINDS.map((kind) => [
            kind,
            kind === 'omp'
              ? { state: 'ok', fingerprints: { [FILE_1]: { size: 10, mtimeMs: 20 } } }
              : kind === 'claude'
                ? { state: 'not_installed', fingerprints: {} }
                : { state: 'ok', fingerprints: {} },
          ])
        ),
        rows: rows
          ? [
              {
                k: 'omp',
                f: FILE_1,
                m: 'm',
                h: '2026-10-01 15:00',
                i: 1,
                o: 1,
                cr: 0,
                cw: 0,
                c: 0,
                n: 2,
              },
            ]
          : [],
        srows: [],
      })
    );

  it('says the scan failed and why, never the last good scan as current, also from the cache after a restart', async () => {
    const cache = path.join(dir, 'cache');
    const hosts = async () => ({ mac: null, windows: null, nas1: null, alexis: 'alexis-windows' });
    let now = Date.parse('2026-11-12T17:06:35Z');
    // 17:06: a scan that worked: Codex logs held no usage, Claude had no logs yet, OMP had usage.
    await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: cache,
      now: () => now,
      hosts,
      runHelper: async () => answer(true),
    });
    // Then every scan fails: the launcher started the Store alias stub.
    now = Date.parse('2026-11-13T05:34:00Z');
    const failed = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: cache,
      now: () => now,
      hosts,
      runHelper: async () => {
        throw new AnalyticsRemoteTransportError(false, 'runtime_missing:store_alias', 86);
      },
    });
    const cell = (states: typeof failed.states, tool: string) =>
      states.find((entry) => entry.host === 'alexis' && entry.tool === tool);
    // Before: Codex "no usage" and Claude "not installed", read 12 hours earlier, shown as now.
    expect(cell(failed.states, 'codex')).toMatchObject({
      state: 'scan_failed',
      cause: 'runtime_missing:store_alias',
      lastScanAt: '2026-11-12T17:06:35.000Z',
    });
    expect(cell(failed.states, 'codex')?.detail).toContain('Microsoft Store alias');
    expect(cell(failed.states, 'codex')?.detail).toContain('found no usage in range');
    expect(cell(failed.states, 'claude')?.state).toBe('scan_failed');
    // Usage read before is still counted, and says why it is not newer.
    expect(cell(failed.states, 'omp')).toMatchObject({
      state: 'cached',
      cause: 'runtime_missing:store_alias',
      rowCount: 2,
    });
    // The failure is kept with the host, and a restarted server (the cache alone) still names it.
    const saved = JSON.parse(
      fs.readFileSync(path.join(cache, 'analytics-remote-v1', 'alexis.json'), 'utf8')
    );
    expect(saved.lastFailure).toEqual({
      cause: 'runtime_missing:store_alias',
      exitCode: 86,
      at: '2026-11-13T05:34:00.000Z',
    });
    expect(saved.rows).toHaveLength(1);
    const restarted = loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache });
    expect(cell(restarted.states, 'codex')).toMatchObject({
      state: 'scan_failed',
      cause: 'runtime_missing:store_alias',
    });
    // A scan that answers again clears it.
    now = Date.parse('2026-11-13T06:00:00Z');
    const fixed = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: cache,
      now: () => now,
      hosts,
      runHelper: async () => answer(true),
    });
    expect(cell(fixed.states, 'codex')).toMatchObject({ state: 'no_usage' });
    expect(cell(fixed.states, 'codex')?.cause).toBeUndefined();
    const cleared = JSON.parse(
      fs.readFileSync(path.join(cache, 'analytics-remote-v1', 'alexis.json'), 'utf8')
    );
    expect(cleared.lastFailure).toBeUndefined();
  });

  it('drops a damaged saved failure instead of trusting it', async () => {
    const cache = path.join(dir, 'cache');
    fs.mkdirSync(path.join(cache, 'analytics-remote-v1'), { recursive: true });
    fs.writeFileSync(
      path.join(cache, 'analytics-remote-v1', 'alexis.json'),
      JSON.stringify({
        version: 6,
        fingerprints: {},
        rows: [],
        srows: [],
        lastScanAt: null,
        kinds: { codex: { lastScanAt: '2026-11-12T17:06:35.000Z', state: 'no_usage' } },
        lastFailure: { cause: '<script>', exitCode: 'x', at: 'never' },
      })
    );
    const { states } = loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache });
    expect(states.find((entry) => entry.host === 'alexis' && entry.tool === 'codex')?.state).toBe(
      'no_usage'
    );
  });
});
