/**
 * The Rust usage-log scanner on remote computers (DESIGN 12.2): the hash-checked launcher, the copy
 * and handshake, the labelled Python fallback, the scanner's answer sections, and the states the
 * page gets from them. Every test fails on origin/main, which has none of this.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import { spawnSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AnalyticsRemoteTransportError,
  parseAnalyticsRemoteResponse,
  resetScannerState,
  runAnalyticsRemoteScan,
  usageScanRequest,
  type AnalyticsRemoteKind,
  type AnalyticsRemoteRequest,
  type AnalyticsRemoteResponse,
} from '../../../src/web-server/services/analytics-remote-transport';
import {
  BINARY_MISSING_EXIT,
  UsageScanInstallError,
  binaryMarker,
  installUsageScan,
  readUsageScanManifest,
  remoteName,
  scanCommand,
  type SshRunner,
  type UsageScanManifest,
} from '../../../src/web-server/services/usage-scan-binary';
import {
  loadAnalyticsRemoteCachedSources,
  loadAnalyticsRemoteSources,
} from '../../../src/web-server/services/analytics-remote-sources';
import {
  readAnalyticsCaptureSettings,
  writeAnalyticsCaptureSettings,
} from '../../../src/web-server/services/analytics-capture-settings';

const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');

it('asks for read-only WAL-visible zcode usage rather than an immutable older checkpoint', () => {
  expect(
    usageScanRequest({ kinds: ['zcode'], minDateMs: MIN_DATE, fingerprints: {} }, 20000)
      .immutableSqlite
  ).toBe(false);
});
const KINDS: AnalyticsRemoteKind[] = ['claude', 'codex', 'omp', 'muse', 'zcode', 'antigravity'];
const FILE_1 = 'a1'.repeat(32);
const ROOT_1 = 'b2'.repeat(8);
const SESSION = 'c3'.repeat(8);

let dir: string;
let savedPath: string | undefined;
let savedCcs: string | undefined;
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-usage-scan-'));
  savedPath = process.env.PATH;
  savedCcs = process.env.CCS_HOME;
  process.env.CCS_HOME = path.join(dir, 'ccs');
  // SSH argv builders need an identity in the home; a placeholder, never a key.
  fs.mkdirSync(path.join(dir, 'ccs', '.ssh'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'ccs', '.ssh', 'id_ed25519'), 'fixture placeholder, not a key\n');
  resetScannerState();
});
afterEach(() => {
  process.env.PATH = savedPath;
  if (savedCcs === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = savedCcs;
  fs.rmSync(dir, { recursive: true, force: true });
});

const sha = (bytes: Buffer | string) => createHash('sha256').update(bytes).digest('hex');

/** A packaged "scanner": a shell script standing in for the binary, with its manifest. */
function packaged(body = '#!/bin/sh\ncat\n'): UsageScanManifest {
  const pkg = path.join(dir, 'pkg', 'x86_64-unknown-linux-gnu');
  fs.mkdirSync(pkg, { recursive: true });
  fs.writeFileSync(path.join(pkg, 'aac-usage-scan'), body, { mode: 0o755 });
  const manifest = {
    name: 'aac-usage-scan',
    version: '0.1.0',
    protocol: 3,
    targets: {
      'x86_64-unknown-linux-gnu': {
        file: 'x86_64-unknown-linux-gnu/aac-usage-scan',
        sha256: sha(body),
        size: Buffer.byteLength(body),
      },
    },
  };
  fs.writeFileSync(path.join(dir, 'pkg', 'manifest.json'), JSON.stringify(manifest));
  const read = readUsageScanManifest(path.join(dir, 'pkg'));
  if (!read) throw new Error('manifest did not read');
  return read;
}

function answer(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    version: 2,
    truncated: false,
    kinds: Object.fromEntries(
      KINDS.map((kind) => [
        kind,
        kind === 'codex'
          ? {
              state: 'ok',
              fingerprints: {
                [FILE_1]: { size: 10, mtimeMs: 20, head: 'a'.repeat(64), tail: 'a'.repeat(64) },
              },
            }
          : kind === 'claude'
            ? { state: 'not_installed', fingerprints: {} }
            : { state: 'ok', fingerprints: {} },
      ]).concat([
        ['cursor', { state: 'not_installed', fingerprints: {} }],
        ['devin', { state: 'not_installed', fingerprints: {} }],
      ])
    ),
    rows: [
      {
        k: 'codex',
        f: FILE_1,
        m: 'gpt-6.1-sol',
        h: '2026-10-01 15:00',
        i: 1,
        o: 2,
        cr: 0,
        cw: 0,
        c: 0,
        n: 3,
      },
    ],
    srows: [],
    reader: {
      name: 'aac-usage-scan',
      version: '0.1.0',
      protocol: 3,
      target: 'x86_64-unknown-linux-gnu',
    },
    discovery: {
      kinds: {
        codex: {
          installed: true,
          signals: ['home', 'binary'],
          roots: [
            {
              root: ROOT_1,
              origin: 'signature',
              state: 'ok',
              counts: { filesFound: 338, newestMtime: 1_790_000_000_000 },
            },
          ],
          counts: {
            roots: 1,
            filesFound: 338,
            filesRead: 338,
            filesFailed: 0,
            newestMtime: 1_790_000_000_000,
          },
        },
        claude: {
          installed: true,
          signals: ['t3_instance'],
          roots: [],
          counts: { roots: 0, filesFound: 0, filesRead: 0, filesFailed: 0, newestMtime: null },
        },
      },
      tools: {
        cursor: {
          installed: true,
          signals: ['home'],
          cause: 'no_local_usage_records',
          counts: { filesFound: 74, newestMtime: null },
        },
      },
    },
    sessions: {},
    ...overrides,
  };
}

describe.skipIf(process.platform === 'win32')('the hash-checked launcher (run for real)', () => {
  it('runs the copy whose digest matches, with the request on stdin', () => {
    const manifest = packaged();
    const home = path.join(dir, 'home');
    const bin = path.join(home, '.cache', 'ai-account-center', 'bin');
    fs.mkdirSync(bin, { recursive: true });
    const entry = manifest.targets['x86_64-unknown-linux-gnu'];
    fs.copyFileSync(
      path.join(manifest.dir, entry!.file),
      path.join(bin, remoteName('x86_64-unknown-linux-gnu', entry!.sha256))
    );
    fs.chmodSync(path.join(bin, remoteName('x86_64-unknown-linux-gnu', entry!.sha256)), 0o755);
    const run = spawnSync('sh', ['-c', scanCommand('nas1', manifest)], {
      input: '{"kinds":["codex"]}',
      encoding: 'utf8',
      env: { ...process.env, HOME: home },
    });
    if (process.arch !== 'x64' || process.platform !== 'linux') return;
    expect(run.status).toBe(0);
    expect(run.stdout).toBe('{"kinds":["codex"]}');
  });

  it('runs nothing and names the target when the copy is missing or altered', () => {
    const manifest = packaged();
    const home = path.join(dir, 'home');
    const missing = spawnSync('sh', ['-c', scanCommand('nas1', manifest)], {
      encoding: 'utf8',
      env: { ...process.env, HOME: home },
    });
    if (process.arch !== 'x64' || process.platform !== 'linux') return;
    expect(missing.status).toBe(BINARY_MISSING_EXIT);
    expect(binaryMarker(missing.status, missing.stderr)).toEqual({
      missing: 'x86_64-unknown-linux-gnu',
    });
    const bin = path.join(home, '.cache', 'ai-account-center', 'bin');
    fs.mkdirSync(bin, { recursive: true });
    const name = remoteName(
      'x86_64-unknown-linux-gnu',
      manifest.targets['x86_64-unknown-linux-gnu']!.sha256
    );
    fs.writeFileSync(path.join(bin, name), '#!/bin/sh\necho tampered\n', { mode: 0o755 });
    const altered = spawnSync('sh', ['-c', scanCommand('nas1', manifest)], {
      encoding: 'utf8',
      env: { ...process.env, HOME: home },
    });
    expect(altered.status).toBe(BINARY_MISSING_EXIT);
    expect(altered.stdout).toBe('');
  });

  it('says when no build exists for the computer', () => {
    const manifest = packaged();
    // A Mac computer: the package carries Linux only.
    const script = scanCommand('mac', { ...manifest, targets: {} });
    const run = spawnSync('sh', ['-c', script], { encoding: 'utf8' });
    expect(run.status).toBe(BINARY_MISSING_EXIT);
    expect(binaryMarker(run.status, run.stderr)).toMatchObject({ unavailable: expect.any(String) });
  });

  it('builds the Windows launcher to hash-check and run the .exe without reading stdin itself', () => {
    const manifest = packaged();
    const win = {
      ...manifest,
      targets: {
        'x86_64-pc-windows-msvc': { file: 'w/aac-usage-scan.exe', sha256: 'd'.repeat(64), size: 1 },
      },
    };
    const encoded = scanCommand('alexis', win).split(' ').pop() ?? '';
    const text = Buffer.from(encoded, 'base64').toString('utf16le');
    expect(text).toContain("Join-Path $env:LOCALAPPDATA 'ai-account-center\\bin'");
    expect(text).toContain(`'${remoteName('x86_64-pc-windows-msvc', 'd'.repeat(64))}'`);
    expect(text).toContain('Get-FileHash -LiteralPath $p -Algorithm SHA256');
    expect(text.indexOf('Get-FileHash')).toBeLessThan(text.indexOf('& $p'));
    expect(text).not.toContain('[Console]::In');
    // Fails closed (FIX-ROUND-BIG 14): 'Stop' before the check, the run only after `$verified`.
    expect(text.startsWith("$ErrorActionPreference = 'Stop'")).toBe(true);
    expect(text.indexOf('if ($verified -ne $true)')).toBeLessThan(text.indexOf('& $p'));
    expect(text).toContain('PROCESSOR_ARCHITEW6432');
  });
});

/**
 * The Windows launcher run by PowerShell itself (FIX-ROUND-BIG 14). The "scanner" is a copy of
 * hostname.exe, which prints a line when it runs. With Get-FileHash made to fail (a non-terminating
 * error and no object, or a throw) the launcher must exit BINARY_MISSING_EXIT and print nothing.
 * At b09066667 the null hash threw inside the `if`, PowerShell went on to `& $p` and the copy ran.
 */
describe.skipIf(process.platform !== 'win32')(
  'the Windows launcher fails closed (run for real)',
  () => {
    const run = (prefix: string) => {
      const exe = path.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'HOSTNAME.EXE');
      const body = fs.readFileSync(exe);
      const local = path.join(dir, 'local');
      const bin = path.join(local, 'ai-account-center', 'bin');
      fs.mkdirSync(bin, { recursive: true });
      const manifest: UsageScanManifest = {
        dir,
        version: '0.1.0',
        protocol: 3,
        targets: {
          'x86_64-pc-windows-msvc': { file: 'x.exe', sha256: sha(body), size: body.length },
        },
      };
      fs.writeFileSync(path.join(bin, remoteName('x86_64-pc-windows-msvc', sha(body))), body);
      const encoded = scanCommand('alexis', manifest).split(' ').pop() ?? '';
      const script = `${prefix}${Buffer.from(encoded, 'base64').toString('utf16le')}`;
      return spawnSync(
        'powershell.exe',
        [
          '-NoProfile',
          '-NonInteractive',
          '-EncodedCommand',
          Buffer.from(script, 'utf16le').toString('base64'),
        ],
        {
          encoding: 'utf8',
          env: { ...process.env, LOCALAPPDATA: local, PROCESSOR_ARCHITECTURE: 'AMD64' },
          timeout: 60_000,
        }
      );
    };

    it('runs the copy when its hash matches (control)', () => {
      const ok = run('');
      expect(ok.status).toBe(0);
      expect(ok.stdout.trim().length).toBeGreaterThan(0);
    });

    it('runs nothing when Get-FileHash gives no object', () => {
      const failed = run(
        "function Get-FileHash { Write-Error 'simulated hash failure'; return $null }; "
      );
      expect(failed.status).toBe(BINARY_MISSING_EXIT);
      expect(failed.stdout.trim()).toBe('');
      expect(binaryMarker(failed.status, failed.stderr)).toEqual({
        missing: 'x86_64-pc-windows-msvc',
      });
    });

    it('runs nothing when Get-FileHash throws or answers another hash', () => {
      const thrown = run("function Get-FileHash { throw 'simulated lock' }; ");
      expect(thrown.status).toBe(BINARY_MISSING_EXIT);
      expect(thrown.stdout.trim()).toBe('');
      const other = run(
        "function Get-FileHash { [pscustomobject]@{ Hash = 'AB' + ('0' * 62) } }; "
      );
      expect(other.status).toBe(BINARY_MISSING_EXIT);
      expect(other.stdout.trim()).toBe('');
    });
  }
);

describe('copying the scanner to a computer', () => {
  const steps = (codes: Array<number | null>, stdout: string[] = []) => {
    const calls: string[][] = [];
    const ssh: SshRunner = async (args, options) => {
      calls.push([options.file ?? 'ssh', ...args]);
      const index = calls.length - 1;
      return { stdout: stdout[index] ?? '', stderr: '', code: codes[index] ?? 0 };
    };
    return { calls, ssh };
  };

  it('copies by scp, verifies, moves into place and checks the version', async () => {
    const manifest = packaged();
    const version = JSON.stringify({
      name: 'aac-usage-scan',
      version: '0.1.0',
      protocol: 3,
      target: 'x86_64-unknown-linux-gnu',
    });
    const { calls, ssh } = steps([0, 0, 0], ['/home/u/.cache/ai-account-center/bin', '', version]);
    await installUsageScan('nas1-agent', 'nas1', 'x86_64-unknown-linux-gnu', manifest, ssh);
    expect(calls.map((call) => call[0])).toEqual(['ssh', 'scp', 'ssh']);
    expect(calls[1]?.slice(1, 3)).toEqual(['-q', '-S']);
    // scp runs the probed SSH file itself, never a name scp would resolve on its own.
    expect(path.isAbsolute(calls[1]![3]!)).toBe(true);
    expect(path.basename(calls[1]![3]!)).toMatch(/^ssh(\.exe)?$/);
    for (const call of calls) expect(call).toContain('UpdateHostKeys=no');
    expect(calls[1].at(-1)).toBe(
      `nas1-agent:/home/u/.cache/ai-account-center/bin/${remoteName('x86_64-unknown-linux-gnu', manifest.targets['x86_64-unknown-linux-gnu']!.sha256)}.part`
    );
    expect(calls[2].at(-1)).toContain(manifest.targets['x86_64-unknown-linux-gnu']!.sha256);
  });

  it('names the step that failed and never trusts a wrong answer', async () => {
    const manifest = packaged();
    await expect(
      installUsageScan('a', 'nas1', 'x86_64-unknown-linux-gnu', manifest, steps([1]).ssh)
    ).rejects.toMatchObject({ step: 'mkdir' });
    await expect(
      installUsageScan('a', 'nas1', 'x86_64-unknown-linux-gnu', manifest, steps([0, 1], ['/d']).ssh)
    ).rejects.toMatchObject({ step: 'copy' });
    await expect(
      installUsageScan(
        'a',
        'nas1',
        'x86_64-unknown-linux-gnu',
        manifest,
        steps([0, 0, 3], ['/d']).ssh
      )
    ).rejects.toMatchObject({ step: 'verify' });
    const wrong = JSON.stringify({
      name: 'aac-usage-scan',
      version: '9.9.9',
      protocol: 3,
      target: 'x86_64-unknown-linux-gnu',
    });
    await expect(
      installUsageScan(
        'a',
        'nas1',
        'x86_64-unknown-linux-gnu',
        manifest,
        steps([0, 0, 0], ['/d', '', wrong]).ssh
      )
    ).rejects.toMatchObject({ step: 'version' });
    // A packaged file that does not match its manifest is never sent.
    fs.writeFileSync(
      path.join(manifest.dir, 'x86_64-unknown-linux-gnu', 'aac-usage-scan'),
      'changed'
    );
    await expect(
      installUsageScan('a', 'nas1', 'x86_64-unknown-linux-gnu', manifest, steps([0]).ssh)
    ).rejects.toBeInstanceOf(UsageScanInstallError);
  });
});

describe.skipIf(process.platform === 'win32')('the scanner or the labelled Python fallback', () => {
  /** A fake `ssh` first on PATH: answers from a queue of (exit, stdout, stderr) files. */
  function fakeSsh(replies: Array<{ code: number; stdout?: string; stderr?: string }>) {
    const bin = path.join(dir, 'bin');
    fs.mkdirSync(bin, { recursive: true });
    replies.forEach((reply, index) => {
      fs.writeFileSync(path.join(bin, `out-${index}`), reply.stdout ?? '');
      fs.writeFileSync(path.join(bin, `err-${index}`), reply.stderr ?? '');
      fs.writeFileSync(path.join(bin, `code-${index}`), String(reply.code));
    });
    fs.writeFileSync(
      path.join(bin, 'ssh'),
      `#!/bin/sh\nif [ "$1" = "-G" ]; then printf 'hostname fixture.invalid\\nuser fixture\\nport 22\\nidentityfile /fixture/key\\n'; exit 0; fi\ncat >/dev/null\nn=$(cat "${bin}/n" 2>/dev/null || echo 0)\necho $((n+1)) > "${bin}/n"\ncat "${bin}/out-$n"\ncat "${bin}/err-$n" >&2\nexit $(cat "${bin}/code-$n")\n`,
      { mode: 0o755 }
    );
    process.env.PATH = `${bin}${path.delimiter}${savedPath ?? ''}`;
  }
  const request: AnalyticsRemoteRequest = {
    kinds: ['codex'],
    minDateMs: MIN_DATE,
    fingerprints: {},
    presence: true,
  };
  const python = async (_a: string, _p: unknown, older: AnalyticsRemoteRequest) => {
    seen.push(older);
    return parseAnalyticsRemoteResponse(
      JSON.stringify(answer({ reader: undefined, discovery: undefined }))
    );
  };
  let seen: AnalyticsRemoteRequest[];
  beforeEach(() => {
    seen = [];
  });

  it('copies a missing scanner, then reads with it', async () => {
    fakeSsh([
      { code: BINARY_MISSING_EXIT, stderr: 'aac-helper: binary_missing:x86_64-unknown-linux-gnu' },
      { code: 0, stdout: JSON.stringify(answer()) },
    ]);
    const installs: string[] = [];
    const result = await runAnalyticsRemoteScan('nas1-agent', 'nas1', request, {
      manifest: packaged(),
      install: async (_alias, _platform, target) => {
        installs.push(target);
      },
      python,
    });
    expect(installs).toEqual(['x86_64-unknown-linux-gnu']);
    expect(result.reader).toMatchObject({ name: 'aac-usage-scan' });
    expect(result.discovery?.tools?.cursor?.cause).toBe('no_local_usage_records');
    expect(seen).toEqual([]);
  });

  it('falls back to Python, labelled with why, without the fields Python refuses, and waits before retrying', async () => {
    fakeSsh([
      { code: BINARY_MISSING_EXIT, stderr: 'aac-helper: binary_missing:x86_64-unknown-linux-gnu' },
    ]);
    let now = 1_000;
    const deps = {
      manifest: packaged(),
      install: async () => {
        throw new UsageScanInstallError('copy');
      },
      python,
      now: () => now,
    };
    const first = await runAnalyticsRemoteScan(
      'nas1-agent',
      'nas1',
      { ...request, cwd: false },
      deps
    );
    expect(first.reader).toEqual({ name: 'python-fallback', cause: 'binary_install:copy' });
    expect(seen[0]).not.toHaveProperty('presence');
    expect(seen[0]).not.toHaveProperty('cwd');
    // Within six hours the scanner is not tried again: no ssh call is queued for it.
    now += 60_000;
    const second = await runAnalyticsRemoteScan('nas1-agent', 'nas1', request, deps);
    expect(second.reader).toEqual({ name: 'python-fallback', cause: 'binary_install:copy' });
  });

  it('never asks Python for Cursor or Devin: they are answered not reached, under the fallback label', async () => {
    const answer = await runAnalyticsRemoteScan(
      'nas1-agent',
      'nas1',
      { ...request, kinds: ['codex', 'cursor', 'devin'] },
      { manifest: null, python, now: () => 5_000 }
    );
    expect(seen.at(-1)?.kinds).toEqual(['codex']);
    expect(answer.kinds.cursor).toEqual({ state: 'pending', fingerprints: {} });
    expect(answer.kinds.devin).toEqual({ state: 'pending', fingerprints: {} });
    expect(answer.reader?.name).toBe('python-fallback');
  });

  it('labels a package without a scanner, and never blames the scanner for an unreachable computer', async () => {
    const plain = await runAnalyticsRemoteScan('nas1-agent', 'nas1', request, {
      manifest: null,
      python,
    });
    expect(plain.reader).toEqual({ name: 'python-fallback', cause: 'binary_not_built' });
    fakeSsh([{ code: 255, stderr: 'ssh: connect to host nas1 port 22: No route to host' }]);
    const error = await runAnalyticsRemoteScan('nas1-agent', 'nas1', request, {
      manifest: packaged(),
      python,
    }).catch((e) => e);
    expect(error).toBeInstanceOf(AnalyticsRemoteTransportError);
    expect((error as AnalyticsRemoteTransportError).cause).toBe('ssh_unreachable');
  });
});

describe('the scanner answer and the cells it gives', () => {
  it('drops malformed discovery and evidence entries without dropping usage', () => {
    const parsed = parseAnalyticsRemoteResponse(
      JSON.stringify(
        answer({
          sessions: {
            [SESSION]: {
              hash: `${SESSION}${'d'.repeat(48)}`,
              cwd: {
                key: 'e'.repeat(16),
                path: '~/code/proj',
                identitySource: 'inferred_cwd',
                confidence: 'inferred',
              },
            },
            ['f'.repeat(16)]: { hash: 'not-a-hash', cwd: null },
            [`${'0'.repeat(16)}`]: { hash: `${'1'.repeat(64)}`, cwd: null },
          },
          discovery: {
            kinds: {
              codex: {
                installed: true,
                signals: ['home'],
                roots: [
                  {
                    root: '/etc/passwd',
                    origin: 'default',
                    state: 'ok',
                    counts: { filesFound: 1, newestMtime: null },
                  },
                ],
                counts: {
                  roots: 1,
                  filesFound: 1,
                  filesRead: 1,
                  filesFailed: 0,
                  newestMtime: null,
                },
              },
            },
            tools: {
              cursor: {
                installed: true,
                signals: ['home'],
                cause: 'made-up',
                counts: { filesFound: 1, newestMtime: null },
              },
            },
          },
        })
      )
    );
    expect(parsed.rows).toHaveLength(1);
    expect(Object.keys(parsed.sessions ?? {})).toEqual([SESSION]);
    expect(parsed.discovery?.kinds.codex).toBeUndefined();
    expect(parsed.discovery?.tools?.cursor).toBeUndefined();
  });

  it('turns discovery into installed-no-logs, no-reader, counts and origins, and keeps them for a restart', async () => {
    const cache = path.join(dir, 'cache');
    const hosts = async () => ({ mac: null, windows: null, nas1: 'nas1-agent', alexis: null });
    const runHelper = async (): Promise<AnalyticsRemoteResponse> =>
      parseAnalyticsRemoteResponse(JSON.stringify(answer()));
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: cache,
      hosts,
      runHelper,
      now: () => Date.parse('2026-10-02T00:00:00Z'),
    });
    const cell = (list: typeof states, tool: string) =>
      list.find((entry) => entry.host === 'nas1' && entry.tool === tool);
    expect(cell(states, 'claude')).toMatchObject({ state: 'installed_no_logs' });
    expect(cell(states, 'codex')).toMatchObject({
      state: 'ok',
      origins: [{ origin: 'signature', files: 338 }],
    });
    expect(cell(states, 'codex')?.counts).toMatchObject({ roots: 1, filesFound: 338 });
    expect(cell(states, 'cursor')).toMatchObject({
      state: 'no_reader',
      cause: 'no_local_usage_records',
    });
    const restarted = loadAnalyticsRemoteCachedSources(MIN_DATE, { cacheDir: cache });
    expect(cell(restarted.states, 'cursor')).toMatchObject({ state: 'no_reader' });
    expect(cell(restarted.states, 'claude')).toMatchObject({ state: 'installed_no_logs' });
  });

  it('sends cwd: false when cwd capture is turned off, and captures by default', async () => {
    expect(readAnalyticsCaptureSettings().cwd).toBe(true);
    writeAnalyticsCaptureSettings({ cwd: false });
    expect(readAnalyticsCaptureSettings().cwd).toBe(false);
    const requests: AnalyticsRemoteRequest[] = [];
    await loadAnalyticsRemoteSources(MIN_DATE, {
      cacheDir: path.join(dir, 'cache'),
      hosts: async () => ({ mac: null, windows: null, nas1: 'nas1-agent', alexis: null }),
      runHelper: async (_a, _p, request) => {
        requests.push(request);
        return parseAnalyticsRemoteResponse(JSON.stringify(answer()));
      },
    });
    expect(requests.every((request) => request.cwd === false)).toBe(true);
    expect(requests.filter((request) => request.presence === true)).toHaveLength(1);
  });
});
