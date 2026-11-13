/**
 * The Rust usage-log scanner on remote computers (DESIGN 12.2 in the share folder
 * 2026-11-13-analytics-discovery). The package carries one `aac-usage-scan` per target in
 * dist/native/aac-usage with a manifest of their SHA-256 digests (scripts/build-usage-scan.js). AAC
 * keeps a copy in its own cache folder on each computer, named by its digest:
 *
 * - POSIX: `~/.cache/ai-account-center/bin/aac-usage-scan-<sha256[:12]>`;
 * - Windows: `%LOCALAPPDATA%\ai-account-center\bin\aac-usage-scan-<sha256[:12]>.exe`.
 *
 * Every scan is one ssh call: the fixed launcher (scanCommand) picks the copy for the computer's
 * target, checks its SHA-256 against the pinned value and runs it with the request on stdin.
 * Missing or mismatched, it runs nothing and exits BINARY_MISSING_EXIT naming the target; the
 * server then copies the packaged binary over (scp, same key, BatchMode), verifies it there, renames
 * it into place, runs `--version` and checks the answer (installUsageScan). Nothing else is written:
 * the binary only reads usage logs. Older copies are removed once the new one answered.
 */
import { execFile } from 'child_process';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import { HOST_OS, type RemoteDashboardHost as AnalyticsRemoteHost } from './dashboard-hosts';

export const BINARY_MISSING_EXIT = 87;
export const USAGE_SCAN_NAME = 'aac-usage-scan';

export type UsageScanTarget =
  | 'x86_64-unknown-linux-gnu'
  | 'aarch64-apple-darwin'
  | 'x86_64-apple-darwin'
  | 'x86_64-pc-windows-msvc';
export const USAGE_SCAN_TARGETS: readonly UsageScanTarget[] = [
  'x86_64-unknown-linux-gnu',
  'aarch64-apple-darwin',
  'x86_64-apple-darwin',
  'x86_64-pc-windows-msvc',
];

export interface UsageScanManifest {
  dir: string;
  version: string;
  protocol: number;
  targets: Partial<Record<UsageScanTarget, { file: string; sha256: string; size: number }>>;
}

/** The packaged scanners next to the built server (dist/native/aac-usage). */
export function usageScanDir(): string {
  return path.resolve(__dirname, '../../native/aac-usage');
}

/** The packaged manifest, validated; null when no scanner was built into this package. */
export function readUsageScanManifest(dir = usageScanDir()): UsageScanManifest | null {
  try {
    const raw = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8')) as Record<
      string,
      unknown
    >;
    if (
      raw.name !== USAGE_SCAN_NAME ||
      typeof raw.version !== 'string' ||
      !/^[0-9A-Za-z.+-]{1,32}$/.test(raw.version) ||
      !Number.isSafeInteger(raw.protocol) ||
      !raw.targets ||
      typeof raw.targets !== 'object'
    )
      return null;
    const targets: UsageScanManifest['targets'] = {};
    for (const [target, entry] of Object.entries(raw.targets as Record<string, unknown>)) {
      const item = entry as { file?: unknown; sha256?: unknown; size?: unknown };
      if (
        !USAGE_SCAN_TARGETS.includes(target as UsageScanTarget) ||
        typeof item.file !== 'string' ||
        item.file.includes('..') ||
        typeof item.sha256 !== 'string' ||
        !/^[0-9a-f]{64}$/.test(item.sha256) ||
        !Number.isSafeInteger(item.size)
      )
        continue;
      targets[target as UsageScanTarget] = {
        file: item.file,
        sha256: item.sha256,
        size: item.size as number,
      };
    }
    if (!Object.keys(targets).length) return null;
    return { dir, version: raw.version, protocol: raw.protocol as number, targets };
  } catch {
    return null;
  }
}

/** The copy's file name on the computer: the digest names the build. */
export function remoteName(target: UsageScanTarget, sha256: string): string {
  return `${USAGE_SCAN_NAME}-${sha256.slice(0, 12)}${target.endsWith('windows-msvc') ? '.exe' : ''}`;
}

function quoteShell(value: string): string {
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}

/**
 * The fixed launcher: the copy for this computer's target, hash-checked, run with stdin inherited.
 * Exit BINARY_MISSING_EXIT with `aac-helper: binary_missing:<target>` (copy it) or
 * `aac-helper: binary_unavailable:<os-arch>` (no build for this computer).
 */
export function scanCommand(platform: AnalyticsRemoteHost, manifest: UsageScanManifest): string {
  if (HOST_OS[platform] === 'windows') {
    const entry = manifest.targets['x86_64-pc-windows-msvc'];
    const lines = [
      "$ErrorActionPreference = 'Continue'",
      "$ProgressPreference = 'SilentlyContinue'",
      "$dir = Join-Path $env:LOCALAPPDATA 'ai-account-center\\bin'",
      entry
        ? `if ($env:PROCESSOR_ARCHITECTURE -eq 'AMD64') { $p = Join-Path $dir '${remoteName('x86_64-pc-windows-msvc', entry.sha256)}'; $h = '${entry.sha256}'; $t = 'x86_64-pc-windows-msvc' } else { $p = $null }`
        : '$p = $null',
      `if (-not $p) { [Console]::Error.WriteLine('aac-helper: binary_unavailable:windows-' + $env:PROCESSOR_ARCHITECTURE); exit ${BINARY_MISSING_EXIT} }`,
      `if (-not (Test-Path -LiteralPath $p)) { [Console]::Error.WriteLine('aac-helper: binary_missing:' + $t); exit ${BINARY_MISSING_EXIT} }`,
      `if ((Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLower() -ne $h) { [Console]::Error.WriteLine('aac-helper: binary_missing:' + $t); exit ${BINARY_MISSING_EXIT} }`,
      // No pipeline input: the scanner inherits the ssh session's stdin and reads the request itself.
      '& $p',
      'exit $LASTEXITCODE',
    ];
    return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(lines.join('; '), 'utf16le').toString('base64')}`;
  }
  const cases: string[] = [];
  const posix: Array<[UsageScanTarget, string]> = [
    ['x86_64-unknown-linux-gnu', 'Linux-x86_64'],
    ['aarch64-apple-darwin', 'Darwin-arm64'],
    ['x86_64-apple-darwin', 'Darwin-x86_64'],
  ];
  for (const [target, uname] of posix) {
    const entry = manifest.targets[target];
    if (entry)
      cases.push(
        `  ${uname}) f=${remoteName(target, entry.sha256)}; h=${entry.sha256}; t=${target} ;;`
      );
  }
  const script = [
    'dir="$HOME/.cache/ai-account-center/bin"',
    'case "$(uname -s)-$(uname -m)" in',
    ...cases,
    `  *) echo "aac-helper: binary_unavailable:$(uname -s)-$(uname -m)" >&2; exit ${BINARY_MISSING_EXIT} ;;`,
    'esac',
    'p="$dir/$f"',
    `[ -x "$p" ] || { echo "aac-helper: binary_missing:$t" >&2; exit ${BINARY_MISSING_EXIT}; }`,
    'got=$( (sha256sum "$p" 2>/dev/null || shasum -a 256 "$p" 2>/dev/null) | cut -c1-64)',
    `[ "$got" = "$h" ] || { echo "aac-helper: binary_missing:$t" >&2; exit ${BINARY_MISSING_EXIT}; }`,
    'exec "$p"',
  ].join('\n');
  return `/bin/sh -c ${quoteShell(script)}`;
}

/** What the launcher's exit and stderr say about the copy. */
export function binaryMarker(
  exitCode: number | null,
  stderr: string
): { missing: UsageScanTarget } | { unavailable: string } | null {
  if (exitCode !== BINARY_MISSING_EXIT) return null;
  const missing = stderr.match(/aac-helper: binary_missing:([a-z0-9_-]+)/);
  if (missing && USAGE_SCAN_TARGETS.includes(missing[1] as UsageScanTarget))
    return { missing: missing[1] as UsageScanTarget };
  const unavailable = stderr.match(/aac-helper: binary_unavailable:([A-Za-z0-9_-]{1,40})/);
  return { unavailable: unavailable ? unavailable[1] : 'unknown' };
}

export type SshRunner = (
  args: string[],
  options: { input?: Buffer | string; timeoutMs: number; file?: string }
) => Promise<{ stdout: string; stderr: string; code: number | null }>;

const SSH_OPTIONS = ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5', '-o', 'ConnectionAttempts=1'];

/** One ssh or scp process; never rejects, the exit code says what happened. */
export const runSsh: SshRunner = (args, options) =>
  new Promise((resolve) => {
    const child = execFile(
      options.file ?? 'ssh',
      args,
      { encoding: 'utf8', timeout: options.timeoutMs, maxBuffer: 1024 * 1024, windowsHide: true },
      (error, stdout, stderr) => {
        const code =
          error && typeof (error as { code?: unknown }).code === 'number'
            ? ((error as { code: number }).code as number)
            : error
              ? null
              : 0;
        resolve({ stdout: String(stdout ?? ''), stderr: String(stderr ?? ''), code });
      }
    );
    child.stdin?.on('error', () => {
      /* Exit codes carry the outcome. */
    });
    child.stdin?.end(options.input ?? '');
  });

export class UsageScanInstallError extends Error {
  constructor(readonly step: 'local' | 'mkdir' | 'copy' | 'verify' | 'version') {
    super(`The usage-log scanner could not be installed (${step}).`);
    this.name = 'UsageScanInstallError';
  }
}

function sha256(file: string): string {
  return createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

/**
 * Copy the packaged scanner for `target` to the computer, verify it there, move it into place, run
 * `--version` and check the answer; then remove older copies. Throws UsageScanInstallError naming
 * the step that failed; nothing is left in place unless every step passed.
 */
export async function installUsageScan(
  alias: string,
  platform: AnalyticsRemoteHost,
  target: UsageScanTarget,
  manifest: UsageScanManifest,
  ssh: SshRunner = runSsh
): Promise<void> {
  const entry = manifest.targets[target];
  if (!entry) throw new UsageScanInstallError('local');
  const local = path.join(manifest.dir, entry.file);
  try {
    if (fs.statSync(local).size !== entry.size || sha256(local) !== entry.sha256)
      throw new UsageScanInstallError('local');
  } catch (error) {
    if (error instanceof UsageScanInstallError) throw error;
    throw new UsageScanInstallError('local');
  }
  const name = remoteName(target, entry.sha256);
  const windows = HOST_OS[platform] === 'windows';
  const encode = (script: string) =>
    `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
  // 1. The folder, and where it is (scp needs the path).
  const made = await ssh(
    [
      ...SSH_OPTIONS,
      '-T',
      '--',
      alias,
      windows
        ? encode(
            "$ErrorActionPreference = 'Stop'; $d = Join-Path $env:LOCALAPPDATA 'ai-account-center\\bin'; New-Item -ItemType Directory -Force -Path $d | Out-Null; [Console]::Out.Write(($d -replace '\\\\', '/'))"
          )
        : '/bin/sh -c \'mkdir -p "$HOME/.cache/ai-account-center/bin" && chmod 700 "$HOME/.cache/ai-account-center" && cd "$HOME/.cache/ai-account-center/bin" && pwd\'',
    ],
    { timeoutMs: 30_000 }
  );
  const dir = made.stdout.trim();
  if (made.code !== 0 || !dir || /[\r\n'"`$;|&]/.test(dir))
    throw new UsageScanInstallError('mkdir');
  // 2. The bytes, by scp: never through a PowerShell stdin read (large reads hang over Windows OpenSSH).
  const remotePart = `${windows && /^[A-Za-z]:/.test(dir) ? `/${dir}` : dir}/${name}.part`;
  const copied = await ssh(['-q', ...SSH_OPTIONS, '--', local, `${alias}:${remotePart}`], {
    timeoutMs: 120_000,
    file: 'scp',
  });
  if (copied.code !== 0) throw new UsageScanInstallError('copy');
  // 3. Verify there, move into place, answer --version; then drop older copies.
  const verified = await ssh(
    [
      ...SSH_OPTIONS,
      '-T',
      '--',
      alias,
      windows
        ? encode(
            [
              "$ErrorActionPreference = 'Stop'",
              "$d = Join-Path $env:LOCALAPPDATA 'ai-account-center\\bin'",
              `$part = Join-Path $d '${name}.part'`,
              `$p = Join-Path $d '${name}'`,
              `if ((Get-FileHash -LiteralPath $part -Algorithm SHA256).Hash.ToLower() -ne '${entry.sha256}') { Remove-Item -LiteralPath $part -Force; exit 3 }`,
              'Move-Item -LiteralPath $part -Destination $p -Force',
              '$v = & $p --version',
              'if ($LASTEXITCODE -ne 0) { exit 4 }',
              `Get-ChildItem -LiteralPath $d -Filter '${USAGE_SCAN_NAME}-*' | Where-Object { $_.Name -ne '${name}' } | Remove-Item -Force -ErrorAction SilentlyContinue`,
              '[Console]::Out.Write($v)',
            ].join('; ')
          )
        : `/bin/sh -c ${quoteShell(
            [
              'cd "$HOME/.cache/ai-account-center/bin" || exit 2',
              `got=$( (sha256sum ${name}.part 2>/dev/null || shasum -a 256 ${name}.part 2>/dev/null) | cut -c1-64)`,
              `[ "$got" = "${entry.sha256}" ] || { rm -f ${name}.part; exit 3; }`,
              `chmod 755 ${name}.part && mv -f ${name}.part ${name} || exit 2`,
              `v=$(./${name} --version) || exit 4`,
              `for old in ${USAGE_SCAN_NAME}-*; do [ "$old" = ${name} ] || rm -f -- "$old"; done`,
              'printf "%s" "$v"',
            ].join('\n')
          )}`,
    ],
    { timeoutMs: 60_000 }
  );
  if (verified.code === 3) throw new UsageScanInstallError('verify');
  if (verified.code !== 0) throw new UsageScanInstallError('version');
  try {
    const answer = JSON.parse(verified.stdout) as Record<string, unknown>;
    if (
      answer.name !== USAGE_SCAN_NAME ||
      answer.version !== manifest.version ||
      answer.protocol !== manifest.protocol ||
      answer.target !== target
    )
      throw new UsageScanInstallError('version');
  } catch (error) {
    if (error instanceof UsageScanInstallError) throw error;
    throw new UsageScanInstallError('version');
  }
}
