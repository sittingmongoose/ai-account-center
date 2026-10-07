/**
 * `ai-account-center dashboard proxy <command>`: the LAN HTTPS reverse proxy
 * (`dashboard_tls.trusted_proxy: lan-https-proxy`), set through the product so
 * config.yaml is never edited by hand.
 *
 *   status                                   Show whether the proxy is trusted, and why not
 *   set --address <ip> --origin <https url>  Trust the proxy at <ip> (repeat --address, up to 8)
 *   off                                      Stop trusting it
 *
 * `set` and `off` check every value with the dashboard's own parser first, then
 * change config.yaml through the product's config writer (locked, atomic,
 * 0600), after saving the previous file next to it as
 * `config.yaml.bak-proxy-<UTC time>` (0600). Only `dashboard_tls` changes;
 * the writer keeps every other setting, though it rewrites the file's layout
 * and comments the way every product save does. Nothing secret is printed.
 *
 * The running dashboard reads config.yaml on every request (cached by the
 * file's size and time), so a change applies to the next request: no restart.
 */
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  getConfigYamlPath,
  loadOrCreateUnifiedConfig,
  mutateConfig,
} from '../config/config-loader-facade';
import type { DashboardTlsConfig } from '../config/schemas/auth';
import { ConfigError } from '../errors/error-types';
import {
  describeTrustedProxyProblem,
  invalidateDashboardTlsSettings,
  parseDashboardTlsSettings,
  parsePublicOrigin,
  parseTrustedProxyAddresses,
  TRUSTED_PROXY_ADDRESSES_MAX,
} from '../web-server/services/dashboard-tls-config';

export interface ProxyCommandIo {
  out: (text: string) => void;
  err: (text: string) => void;
}

export interface ProxyCommandDeps {
  /** This computer's addresses (the parser's default reads the interfaces). Tests only. */
  selfAddresses?: readonly string[];
  /** The clock for the backup's name. Tests only. */
  now?: () => Date;
}

const defaultIo: ProxyCommandIo = {
  out: (text) => process.stdout.write(text),
  err: (text) => process.stderr.write(text),
};

const COMMANDS = ['status', 'set', 'off'] as const;
type Command = (typeof COMMANDS)[number];
const LAN_KIND = 'lan-https-proxy';
const LIVE_NOTE =
  'The running dashboard reads config.yaml on every request, so this applies to its next request; no restart is needed.\n';

export function proxyHelp(): string {
  return [
    'LAN HTTPS reverse proxy (an HTTPS proxy on another computer in front of the dashboard)',
    '',
    'Usage: ai-account-center dashboard proxy <command>',
    '',
    'Commands:',
    '  status                                   Show whether the proxy is trusted, and why not',
    '  set --address <ip> --origin <https url>  Trust the proxy at <ip> (repeat --address, up to 8)',
    '  off                                      Stop trusting the proxy',
    '',
    'Options for set:',
    "  --address <ip>    The proxy's exact LAN address as the dashboard sees it: a private",
    '                    address, not a range, not loopback and not this computer',
    '  --origin <url>    The public https:// address the proxy serves, with no path',
    '  --replace         Replace a local proxy kind (tailscale-serve, loopback-https-proxy)',
    '',
    'Example:',
    '  ai-account-center dashboard proxy set --address 192.168.1.20 --origin https://aac.example.test',
    '',
    'set and off save the previous config.yaml next to it as config.yaml.bak-proxy-<UTC time> (0600).',
    'The running dashboard applies the change to its next request; no restart is needed.',
    '',
  ].join('\n');
}

/** Home-relative, for messages: never a full path outside the home folder. */
function displayPath(file: string): string {
  const home = os.homedir();
  return home && file.startsWith(`${home}${path.sep}`) ? `~${file.slice(home.length)}` : file;
}

function backupStamp(now: Date): string {
  return now
    .toISOString()
    .replace(/[-:]/g, '')
    .replace(/\.\d+Z$/, 'Z');
}

/**
 * Copy config.yaml's current bytes next to it (0600, never overwriting an
 * existing file). Runs inside the config writer's lock, so the copy is exactly
 * the file the change replaces. Null when there is no file yet.
 */
function backupConfig(now: Date): string | null {
  const file = getConfigYamlPath();
  let bytes: Buffer;
  try {
    bytes = fs.readFileSync(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
  const base = `${file}.bak-proxy-${backupStamp(now)}`;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const target = attempt === 0 ? base : `${base}-${attempt + 1}`;
    try {
      fs.writeFileSync(target, bytes, { mode: 0o600, flag: 'wx' });
      fs.chmodSync(target, 0o600);
      return target;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
    }
  }
  throw new ConfigError('No free name for the config.yaml backup', file);
}

function currentTlsBlock(): Record<string, unknown> {
  const block = (loadOrCreateUnifiedConfig() as { dashboard_tls?: unknown }).dashboard_tls;
  return block && typeof block === 'object' && !Array.isArray(block)
    ? { ...(block as Record<string, unknown>) }
    : {};
}

function showStatus(io: ProxyCommandIo, deps: ProxyCommandDeps): number {
  let block: Record<string, unknown>;
  try {
    block = currentTlsBlock();
  } catch {
    io.err('[X] config.yaml cannot be read; nothing is trusted until it can.\n');
    return 1;
  }
  const settings = parseDashboardTlsSettings(block, { selfAddresses: deps.selfAddresses });
  if (settings.trustedProxy === LAN_KIND) {
    io.out('LAN HTTPS proxy: on\n');
    io.out(`  proxy address: ${settings.trustedProxyAddresses.join(', ')}\n`);
    io.out(`  public origin: ${settings.publicOrigin ?? 'not set'}\n`);
    io.out(
      '  Requests from that address count as secure only with X-Forwarded-Proto: https and a client IP as the last X-Forwarded-For entry.\n'
    );
    return 0;
  }
  if (settings.trustedProxyProblem) {
    io.out(
      `LAN HTTPS proxy: off. dashboard_tls.trusted_proxy_addresses was refused: ${describeTrustedProxyProblem(settings.trustedProxyProblem)}.\n`
    );
    io.out(
      'Fix it with: ai-account-center dashboard proxy set --address <ip> --origin <https url>\n'
    );
    return 0;
  }
  if (settings.trustedProxy !== null) {
    io.out(
      `LAN HTTPS proxy: off. dashboard_tls.trusted_proxy is ${settings.trustedProxy} (a local proxy on this computer).\n`
    );
    return 0;
  }
  io.out(
    'LAN HTTPS proxy: off. Turn it on: ai-account-center dashboard proxy set --address <ip> --origin <https url>\n'
  );
  return 0;
}

interface SetArgs {
  addresses: string[];
  origin: string | null;
  replace: boolean;
}

function parseSetArgs(args: string[]): SetArgs | string {
  const result: SetArgs = { addresses: [], origin: null, replace: false };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    const [flag, inline] = arg.includes('=') ? [arg.slice(0, arg.indexOf('=')), arg] : [arg, null];
    const value = (): string | null => {
      if (inline !== null) return inline.slice(inline.indexOf('=') + 1);
      const next = args[index + 1];
      if (next === undefined || next.startsWith('--')) return null;
      index += 1;
      return next;
    };
    if (flag === '--replace' && inline === null) {
      result.replace = true;
    } else if (flag === '--address') {
      const address = value();
      if (address === null) return '--address needs a value';
      result.addresses.push(address);
    } else if (flag === '--origin') {
      const origin = value();
      if (origin === null) return '--origin needs a value';
      if (result.origin !== null) return '--origin may be given once';
      result.origin = origin;
    } else {
      return `unexpected argument: ${arg}`;
    }
  }
  if (result.addresses.length === 0) return '--address is required';
  if (result.origin === null) return '--origin is required';
  return result;
}

function sameList(left: unknown, right: readonly string[]): boolean {
  return (
    Array.isArray(left) &&
    left.length === right.length &&
    left.every((value, index) => value === right[index])
  );
}

function setProxy(args: string[], io: ProxyCommandIo, deps: ProxyCommandDeps): number {
  const parsed = parseSetArgs(args);
  if (typeof parsed === 'string') {
    io.err(`[X] proxy set: ${parsed}.\n${proxyHelp()}`);
    return 1;
  }
  const list = parseTrustedProxyAddresses(parsed.addresses, { selfAddresses: deps.selfAddresses });
  if (!list.ok) {
    io.err(
      `[X] Not saved: ${describeTrustedProxyProblem(list.problem)}. Give the proxy's exact private LAN address (up to ${TRUSTED_PROXY_ADDRESSES_MAX}).\n`
    );
    return 1;
  }
  const origin = parsePublicOrigin(parsed.origin);
  if (origin === null) {
    io.err(
      '[X] Not saved: --origin must be an https:// address with no path, user or query, like https://aac.example.test\n'
    );
    return 1;
  }
  let before: Record<string, unknown>;
  try {
    before = currentTlsBlock();
  } catch {
    io.err('[X] config.yaml cannot be read, so nothing was changed.\n');
    return 1;
  }
  const kind = before.trusted_proxy;
  if ((kind === 'tailscale-serve' || kind === 'loopback-https-proxy') && !parsed.replace) {
    io.err(
      `[X] Not saved: dashboard_tls.trusted_proxy is ${kind}, and only one proxy kind works at a time. Add --replace to switch to the LAN HTTPS proxy.\n`
    );
    return 1;
  }
  if (
    kind === LAN_KIND &&
    sameList(before.trusted_proxy_addresses, list.addresses) &&
    before.public_origin === origin
  ) {
    io.out('[i] The LAN HTTPS proxy is already set to these values; nothing changed.\n');
    return 0;
  }
  const saved: { backup: string | null } = { backup: null };
  try {
    mutateConfig((config) => {
      saved.backup = backupConfig((deps.now ?? (() => new Date()))());
      const existing = { ...(config.dashboard_tls ?? {}) } as DashboardTlsConfig;
      config.dashboard_tls = {
        ...existing,
        trusted_proxy: LAN_KIND,
        trusted_proxy_addresses: [...list.addresses],
        public_origin: origin,
      };
    });
  } catch {
    io.err('[X] config.yaml could not be saved; the previous settings stay in place.\n');
    return 1;
  } finally {
    invalidateDashboardTlsSettings();
  }
  io.out(
    `[OK] LAN HTTPS proxy on: address ${list.addresses.join(', ')}, public origin ${origin}.\n`
  );
  io.out(
    saved.backup
      ? `[i] The previous config.yaml is saved as ${displayPath(saved.backup)} (0600).\n`
      : '[i] There was no config.yaml before, so there is no backup.\n'
  );
  io.out(LIVE_NOTE);
  return 0;
}

function proxyOff(io: ProxyCommandIo, deps: ProxyCommandDeps): number {
  let before: Record<string, unknown>;
  try {
    before = currentTlsBlock();
  } catch {
    io.err('[X] config.yaml cannot be read, so nothing was changed.\n');
    return 1;
  }
  const lanKind = before.trusted_proxy === LAN_KIND;
  if (!lanKind && before.trusted_proxy_addresses === undefined) {
    io.out('[i] The LAN HTTPS proxy is already off; nothing changed.\n');
    return 0;
  }
  const saved: { backup: string | null } = { backup: null };
  try {
    mutateConfig((config) => {
      saved.backup = backupConfig((deps.now ?? (() => new Date()))());
      const block = { ...(config.dashboard_tls ?? {}) } as DashboardTlsConfig;
      if (block.trusted_proxy === LAN_KIND) {
        delete block.trusted_proxy;
        // The public origin was the proxy's address; it means nothing without it.
        delete block.public_origin;
      }
      delete block.trusted_proxy_addresses;
      if (Object.keys(block).length === 0) delete config.dashboard_tls;
      else config.dashboard_tls = block;
    });
  } catch {
    io.err('[X] config.yaml could not be saved; the previous settings stay in place.\n');
    return 1;
  } finally {
    invalidateDashboardTlsSettings();
  }
  io.out('[OK] LAN HTTPS proxy off: requests from its address are ordinary LAN requests again.\n');
  if (saved.backup) {
    io.out(`[i] The previous config.yaml is saved as ${displayPath(saved.backup)} (0600).\n`);
  }
  io.out(LIVE_NOTE);
  return 0;
}

export async function handleProxyCommand(
  args: string[],
  io: ProxyCommandIo = defaultIo,
  deps: ProxyCommandDeps = {}
): Promise<number> {
  const [first, ...rest] = args;
  if (first === undefined || ['help', '--help', '-h'].includes(first)) {
    io.out(proxyHelp());
    return 0;
  }
  if (!COMMANDS.includes(first as Command)) {
    io.err(`[X] Unknown proxy command: ${first}\n${proxyHelp()}`);
    return 1;
  }
  const command = first as Command;
  if (rest.some((arg) => ['help', '--help', '-h'].includes(arg))) {
    io.out(proxyHelp());
    return 0;
  }
  if (command === 'set') return setProxy(rest, io, deps);
  if (rest.length > 0) {
    io.err(`[X] Unexpected arguments for proxy ${command}: ${rest.join(' ')}\n`);
    return 1;
  }
  if (command === 'status') return showStatus(io, deps);
  return proxyOff(io, deps);
}
