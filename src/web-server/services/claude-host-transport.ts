import { execFile } from 'child_process';
import { NetworkError, ValidationError } from '../../errors/error-types';
import { MAC_HOST_PROGRAM } from './claude-host-mac-program';
import {
  CLAUDE_PROFILE_ID,
  parseLauncher,
  type ClaudeHost,
  type ClaudeHostLauncher,
} from './claude-account-stores';

/**
 * Host steps for Claude Add and Remove (CONTRACT-registry-lifecycle 6.2, 6.7,
 * 6.8), over the configured ssh aliases with fixed programs only:
 * - Mac: a fixed Python program run by `/usr/bin/python3 -c <bootstrap>`; the
 *   program and its validated request travel on stdin.
 * - Windows: a fixed PowerShell script read from stdin by a short encoded
 *   bootstrap; the only values in it are a profile id, a trash folder name and
 *   paths from the server's own records, each validated first and checked again
 *   on the host (a data folder must be a direct child of Application Support or
 *   %APPDATA% named `Claude*`; trash folders live only in `~/.ccs/trash/claude`).
 * Renames stay on one volume; nothing is ever copied. Output is a small JSON
 * object; host text is never surfaced or logged.
 */
export interface ClaudeHostStep {
  profileId: string;
  launcher: ClaudeHostLauncher;
}

export interface ClaudeHostTransport {
  create(
    host: ClaudeHost,
    input: { profileId: string; sshHost: string }
  ): Promise<ClaudeHostLauncher>;
  undoCreate(host: ClaudeHost, input: ClaudeHostStep): Promise<void>;
  appState(
    host: ClaudeHost,
    input: { launcher: ClaudeHostLauncher }
  ): Promise<'running' | 'stopped'>;
  trash(
    host: ClaudeHost,
    input: ClaudeHostStep & { trashName: string }
  ): Promise<'moved' | 'cross_volume'>;
  restore(host: ClaudeHost, input: ClaudeHostStep & { trashName: string }): Promise<void>;
  purge(host: ClaudeHost, input: { sshHost: string; trashName: string }): Promise<void>;
}

export type ClaudeHostRunner = (sshHost: string, command: string, input: string) => Promise<string>;

const TRASH_NAME = /^[a-z][a-z0-9-]{1,31}-\d{8}T\d{6}Z$/;
/**
 * A host path that may sit inside a PowerShell single-quoted string: letters,
 * marks, digits, spaces and the punctuation Windows paths use. Every quote
 * PowerShell honours (ASCII and the U+2018-U+201E typographic ones), `$`, the
 * backtick and control characters are outside it.
 */
const SAFE_PATH = /^[\p{L}\p{M}\p{N} _.,()[\]{}\\/:+@#&=!%~;-]{1,1024}$/u;

export class ClaudeHostError extends NetworkError {
  constructor() {
    super('A Claude host step failed.');
    this.name = 'ClaudeHostError';
  }
}

export const runClaudeHostOverSsh: ClaudeHostRunner = (sshHost, command, input) =>
  new Promise((resolve, reject) => {
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(sshHost)) {
      reject(new ValidationError('Claude host alias is not configured safely.'));
      return;
    }
    const child = execFile(
      'ssh',
      [
        '-T',
        '-o',
        'BatchMode=yes',
        '-o',
        'ConnectTimeout=5',
        '-o',
        'ConnectionAttempts=1',
        '-o',
        'ServerAliveInterval=5',
        '-o',
        'ServerAliveCountMax=1',
        '--',
        sshHost,
        command,
      ],
      { encoding: 'utf8', timeout: 60_000, maxBuffer: 64 * 1024, windowsHide: true },
      (error, stdout) => (error ? reject(new ClaudeHostError()) : resolve(stdout))
    );
    child.stdin?.on('error', () => undefined);
    child.stdin?.end(input);
  });

const MAC_BOOTSTRAP =
  "import sys,json;_d=json.loads(sys.stdin.read());exec(compile(_d['program'],'aac-claude-host','exec'),{'REQUEST':_d['request'],'__name__':'__main__'})";

const WINDOWS_BOOTSTRAP = Buffer.from(
  '$s = [Console]::In.ReadToEnd(); & ([scriptblock]::Create($s)); exit $LASTEXITCODE',
  'utf16le'
).toString('base64');

function psValue(value: string): string {
  if (!SAFE_PATH.test(value)) throw new ValidationError('Claude host value is not safe.');
  return `'${value}'`;
}

const WINDOWS_COMMON = [
  "$ErrorActionPreference = 'Stop'",
  "$ProgressPreference = 'SilentlyContinue'",
  '$support = [IO.Path]::GetFullPath($env:APPDATA)',
  "$trashRoot = [IO.Path]::Combine($env:USERPROFILE, '.ccs', 'trash', 'claude')",
  "$helper = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude', 'ccs-claude.exe')",
  'function Fail([int]$code) { exit $code }',
  "function Check-Profile([string]$path) { $full = [IO.Path]::GetFullPath($path); if ([IO.Path]::GetDirectoryName($full) -ne $support -or -not [IO.Path]::GetFileName($full).StartsWith('Claude')) { Fail 1 }; return $full }",
  "function Check-Link([string]$path) { $full = [IO.Path]::GetFullPath($path); $dir = [IO.Path]::GetDirectoryName($full); $name = [IO.Path]::GetFileName($full); if (($dir -ne [Environment]::GetFolderPath('Desktop') -and $dir -ne [Environment]::GetFolderPath('Programs')) -or -not $name.StartsWith('Claude (') -or -not $name.EndsWith(').lnk')) { Fail 1 }; return $full }",
  "function Claude-Exe { $package = Get-AppxPackage -Name 'Claude' | Sort-Object Version -Descending | Select-Object -First 1; if (-not $package) { Fail 3 }; $exe = Join-Path $package.InstallLocation 'app\\Claude.exe'; if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { Fail 3 }; return $exe }",
  "function Make-Links([string[]]$links, [string]$profile) { $exe = Claude-Exe; $shell = New-Object -ComObject WScript.Shell; foreach ($link in $links) { $s = $shell.CreateShortcut($link); $s.TargetPath = $exe; $s.Arguments = '--user-data-dir=\"' + $profile + '\"'; $s.WorkingDirectory = Split-Path -Parent $exe; $s.IconLocation = $exe + ',0'; $s.Save() } }",
  "function Make-Task([string]$id) { $action = New-ScheduledTaskAction -Execute $helper -Argument ('ccs-claude://launch/' + $id); $principal = New-ScheduledTaskPrincipal -UserId ($env:USERDOMAIN + '\\' + $env:USERNAME) -LogonType Interactive -RunLevel Limited; Register-ScheduledTask -TaskPath '\\' -TaskName ('ccs-claude-' + $id) -Action $action -Principal $principal -Settings (New-ScheduledTaskSettingsSet) | Out-Null }",
  "function Drop-Task([string]$id) { Get-ScheduledTask -TaskPath '\\' -TaskName ('ccs-claude-' + $id) -ErrorAction SilentlyContinue | Unregister-ScheduledTask -Confirm:$false }",
  'function Done($value) { [Console]::Write((ConvertTo-Json -Compress $value)); exit 0 }',
];

export function windowsHostScript(
  op: 'create' | 'undo' | 'state' | 'trash' | 'restore' | 'purge',
  values: { profileId?: string; trashName?: string; launcher?: ClaudeHostLauncher }
): string {
  const lines = [...WINDOWS_COMMON];
  if (values.profileId !== undefined) {
    if (!CLAUDE_PROFILE_ID.test(values.profileId)) throw new ValidationError('Invalid profile id.');
    lines.push(`$id = '${values.profileId}'`);
  }
  if (values.trashName !== undefined) {
    if (!TRASH_NAME.test(values.trashName)) throw new ValidationError('Invalid trash name.');
    lines.push(`$target = Join-Path $trashRoot '${values.trashName}'`);
  }
  if (values.launcher) {
    lines.push(`$profile = Check-Profile ${psValue(values.launcher.profilePath)}`);
    const links = [values.launcher.launcherPath, values.launcher.startMenuPath].filter(
      (link): link is string => typeof link === 'string'
    );
    lines.push(`$links = @(${links.map((link) => `(Check-Link ${psValue(link)})`).join(', ')})`);
  }
  switch (op) {
    case 'create':
      lines.push(
        "$profile = Join-Path $support ('Claude-' + $id)",
        "$links = @((Join-Path ([Environment]::GetFolderPath('Desktop')) ('Claude (' + $id + ').lnk')), (Join-Path ([Environment]::GetFolderPath('Programs')) ('Claude (' + $id + ').lnk')))",
        'if (Test-Path -LiteralPath $profile) { Fail 3 }; foreach ($link in $links) { if (Test-Path -LiteralPath $link) { Fail 3 } }',
        "if (Get-ScheduledTask -TaskPath '\\' -TaskName ('ccs-claude-' + $id) -ErrorAction SilentlyContinue) { Fail 3 }",
        'New-Item -ItemType Directory -Path $profile | Out-Null',
        'try { Make-Links $links $profile; Make-Task $id } catch { foreach ($link in $links) { Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue }; Drop-Task $id; Remove-Item -LiteralPath $profile -Force -ErrorAction SilentlyContinue; Fail 1 }',
        "Done ([ordered]@{ launcherName = 'Claude (' + $id + ')'; launcherPath = $links[0]; startMenuPath = $links[1]; profilePath = $profile })"
      );
      break;
    case 'undo':
      lines.push(
        'foreach ($link in $links) { Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue }',
        'Drop-Task $id',
        'if ((Test-Path -LiteralPath $profile) -and @(Get-ChildItem -LiteralPath $profile -Force).Count -eq 0) { Remove-Item -LiteralPath $profile -Force }',
        'Done @{ ok = $true }'
      );
      break;
    case 'state':
      lines.push(
        "$needle = '--user-data-dir=\"' + $profile + '\"'",
        '$found = @(Get-CimInstance Win32_Process -Filter "Name = \'claude.exe\'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($needle) })',
        'Done @{ running = ($found.Count -gt 0) }'
      );
      break;
    case 'trash':
      lines.push(
        'New-Item -ItemType Directory -Path $trashRoot -Force | Out-Null',
        'if (Test-Path -LiteralPath $target) { Fail 1 }',
        "$moved = $false; if (Test-Path -LiteralPath $profile) { if ([IO.Path]::GetPathRoot($profile) -ne [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($trashRoot))) { Done @{ result = 'cross_volume' } }; [IO.Directory]::Move($profile, $target); $moved = $true } else { New-Item -ItemType Directory -Path $target | Out-Null }",
        'try { foreach ($link in $links) { Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue }; Drop-Task $id } catch { if ($moved) { [IO.Directory]::Move($target, $profile) }; Fail 1 }',
        "Done @{ result = 'moved' }"
      );
      break;
    case 'restore':
      lines.push(
        'if (-not (Test-Path -LiteralPath $target -PathType Container) -or (Test-Path -LiteralPath $profile)) { Fail 3 }',
        '[IO.Directory]::Move($target, $profile)',
        'Make-Links $links $profile; Drop-Task $id; Make-Task $id',
        'Done @{ ok = $true }'
      );
      break;
    case 'purge':
      lines.push(
        'if (Test-Path -LiteralPath $target) { if ((Get-Item -LiteralPath $target -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { Fail 1 }; Remove-Item -LiteralPath $target -Recurse -Force }',
        'Done @{ ok = $true }'
      );
      break;
  }
  return lines.join('\n');
}

function reply(stdout: string): Record<string, unknown> {
  try {
    const value = JSON.parse(stdout.trim()) as unknown;
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      return value as Record<string, unknown>;
    }
  } catch {
    /* Fall through. */
  }
  throw new ClaudeHostError();
}

function checkStep(launcher: ClaudeHostLauncher): ClaudeHostLauncher {
  const checked = parseLauncher(launcher);
  if (!checked) throw new ValidationError('Claude launcher record is not valid.');
  return checked;
}

/** The production transport: one ssh call per host step. */
export class SshClaudeHostTransport implements ClaudeHostTransport {
  constructor(private readonly run: ClaudeHostRunner = runClaudeHostOverSsh) {}

  private async step(
    host: ClaudeHost,
    sshHost: string,
    request: Record<string, unknown>,
    windows: string
  ): Promise<Record<string, unknown>> {
    if (host === 'mac') {
      const input = JSON.stringify({ program: MAC_HOST_PROGRAM, request });
      return reply(await this.run(sshHost, `/usr/bin/python3 -c "${MAC_BOOTSTRAP}"`, input));
    }
    const command = `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${WINDOWS_BOOTSTRAP}`;
    return reply(await this.run(sshHost, command, windows));
  }

  async create(host: ClaudeHost, input: { profileId: string; sshHost: string }) {
    const value = await this.step(
      host,
      input.sshHost,
      { op: 'create', profileId: input.profileId },
      windowsHostScript('create', { profileId: input.profileId })
    );
    const launcher = parseLauncher({ ...value, sshHost: input.sshHost });
    if (!launcher) throw new ClaudeHostError();
    return launcher;
  }

  async undoCreate(host: ClaudeHost, input: ClaudeHostStep) {
    const launcher = checkStep(input.launcher);
    await this.step(
      host,
      launcher.sshHost,
      { op: 'undo', profileId: input.profileId, ...launcher },
      windowsHostScript('undo', { profileId: input.profileId, launcher })
    );
  }

  async appState(host: ClaudeHost, input: { launcher: ClaudeHostLauncher }) {
    const launcher = checkStep(input.launcher);
    const value = await this.step(
      host,
      launcher.sshHost,
      { op: 'state', ...launcher },
      windowsHostScript('state', { launcher })
    );
    if (typeof value.running !== 'boolean') throw new ClaudeHostError();
    return value.running ? 'running' : 'stopped';
  }

  async trash(host: ClaudeHost, input: ClaudeHostStep & { trashName: string }) {
    const launcher = checkStep(input.launcher);
    const value = await this.step(
      host,
      launcher.sshHost,
      { op: 'trash', profileId: input.profileId, trashName: input.trashName, ...launcher },
      windowsHostScript('trash', {
        profileId: input.profileId,
        trashName: input.trashName,
        launcher,
      })
    );
    if (value.result !== 'moved' && value.result !== 'cross_volume') throw new ClaudeHostError();
    return value.result;
  }

  async restore(host: ClaudeHost, input: ClaudeHostStep & { trashName: string }) {
    const launcher = checkStep(input.launcher);
    await this.step(
      host,
      launcher.sshHost,
      { op: 'restore', profileId: input.profileId, trashName: input.trashName, ...launcher },
      windowsHostScript('restore', {
        profileId: input.profileId,
        trashName: input.trashName,
        launcher,
      })
    );
  }

  async purge(host: ClaudeHost, input: { sshHost: string; trashName: string }) {
    await this.step(
      host,
      input.sshHost,
      { op: 'purge', trashName: input.trashName },
      windowsHostScript('purge', { trashName: input.trashName })
    );
  }
}
