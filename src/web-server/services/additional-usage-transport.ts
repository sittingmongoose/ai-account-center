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

/**
 * How a registry v2 account finds its credential (CONTRACT-registry-lifecycle 3.1).
 * Only ids reach the collector; it derives every path from fixed directories.
 */
export type CollectorCredential =
  | { kind: 'discover' }
  | { kind: 'aac-key'; keyId: string }
  | { kind: 'browser-capsule'; capsuleId: string }
  | { kind: 'config-home'; homeId: string }
  | { kind: 'antigravity-profile'; profileId: string };

/** A registry v2 account behind a source; absent for version 1 manifest sources. */
export interface AdditionalUsageAccount {
  id: string;
  label: string | null;
  credential: CollectorCredential;
}

export interface AdditionalUsageSource {
  provider: AdditionalProvider;
  platform: DashboardPlatform;
  sshHost?: string;
  account?: AdditionalUsageAccount;
}

const PROCESS_TIMEOUT_MS = 25_000;
const MAX_STDOUT_BYTES = 64 * 1024;

export class AdditionalUsageTransportError extends NetworkError {
  /**
   * The helper rejected the account arguments (exit status 2, argparse usage
   * error, or a missing helper file): the host still runs an older helper.
   */
  readonly helperOutdated: boolean;

  constructor(
    readonly timedOut = false,
    helperOutdated = false
  ) {
    super(timedOut ? 'Account usage request timed out.' : 'Account usage request failed.');
    this.name = 'AdditionalUsageTransportError';
    this.helperOutdated = helperOutdated;
  }
}

const HEX8 = /^[a-f0-9]{8}$/;
const CAPSULE_ID = /^(?:default|[a-f0-9]{8})$/;
const ANTIGRAVITY_PROFILE_ID = /^[a-z][a-z0-9_-]{0,47}$/;

/** Registry v2 ids: `<provider>:usage` or `<provider>:acct:<8 lowercase hex>`. */
export function isAdditionalAccountId(provider: AdditionalProvider, value: unknown): boolean {
  return (
    typeof value === 'string' &&
    (value === `${provider}:usage` ||
      (value.startsWith(`${provider}:acct:`) && HEX8.test(value.slice(provider.length + 6))))
  );
}

export function isCollectorCredential(value: unknown): value is CollectorCredential {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const credential = value as Record<string, unknown>;
  const keys = Object.keys(credential).sort().join(',');
  switch (credential.kind) {
    case 'discover':
      return keys === 'kind';
    case 'aac-key':
      return (
        keys === 'keyId,kind' && typeof credential.keyId === 'string' && HEX8.test(credential.keyId)
      );
    case 'browser-capsule':
      return (
        keys === 'capsuleId,kind' &&
        typeof credential.capsuleId === 'string' &&
        CAPSULE_ID.test(credential.capsuleId)
      );
    case 'config-home':
      return (
        keys === 'homeId,kind' &&
        typeof credential.homeId === 'string' &&
        HEX8.test(credential.homeId)
      );
    case 'antigravity-profile':
      return (
        keys === 'kind,profileId' &&
        typeof credential.profileId === 'string' &&
        ANTIGRAVITY_PROFILE_ID.test(credential.profileId)
      );
    default:
      return false;
  }
}

/**
 * Credential kinds the usage helpers read in this release. A `discover`
 * account keeps today's call exactly (no account arguments), so hosts with an
 * older helper keep working; `config-home` and `antigravity-profile` accounts
 * are not read by a helper yet.
 */
export function isCollectableSource(source: AdditionalUsageSource): boolean {
  const kind = source.account?.credential.kind ?? 'discover';
  return (
    kind === 'discover' ||
    (kind === 'aac-key' && ['kimi-code', 'zai', 'opencode-go'].includes(source.provider)) ||
    (kind === 'browser-capsule' && source.provider === 'qwen')
  );
}

/** Enumerated account arguments; empty for version 1 sources and `discover` accounts. */
export function collectorAccountArguments(source: AdditionalUsageSource): string[] {
  const account = source.account;
  if (!account || account.credential.kind === 'discover') return [];
  const credential = account.credential;
  const extra =
    credential.kind === 'aac-key'
      ? ['--key-id', credential.keyId]
      : credential.kind === 'browser-capsule'
        ? ['--capsule-id', credential.capsuleId]
        : credential.kind === 'config-home'
          ? ['--home-id', credential.homeId]
          : [];
  return ['--account', account.id, '--credential', credential.kind, ...extra];
}

function isValidSourceAccount(source: AdditionalUsageSource): boolean {
  const account = source.account;
  if (account === undefined) return true;
  return (
    !!account &&
    isAdditionalAccountId(source.provider, account.id) &&
    isCollectorCredential(account.credential) &&
    isCollectableSource(source)
  );
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
  // Every value is enumerated or matches a fixed pattern without quotes or spaces.
  const accountArguments = collectorAccountArguments(source)
    .map((value) => (value.startsWith('--') ? ` ${value}` : ` '${value}'`))
    .join('');
  if (source.platform === 'windows') {
    const script = [
      "$ErrorActionPreference = 'Stop'",
      "$env:PYTHONIOENCODING = 'utf-8'",
      "$env:PYTHONUTF8 = '1'",
      `$helper = [IO.Path]::Combine($HOME, '.ccs', 'account-usage', '${helper}')`,
      `& python.exe $helper --provider '${source.provider}' --platform 'windows'${accountArguments}`,
      'exit $LASTEXITCODE',
    ].join('; ');
    return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
  }
  return `/usr/bin/python3 "$HOME/.ccs/account-usage/${helper}" --provider '${source.provider}' --platform '${source.platform}'${accountArguments}`;
}

/** No helper paths, hosts or command fragments are taken from dashboard requests. */
export async function runAdditionalUsageSource(source: AdditionalUsageSource): Promise<string> {
  if (
    !isAdditionalProvider(source.provider) ||
    !['ubuntu', 'mac', 'windows'].includes(source.platform) ||
    (source.sshHost !== undefined && !isSafeUsageSshAlias(source.sshHost)) ||
    !isValidSourceAccount(source)
  ) {
    throw new AdditionalUsageTransportError();
  }
  const accountArguments = collectorAccountArguments(source);
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
        ...accountArguments,
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
            new AdditionalUsageTransportError(
              error?.killed === true && error.signal === 'SIGTERM',
              // An older helper exits 2 on the unknown account arguments.
              accountArguments.length > 0 && error?.killed !== true && error?.code === 2
            )
          );
          return;
        }
        resolve(stdout);
      }
    );
  });
}
