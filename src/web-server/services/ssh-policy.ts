/** One SSH/SCP policy: preserve connection config, override trust and forwarding. */
import os from 'os';
import path from 'path';
import fs from 'fs';
import { execFile } from 'child_process';
import { loadUnifiedConfig } from '../../config/unified-config-loader';
import { ConfigError } from '../../errors/error-types';

export interface SshKeepalive {
  interval: number;
  countMax: number;
}

/** Single-value command-line options take precedence over user/system config. */
export const SSH_ISOLATION = [
  'BatchMode=yes',
  'ConnectTimeout=5',
  'ConnectionAttempts=1',
  'StrictHostKeyChecking=yes',
  'UpdateHostKeys=no',
  'ForwardAgent=no',
  'ForwardX11=no',
  'GlobalKnownHostsFile=none',
  'PermitLocalCommand=no',
  'ClearAllForwardings=yes',
  'ControlMaster=no',
  'ControlPath=none',
].flatMap((value) => ['-o', value]);

/** Preserve local agent authentication; ForwardAgent=no prevents its remote export. */
export function sshEnvironment(
  env: NodeJS.ProcessEnv = process.env,
  windows = process.platform === 'win32'
): Record<string, string> {
  const allowed = new Set([
    'PATH',
    'HOME',
    'USER',
    'LOGNAME',
    'LANG',
    'SSH_AUTH_SOCK',
    ...(windows ? ['SYSTEMROOT', 'USERPROFILE', 'WINDIR'] : []),
  ]);
  return Object.fromEntries(
    Object.entries(env).filter(
      ([name, value]) => value !== undefined && allowed.has(windows ? name.toUpperCase() : name)
    )
  ) as Record<string, string>;
}

const probes = new Map<string, Promise<string[]>>();
const warned = new Set<string>();
/** Bounded, coalesced probe. -F none keeps Match exec and live hosts out of capability checks. */
async function capabilities(): Promise<string[]> {
  const env = sshEnvironment();
  const searchPath = Object.entries(env).find(([name]) => name.toUpperCase() === 'PATH')?.[1] ?? '';
  let executable: string | undefined;
  for (const directory of searchPath.split(path.delimiter)) {
    const candidate = path.resolve(directory, process.platform === 'win32' ? 'ssh.exe' : 'ssh');
    try {
      await fs.promises.access(candidate, fs.constants.X_OK);
      executable = await fs.promises.realpath(candidate);
      break;
    } catch {
      /* Continue through PATH without executing a helper. */
    }
  }
  if (!executable) throw new ConfigError('SSH executable unavailable');
  const key = executable;
  let pending = probes.get(key);
  if (!pending) {
    pending = (async () => {
      const args: string[] = [];
      for (const option of ['KnownHostsCommand=none', 'SetEnv=AAC_SSH_POLICY=1']) {
        const supported = await new Promise<boolean>((resolve, reject) => {
          execFile(
            key,
            ['-G', '-F', 'none', '-o', option, '--', 'localhost'],
            {
              env,
              encoding: 'utf8',
              timeout: 2000,
              maxBuffer: 128 * 1024,
              windowsHide: true,
            },
            (error, _stdout, stderr) => {
              if (!error) resolve(true);
              else if (/Bad configuration option:/i.test(String(stderr))) resolve(false);
              else reject(new ConfigError('SSH capability probe failed'));
            }
          );
        });
        if (supported) args.push('-o', option);
        else if (!warned.has(key + option)) {
          warned.add(key + option);
          console.warn(`[AAC SSH] ${option.split('=')[0]} unsupported; compatibility downgrade`);
        }
      }
      return args;
    })();
    // A failed probe stays cached too: callers never repeatedly pause/retry discovery.
    probes.set(key, pending);
  }
  return pending;
}

const expandHome = (file: string, home: string) =>
  file.startsWith('~/') ? path.join(home, file.slice(2)) : file;

/** Explicit AAC fields override config; otherwise OpenSSH retains every identity/jump/match. */
export function sshConnectionOptions(alias: string): string[] {
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(alias)) throw new ConfigError('Invalid SSH alias');
  const home = process.env.CCS_HOME ?? os.homedir();
  const config = loadUnifiedConfig();
  const definition = Object.entries(config?.computers ?? {}).find(
    ([id, row]) => row && (id === alias || row.id === alias || row.sshAlias === alias)
  )?.[1];
  const explicit = definition?.transport;
  const identity =
    explicit?.key || process.env.AAC_AGENT_IDENTITY_FILE || config?.agentTransportSsh?.identityFile;
  const pin =
    process.env.AAC_AGENT_KNOWN_HOSTS_FILE ??
    config?.agentTransportSsh?.knownHostsFile ??
    path.join(home, '.ssh/known_hosts');
  const optionPath = (file: string) => {
    const expanded = expandHome(file, home);
    const value = process.platform === 'win32' ? expanded.replace(/\\/g, '/') : expanded;
    if (!path.isAbsolute(expanded) || /[\r\n%"\\]/.test(value))
      throw new ConfigError('SSH paths must be explicit absolute paths');
    return value;
  };
  const args = ['-o', `UserKnownHostsFile="${optionPath(pin)}"`];
  if (explicit?.host) {
    if (!/^[A-Za-z0-9.:_-]+$/.test(explicit.host) || explicit.host.startsWith('-'))
      throw new ConfigError('Invalid SSH hostname');
    args.push('-o', `Hostname=${explicit.host}`);
  }
  if (explicit?.user) {
    if (!/^[A-Za-z0-9_.-]+$/.test(explicit.user)) throw new ConfigError('Invalid SSH user');
    args.push('-o', `User=${explicit.user}`);
  }
  if (explicit?.port !== undefined) {
    if (!Number.isInteger(explicit.port) || explicit.port < 1 || explicit.port > 65535)
      throw new ConfigError('Invalid SSH port');
    args.push('-o', `Port=${explicit.port}`);
  }
  if (identity) args.push('-i', optionPath(identity), '-o', 'IdentitiesOnly=yes');
  return args;
}

/** The fixture home selects fixture config; production leaves the complete SSH config intact. */
export async function sshOptions(host: string): Promise<string[]> {
  const connection = sshConnectionOptions(host);
  const fixtureConfig = process.env.CCS_HOME
    ? ['-F', path.join(process.env.CCS_HOME, '.ssh/config')]
    : [];
  // A missing fixture config must not fall through to the user's real home.
  if (fixtureConfig.length) {
    if (!fs.existsSync(fixtureConfig[1] ?? 'none')) fixtureConfig[1] = 'none';
  }
  return [...SSH_ISOLATION, ...(await capabilities()), ...connection, ...fixtureConfig];
}

/** One remote command argument; the caller owns quoting and its process deadline. */
export async function sshArgv(
  host: string,
  command: string,
  keepalive?: SshKeepalive
): Promise<string[]> {
  return [
    '-T',
    ...(await sshOptions(host)),
    ...(keepalive
      ? [
          '-o',
          `ServerAliveInterval=${keepalive.interval}`,
          '-o',
          `ServerAliveCountMax=${keepalive.countMax}`,
        ]
      : []),
    '--',
    host,
    command,
  ];
}
