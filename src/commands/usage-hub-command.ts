/**
 * `ai-account-center dashboard usage-hub <command>`: the T3 usage hub's key.
 *
 *   status              Show whether the hub is on, the key's fingerprint and the URL for T3
 *   generate --stdout   Create the key and write it once to standard output
 *   rotate --stdout     Replace the key (the old one stops working at once)
 *   off                 Remove the key; the hub answers 404 until a new key is made
 *
 * The key is written to standard output only, with nothing else, and only
 * when `--stdout` (or `--print-once`) is given, so it can be piped straight
 * into T3 without landing in a log or a terminal scrollback by accident. Every
 * message goes to standard error. Only the key's SHA-256 is stored
 * (`~/.ccs/auth/usage-hub-key.json`, 0600), so a lost key is rotated, never
 * shown again. The running dashboard reads the file on every request: no
 * restart is needed.
 */
import os from 'os';
import { getDashboardNetworkSettings } from '../web-server/services/dashboard-network-config';
import {
  readUsageHubKeyState,
  removeUsageHubKey,
  usageHubKeyFingerprint,
  writeUsageHubKey,
  UsageHubKeyExistsError,
} from '../web-server/usage-hub/usage-hub-key-store';

export interface UsageHubCommandIo {
  out: (text: string) => void;
  err: (text: string) => void;
}

const defaultIo: UsageHubCommandIo = {
  out: (text) => process.stdout.write(text),
  err: (text) => process.stderr.write(text),
};

const PRINT_FLAGS = ['--stdout', '--print-once'];
const COMMANDS = ['status', 'generate', 'rotate', 'off'] as const;
type Command = (typeof COMMANDS)[number];

export function usageHubHelp(): string {
  return [
    'T3 usage hub (read-only CLIProxyAPI-compatible usage for T3 Code)',
    '',
    'Usage: ai-account-center dashboard usage-hub <command>',
    '',
    'Commands:',
    '  status              Show whether the hub is on and the URL for T3',
    '  generate --stdout   Create the management key and write it once to stdout',
    '  rotate --stdout     Replace the key; the old key stops working at once',
    '  off                 Remove the key and turn the hub off',
    '',
    'The key is shown only once, on stdout, and only with --stdout (or --print-once).',
    'Pipe it straight into T3; AI Account Center keeps only its SHA-256.',
    '',
    'In T3: Settings > Providers > Usage providers > Add hub, with the dashboard',
    'address and no path, for example http://127.0.0.1:3000 on this computer.',
    '',
  ].join('\n');
}

/** Private IPv4 addresses of this computer, for the LAN URL hint. */
function privateAddresses(): string[] {
  const result: string[] = [];
  for (const entries of Object.values(os.networkInterfaces())) {
    for (const entry of entries ?? []) {
      if (entry.family !== 'IPv4' || entry.internal) continue;
      if (/^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(entry.address)) {
        result.push(entry.address);
      }
    }
  }
  return [...new Set(result)];
}

async function showStatus(io: UsageHubCommandIo): Promise<number> {
  const state = await readUsageHubKeyState();
  if (state.state === 'off') {
    io.out(
      'Usage hub: off (no key). Turn it on: ai-account-center dashboard usage-hub generate --stdout\n'
    );
  } else if (state.state === 'invalid') {
    io.out(
      'Usage hub: off. The key file cannot be read safely; replace it: ai-account-center dashboard usage-hub rotate --stdout\n'
    );
  } else {
    io.out(
      `Usage hub: on (key fingerprint ${usageHubKeyFingerprint(state.record)}, created ${state.record.createdAt})\n`
    );
  }
  const network = getDashboardNetworkSettings();
  io.out('URL for T3 (no path; T3 adds /v0/management/...):\n');
  io.out('  this computer:  http://127.0.0.1:<dashboard port>  (3000 for the installed service)\n');
  if (network.trustLocalNetwork) {
    const addresses = privateAddresses();
    for (const address of addresses)
      io.out(`  other computers: http://${address}:<dashboard port>\n`);
    io.out(
      `LAN access: allowed for peers in ${network.trustedNetworks.join(', ') || 'no ranges'} (trusted local network is on).\n`
    );
  } else {
    io.out(
      'LAN access: refused (loopback and HTTPS only). Turn on the trusted local network in the dashboard Settings, from this computer, to allow other computers.\n'
    );
  }
  return 0;
}

async function writeKey(
  command: 'generate' | 'rotate',
  args: string[],
  io: UsageHubCommandIo
): Promise<number> {
  if (!args.some((arg) => PRINT_FLAGS.includes(arg))) {
    io.err(
      `[X] The key is shown only once. Run: ai-account-center dashboard usage-hub ${command} --stdout\n`
    );
    return 1;
  }
  try {
    const key = await writeUsageHubKey({ replace: command === 'rotate' });
    io.out(`${key}\n`);
    io.err(
      command === 'rotate'
        ? '[OK] Usage hub key replaced. The old key no longer works; update T3 with the new one.\n'
        : '[OK] Usage hub key created. It was written to stdout once and is not stored.\n'
    );
    return 0;
  } catch (error) {
    if (error instanceof UsageHubKeyExistsError) {
      io.err(
        '[X] A usage hub key already exists. Replace it with: ai-account-center dashboard usage-hub rotate --stdout\n'
      );
      return 1;
    }
    io.err('[X] The usage hub key could not be saved safely.\n');
    return 1;
  }
}

export async function handleUsageHubCommand(
  args: string[],
  io: UsageHubCommandIo = defaultIo
): Promise<number> {
  const [first, ...rest] = args;
  if (first === undefined || ['help', '--help', '-h'].includes(first)) {
    io.out(usageHubHelp());
    return 0;
  }
  if (!COMMANDS.includes(first as Command)) {
    io.err(`[X] Unknown usage-hub command: ${first}\n${usageHubHelp()}`);
    return 1;
  }
  const command = first as Command;
  const allowed = command === 'generate' || command === 'rotate' ? PRINT_FLAGS : [];
  const unexpected = rest.filter((arg) => !allowed.includes(arg));
  if (unexpected.length > 0) {
    io.err(`[X] Unexpected arguments for usage-hub ${command}: ${unexpected.join(' ')}\n`);
    return 1;
  }
  if (command === 'status') return showStatus(io);
  if (command === 'off') {
    try {
      const removed = await removeUsageHubKey();
      io.err(
        removed
          ? '[OK] Usage hub turned off. Its key no longer works.\n'
          : '[i] The usage hub was already off.\n'
      );
      return 0;
    } catch {
      io.err('[X] The usage hub key could not be removed.\n');
      return 1;
    }
  }
  return writeKey(command, rest, io);
}
