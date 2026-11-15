/** AAC SSH uses isolated options and explicit connection settings; never inherited transport policy. */
/** The keepalive pair; omit it for jobs that run without one (helper sync). */
export interface SshKeepalive {
  interval: number;
  countMax: number;
}

import os from 'os';
import path from 'path';
import fs from 'fs';
import { execFileSync } from 'child_process';
import { loadUnifiedConfig } from '../../config/unified-config-loader';
import { ConfigError } from '../../errors/error-types';

/** No inherited forwarding, commands, environment or alternate host-key sources. Shared with SCP. */
export const SSH_ISOLATION = [
  '-F',
  'none',
  ...[
    'BatchMode=yes',
    'ConnectTimeout=5',
    'ConnectionAttempts=1',
    'StrictHostKeyChecking=yes',
    'UpdateHostKeys=no',
    'IdentitiesOnly=yes',
    'ForwardAgent=no',
    'GlobalKnownHostsFile=none',
    'KnownHostsCommand=none',
    'PermitLocalCommand=no',
    'ClearAllForwardings=yes',
  ].flatMap((value) => ['-o', value]),
];

const expandHome = (file: string, home: string) =>
  file.startsWith('~/') ? path.join(home, file.slice(2)) : file;

const resolved = new Map<string, { until: number; args: string[] }>();
/** Connection discovery is read-only. -G never connects; only these four output fields survive.
 * A private CCS_HOME supplies the fixture home in tests; no real SSH home is opened there.
 * OpenSSH may evaluate Match exec during discovery, as the legacy SSH path did.
 */
export function sshConnectionOptions(alias: string): string[] {
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(alias)) throw new ConfigError('Invalid SSH alias');
  const home = process.env.CCS_HOME ?? os.homedir();
  const config = loadUnifiedConfig();
  const rows = config?.computers;
  const values = rows && typeof rows === 'object' ? Object.entries(rows) : [];
  const definition = values.find(([id, row]) => {
    if (!row || typeof row !== 'object') return false;
    const entry = row as { id?: string; sshAlias?: string };
    return id === alias || entry.id === alias || entry.sshAlias === alias;
  })?.[1] as
    { transport?: { host?: string; user?: string; port?: number; key?: string } } | undefined;
  const explicit = definition?.transport;
  let pin =
    process.env.AAC_AGENT_KNOWN_HOSTS_FILE ??
    config?.agentTransportSsh?.knownHostsFile ??
    path.join(home, '.ssh/known_hosts');
  const identity =
    explicit?.key || process.env.AAC_AGENT_IDENTITY_FILE || config?.agentTransportSsh?.identityFile;
  const cacheKey = JSON.stringify([home, alias, explicit, identity, pin]);
  const cached = resolved.get(cacheKey);
  if (cached && cached.until > Date.now()) return [...cached.args];
  let hostname = explicit?.host;
  let user = explicit?.user;
  let port = explicit?.port;
  let key = identity || undefined;
  if (!hostname || !user || !port || !key) {
    const source = path.join(home, '.ssh/config');
    // Suppress discovery trust/forwarding/commands too; no provider secrets reach Match exec.
    const text = execFileSync(
      'ssh',
      ['-G', '-F', fs.existsSync(source) ? source : 'none', ...SSH_ISOLATION.slice(2), '--', alias],
      {
        encoding: 'utf8',
        timeout: 1000,
        maxBuffer: 128 * 1024,
        stdio: ['ignore', 'pipe', 'ignore'],
        env: Object.fromEntries(
          ['PATH', 'HOME', 'USER', 'LOGNAME', 'SYSTEMROOT', 'WINDIR'].flatMap((name) =>
            process.env[name] ? [[name, process.env[name]]] : []
          )
        ),
      }
    );
    const fields = new Map<string, string>();
    for (const line of text.split('\n')) {
      const space = line.indexOf(' ');
      const name = line.slice(0, space);
      if (!fields.has(name)) fields.set(name, line.slice(space + 1));
    }
    if (
      ['proxyjump', 'proxycommand'].some((name) => fields.has(name) && fields.get(name) !== 'none')
    )
      throw new ConfigError('SSH proxy requires explicit AAC connection configuration');
    hostname ??= fields.get('hostname');
    user ??= fields.get('user');
    port ??= Number(fields.get('port'));
    // One line is the configured IdentityFile. Several are OpenSSH's defaults (or a list):
    // use the first that exists; none means no identity, so refuse rather than guess.
    const identities = text
      .split('\n')
      .filter((line) => line.startsWith('identityfile '))
      .map((line) => expandHome(line.slice('identityfile '.length), home));
    key ??= identities.length > 1 ? identities.find((file) => fs.existsSync(file)) : identities[0];
  }
  key = key && expandHome(key, home);
  if (process.platform === 'win32') {
    key = key?.replace(/\\/g, '/');
    pin = pin.replace(/\\/g, '/');
  }
  if (
    !hostname ||
    !/^[A-Za-z0-9.:_-]+$/.test(hostname) ||
    hostname.startsWith('-') ||
    !user ||
    !/^[A-Za-z0-9_.-]+$/.test(user) ||
    typeof port !== 'number' ||
    !Number.isInteger(port) ||
    port < 1 ||
    port > 65535 ||
    !key ||
    !path.isAbsolute(key) ||
    !path.isAbsolute(pin) ||
    /[\r\n%"\\]/.test(key + pin)
  )
    throw new ConfigError('SSH connection requires an explicit endpoint, identity and pin');
  const args = [
    '-o',
    `Hostname=${hostname}`,
    '-o',
    `User=${user}`,
    '-o',
    `Port=${port}`,
    '-i',
    key,
    '-o',
    `UserKnownHostsFile="${pin}"`,
  ];
  if (resolved.size >= 128) resolved.clear();
  resolved.set(cacheKey, { until: Date.now() + 60_000, args });
  return [...args];
}

/** Identical SSH/SCP isolation and explicit endpoint settings. */
export function sshOptions(host: string): string[] {
  return [...SSH_ISOLATION, ...sshConnectionOptions(host)];
}

/** The full ssh argv for one remote command. */
export function sshArgv(host: string, command: string, keepalive?: SshKeepalive): string[] {
  return [
    '-T',
    ...sshOptions(host),
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
