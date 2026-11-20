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
  'VerifyHostKeyDNS=no',
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

/** Capabilities belong to one executable file, not its pathname: same-path upgrades re-probe. */
const probes = new Map<
  string,
  { identity: string; pending: Promise<string[]>; failedAt?: number }
>();
/** A failed probe is retried after this, so a recovered client is not refused until restart. */
const PROBE_FAILURE_TTL_MS = 30_000;
const CAPABILITY_OPTIONS = ['KnownHostsCommand=none', 'SetEnv=AAC_SSH_POLICY=1'];
const warned = new Set<string>();
async function executableIdentity(candidate: string): Promise<{ file: string; identity: string }> {
  const file = await fs.promises.realpath(candidate);
  // dev/ino are the volume serial and file index on Windows; bigint keeps them exact.
  const info = await fs.promises.stat(file, { bigint: true });
  if (!info.isFile()) throw new ConfigError('SSH executable unavailable');
  return {
    file,
    identity: [file, info.dev, info.ino, info.size, info.mtimeNs, info.ctimeNs].join('\0'),
  };
}
function probe(
  executable: string,
  env: Record<string, string>,
  execute: typeof execFile
): Promise<string[]> {
  return (async () => {
    const args: string[] = [];
    for (const option of CAPABILITY_OPTIONS) {
      const supported = await new Promise<boolean>((resolve, reject) => {
        execute(
          executable,
          ['-G', '-F', 'none', '-o', option, '--', 'localhost'],
          {
            env,
            encoding: 'utf8',
            timeout: 2000,
            killSignal: 'SIGKILL',
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
      else if (!warned.has(executable + option)) {
        warned.add(executable + option);
        console.warn(`[AAC SSH] ${option.split('=')[0]} unsupported; compatibility downgrade`);
      }
    }
    return args;
  })();
}
/**
 * Bounded, coalesced probe. -F none keeps Match exec and live hosts out of capability checks.
 * Returns the PATH-resolved file whose identity was probed, checked again just before return.
 */
async function capabilities(
  execute: typeof execFile = execFile
): Promise<{ file: string; args: string[] }> {
  const cache = execute === execFile ? probes : undefined;
  const env = sshEnvironment();
  const searchPath = Object.entries(env).find(([name]) => name.toUpperCase() === 'PATH')?.[1] ?? '';
  let executable: string | undefined;
  for (const directory of searchPath.split(path.delimiter)) {
    const candidate = path.resolve(directory, process.platform === 'win32' ? 'ssh.exe' : 'ssh');
    try {
      await fs.promises.access(candidate, fs.constants.X_OK);
      executable = candidate;
      break;
    } catch {
      /* Continue through PATH without executing a helper. */
    }
  }
  if (!executable) throw new ConfigError('SSH executable unavailable');
  // A file replaced while its probe runs is probed again; the result must match the file now there.
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const current = await executableIdentity(executable).catch(() => {
      throw new ConfigError('SSH executable unavailable');
    });
    let entry = cache?.get(current.file);
    if (
      entry?.identity !== current.identity ||
      (entry.failedAt !== undefined && Date.now() - entry.failedAt >= PROBE_FAILURE_TTL_MS)
    ) {
      // Success is cached per identity; a failure only for PROBE_FAILURE_TTL_MS.
      const fresh: { identity: string; pending: Promise<string[]>; failedAt?: number } = {
        identity: current.identity,
        pending: probe(current.file, env, execute),
      };
      fresh.pending.catch(() => {
        fresh.failedAt = Date.now();
      });
      entry = fresh;
      cache?.set(current.file, entry);
    }
    const result = await entry.pending;
    const after = await executableIdentity(executable).catch(() => undefined);
    if (after?.identity === current.identity) return { file: executable, args: result };
  }
  throw new ConfigError('SSH executable changed during capability probe');
}

/**
 * The program to spawn for `file`, checked immediately before the spawn: for ssh the exact
 * probed file, for scp its -S file. The argv's capability options must match that file's
 * current probe; a client replaced after argv was built is refused, never launched. Other
 * programs pass through. Residual: a swap between this check and exec, by someone who can
 * already replace the user's ssh, is not detected.
 */
export async function sshLaunch(
  file: string,
  args: readonly string[],
  execute?: typeof execFile
): Promise<string> {
  if (file !== 'ssh' && file !== 'scp') return file;
  const current = await capabilities(execute);
  const flag = args.indexOf('-S');
  if (
    (file === 'scp' && (flag < 0 || args[flag + 1] !== current.file)) ||
    CAPABILITY_OPTIONS.some((option) => current.args.includes(option) !== args.includes(option))
  )
    throw new ConfigError('SSH executable changed before launch');
  return file === 'scp' ? 'scp' : current.file;
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

/** Keep both user and system connection config; only the capability probe uses -F none. */
export async function sshOptions(host: string, execute?: typeof execFile): Promise<string[]> {
  const connection = sshConnectionOptions(host);
  return [...SSH_ISOLATION, ...(await capabilities(execute)).args, ...connection];
}

/**
 * SCP runs the exact SSH file whose identity was probed (-S), with the same policy options,
 * never scp's own built-in or PATH ssh, whose capabilities nobody checked.
 */
export async function scpArgv(
  host: string,
  source: string,
  destination: string
): Promise<string[]> {
  const connection = sshConnectionOptions(host);
  const { file, args } = await capabilities();
  return ['-q', '-S', file, ...SSH_ISOLATION, ...args, ...connection, '--', source, destination];
}

/** One remote command argument; the caller owns quoting and its process deadline. */
export async function sshArgv(
  host: string,
  command: string,
  keepalive?: SshKeepalive,
  execute?: typeof execFile
): Promise<string[]> {
  return [
    '-T',
    ...(await sshOptions(host, execute)),
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
