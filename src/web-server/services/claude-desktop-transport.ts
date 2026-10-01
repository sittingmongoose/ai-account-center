import { execFile } from 'child_process';
import path from 'path';
import { NetworkError, ValidationError } from '../../errors/error-types';
import type { ClaudeDesktopLauncher } from './claude-desktop-profile-service';

const SSH_TIMEOUT_MS = 8000;
const MAX_HISTORY_BYTES = 1024 * 1024;
const MISSING_HISTORY = 'CCS_DESKTOP_USAGE_MISSING';

export class ClaudeDesktopTransportError extends NetworkError {
  readonly timedOut: boolean;

  constructor(timedOut = false) {
    super(timedOut ? 'Claude desktop request timed out.' : 'Claude desktop request failed.');
    this.timedOut = timedOut;
  }
}

function quoteShell(value: string): string {
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}

function checkHost(host: string | undefined): string {
  if (!host || host.length > 128 || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(host)) {
    throw new ValidationError('Claude desktop SSH alias is not configured safely.');
  }
  return host;
}

function checkPath(value: string | undefined, platform: 'mac' | 'windows'): string {
  if (
    !value ||
    value.length > 4096 ||
    /[\u0000-\u001f\u007f]/.test(value) ||
    !(platform === 'mac' ? path.posix.isAbsolute(value) : path.win32.isAbsolute(value))
  ) {
    throw new ValidationError('Claude desktop path is not configured safely.');
  }
  return value;
}

/** Only generated commands and a validated alias are passed to SSH; no client input is accepted. */
async function runDesktopSsh(host: string, command: string): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
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
        checkHost(host),
        command,
      ],
      {
        encoding: 'utf8',
        timeout: SSH_TIMEOUT_MS,
        maxBuffer: MAX_HISTORY_BYTES,
        windowsHide: true,
      },
      (error, stdout) => {
        if (error) {
          reject(
            new ClaudeDesktopTransportError(error.killed === true && error.signal === 'SIGTERM')
          );
          return;
        }
        resolve(stdout);
      }
    );
  });
}

/** Open the configured named wrapper in the console user's GUI session, keeping existing apps. */
export async function openClaudeMacLauncher(launcher: ClaudeDesktopLauncher): Promise<void> {
  const launcherPath = checkPath(launcher.launcherPath, 'mac');
  if (!launcherPath.endsWith('.app')) {
    throw new ValidationError('Claude desktop launcher must be a configured application.');
  }
  const app = quoteShell(launcherPath);
  const command = [
    `test -d ${app} || exit 1`,
    `console_user=$(/usr/bin/stat -f '%Su' /dev/console)`,
    `test "$console_user" != root && test "$console_user" != loginwindow || exit 1`,
    `console_uid=$(/usr/bin/id -u "$console_user") || exit 1`,
    `current_uid=$(/usr/bin/id -u) || exit 1`,
    `test "$console_uid" = "$current_uid" || exit 1`,
    `exec /usr/bin/open ${app}`,
  ].join('\n');
  await runDesktopSsh(checkHost(launcher.sshHost), command);
}

/** Read only the desktop's fixed usage history filename, never its credential or organization stores. */
export async function readClaudeDesktopUsageHistory(
  launcher: ClaudeDesktopLauncher,
  platform: 'mac' | 'windows'
): Promise<string | null> {
  const profilePath = checkPath(launcher.profilePath, platform);
  const historyPath =
    platform === 'mac'
      ? path.posix.join(profilePath, 'plan-usage-history.json')
      : path.win32.join(profilePath, 'plan-usage-history.json');

  let command: string;
  if (platform === 'mac') {
    const history = quoteShell(historyPath);
    command = [
      `if [ ! -f ${history} ]; then printf '%s' ${quoteShell(MISSING_HISTORY)}; exit 0; fi`,
      `history_size=$(/usr/bin/stat -f '%z' ${history}) || exit 1`,
      `test "$history_size" -le ${MAX_HISTORY_BYTES} || exit 1`,
      `/bin/cat ${history}`,
    ].join('\n');
  } else {
    const history = `'${historyPath.replace(/'/g, "''")}'`;
    const script = [
      "$ErrorActionPreference = 'Stop'",
      `$history = ${history}`,
      `if (-not [IO.File]::Exists($history)) { [Console]::Write('${MISSING_HISTORY}'); exit 0 }`,
      `if ((Get-Item -LiteralPath $history).Length -gt ${MAX_HISTORY_BYTES}) { exit 1 }`,
      '[Console]::Write([IO.File]::ReadAllText($history))',
    ].join('; ');
    command = `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
  }

  const contents = await runDesktopSsh(checkHost(launcher.sshHost), command);
  return contents.trim() === MISSING_HISTORY ? null : contents;
}
