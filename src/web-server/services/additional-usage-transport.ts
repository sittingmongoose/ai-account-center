import { execFile } from 'child_process';
import path from 'path';
import { NetworkError } from '../../errors/error-types';
import type { DashboardPlatform } from './account-dashboard-types';

export const ADDITIONAL_PROVIDERS = [
  'antigravity',
  'muse',
  'cursor',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
] as const;
export type AdditionalProvider = (typeof ADDITIONAL_PROVIDERS)[number];

export interface AdditionalUsageSource {
  provider: AdditionalProvider;
  platform: DashboardPlatform;
  sshHost?: string;
}

const PROCESS_TIMEOUT_MS = 25_000;
const MAX_STDOUT_BYTES = 64 * 1024;

export class AdditionalUsageTransportError extends NetworkError {
  constructor(readonly timedOut = false) {
    super(timedOut ? 'Account usage request timed out.' : 'Account usage request failed.');
    this.name = 'AdditionalUsageTransportError';
  }
}

export function isAdditionalProvider(value: unknown): value is AdditionalProvider {
  return typeof value === 'string' && (ADDITIONAL_PROVIDERS as readonly string[]).includes(value);
}

export function isSafeUsageSshAlias(value: unknown): value is string {
  return (
    typeof value === 'string' && value.length <= 128 && /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(value)
  );
}

function helperName(provider: AdditionalProvider): string {
  return ['antigravity', 'muse', 'cursor'].includes(provider)
    ? 'desktop_usage.py'
    : 'plan_usage.py';
}

/** Remote commands contain only fixed paths and enumerated arguments. */
function remoteCommand(source: AdditionalUsageSource): string {
  const helper = helperName(source.provider);
  if (source.platform === 'windows') {
    const script = [
      "$ErrorActionPreference = 'Stop'",
      "$env:PYTHONIOENCODING = 'utf-8'",
      "$env:PYTHONUTF8 = '1'",
      `$helper = [IO.Path]::Combine($HOME, '.ccs', 'account-usage', '${helper}')`,
      `& python.exe $helper --provider '${source.provider}' --platform 'windows'`,
      'exit $LASTEXITCODE',
    ].join('; ');
    return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
  }
  return `/usr/bin/python3 "$HOME/.ccs/account-usage/${helper}" --provider '${source.provider}' --platform '${source.platform}'`;
}

/** No helper paths, hosts or command fragments are taken from dashboard requests. */
export async function runAdditionalUsageSource(source: AdditionalUsageSource): Promise<string> {
  if (
    !isAdditionalProvider(source.provider) ||
    !['ubuntu', 'mac', 'windows'].includes(source.platform) ||
    (source.sshHost !== undefined && !isSafeUsageSshAlias(source.sshHost))
  ) {
    throw new AdditionalUsageTransportError();
  }
  const localPlatform: DashboardPlatform =
    process.platform === 'win32' ? 'windows' : process.platform === 'darwin' ? 'mac' : 'ubuntu';
  if (!source.sshHost && source.platform !== localPlatform) {
    throw new AdditionalUsageTransportError();
  }
  const binary = source.sshHost
    ? 'ssh'
    : source.platform === 'windows'
      ? 'python.exe'
      : '/usr/bin/python3';
  const args = source.sshHost
    ? [
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
        source.sshHost,
        remoteCommand(source),
      ]
    : [
        path.resolve(__dirname, '../../../scripts/account-usage', helperName(source.provider)),
        '--provider',
        source.provider,
        '--platform',
        source.platform,
      ];
  return new Promise((resolve, reject) => {
    execFile(
      binary,
      args,
      {
        encoding: 'utf8',
        timeout: PROCESS_TIMEOUT_MS,
        maxBuffer: MAX_STDOUT_BYTES,
        windowsHide: true,
        env: { ...process.env, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' },
      },
      (error, stdout) => {
        if (error || Buffer.byteLength(stdout, 'utf8') > MAX_STDOUT_BYTES) {
          reject(
            new AdditionalUsageTransportError(error?.killed === true && error.signal === 'SIGTERM')
          );
          return;
        }
        resolve(stdout);
      }
    );
  });
}
