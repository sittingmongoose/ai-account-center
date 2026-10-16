/**
 * `ai-account-center dashboard usage-snapshot <command>`: where the dashboard publishes account
 * usage for agents (usage.json, usage.md and history.json, rewritten after every refresh).
 *
 *   status          Show the folder, its locations and how old its snapshot is
 *   set <folder> [--location <where>=<path>]...
 *                   Publish into <folder> (an absolute path; created when its parent exists).
 *                   Each --location says where a group of computers finds that folder; the
 *                   reader's rules block lists them. Without --location the saved ones stay.
 *   off             Stop publishing; files already written stay until removed
 *
 * The running dashboard reads the setting (`~/.ccs/agent-usage-snapshot.json`) after each
 * refresh, so a change applies within a minute: no restart is needed. Nothing secret is written:
 * see src/web-server/agent-usage/agent-usage-snapshot.ts.
 */
import fs from 'fs';
import path from 'path';
import {
  isSnapshotDirectory,
  isSnapshotLocationList,
  readSnapshotSettings,
  SNAPSHOT_JSON,
  span,
  writeSnapshotSettings,
  type SnapshotLocation,
} from '../web-server/agent-usage/agent-usage-snapshot';

export interface UsageSnapshotCommandIo {
  out: (text: string) => void;
  err: (text: string) => void;
}

const defaultIo: UsageSnapshotCommandIo = {
  out: (text) => process.stdout.write(text),
  err: (text) => process.stderr.write(text),
};

const COMMANDS = ['status', 'set', 'off'] as const;
type Command = (typeof COMMANDS)[number];

const SET_USAGE =
  '[X] Usage: ai-account-center dashboard usage-snapshot set <folder> [--location <where>=<path>]...\n';

export function usageSnapshotHelp(): string {
  return [
    'Agent usage snapshot (account usage for agents, in a shared folder)',
    '',
    'Usage: ai-account-center dashboard usage-snapshot <command>',
    '',
    'Commands:',
    '  status          Show the folder, its locations and how old its snapshot is',
    '  set <folder> [--location <where>=<path>]...',
    '                  Publish usage.json, usage.md and history.json into <folder>',
    '  off             Stop publishing',
    '',
    'The dashboard rewrites the files after every refresh (every minute by default)',
    'from its own readings. Agents read them with the aac-usage command in that folder.',
    'Each --location names where a group of computers finds the folder, such as',
    '--location "the Mac=/Volumes/share/aac-usage"; aac-usage rules lists them.',
    'Without --location, set keeps the locations already saved.',
    '',
  ].join('\n');
}

async function showStatus(io: UsageSnapshotCommandIo): Promise<number> {
  const settings = await readSnapshotSettings();
  if (!settings) {
    io.out(
      'Usage snapshot: off. Turn it on: ai-account-center dashboard usage-snapshot set <folder>\n'
    );
    return 0;
  }
  io.out(`Usage snapshot: on, folder ${settings.directory}\n`);
  for (const location of settings.locations ?? [])
    io.out(`  ${location.where}: ${location.path}\n`);
  try {
    const stat = await fs.promises.stat(path.join(settings.directory, SNAPSHOT_JSON));
    io.out(
      `Last written ${stat.mtime.toISOString()} (${span((Date.now() - stat.mtimeMs) / 1000)} ago).\n`
    );
  } catch {
    io.out('No snapshot written there yet; the dashboard writes one after its next refresh.\n');
  }
  return 0;
}

/** `<where>=<path>`: the name ends at the first "=", so the path may hold one. */
function parseLocation(value: string): SnapshotLocation | null {
  const split = value.indexOf('=');
  if (split <= 0) return null;
  return { where: value.slice(0, split).trim(), path: value.slice(split + 1).trim() };
}

/** The folder and the --location values, or null when the arguments are malformed. */
function parseSetArgs(args: string[]): { folder: string; locations: string[] | null } | null {
  let folder: string | null = null;
  let locations: string[] | null = null;
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    let value: string | undefined;
    if (arg === '--location') value = args[++index];
    else if (arg.startsWith('--location=')) value = arg.slice('--location='.length);
    else if (arg.startsWith('--') || folder !== null) return null;
    else {
      folder = arg;
      continue;
    }
    if (value === undefined) return null;
    (locations ??= []).push(value);
  }
  return folder === null ? null : { folder, locations };
}

async function setFolder(args: string[], io: UsageSnapshotCommandIo): Promise<number> {
  const parsed = parseSetArgs(args);
  if (!parsed) {
    io.err(SET_USAGE);
    return 1;
  }
  const { folder } = parsed;
  const resolved = path.resolve(folder);
  // join(folder, '.') drops a trailing separator as resolve() does ("/mnt/x/" is "/mnt/x"),
  // and keeps a root ("/", "C:\\") whole; only a ".." part still differs.
  if (!isSnapshotDirectory(folder) || resolved !== path.join(folder, '.')) {
    io.err('[X] Give an absolute folder path without "..".\n');
    return 1;
  }
  let locations: SnapshotLocation[];
  if (parsed.locations) {
    const list = parsed.locations.map(parseLocation);
    if (!list.every((entry) => entry !== null) || !isSnapshotLocationList(list)) {
      io.err(
        '[X] Each --location is <where>=<path>, such as "the Mac=/Volumes/share/aac-usage" (at most 8).\n'
      );
      return 1;
    }
    locations = list;
  } else {
    locations = (await readSnapshotSettings())?.locations ?? [];
  }
  try {
    const stat = await fs.promises.stat(resolved).catch(() => null);
    if (stat && !stat.isDirectory()) {
      io.err(`[X] ${resolved} is not a folder.\n`);
      return 1;
    }
    if (!stat) await fs.promises.mkdir(resolved, { mode: 0o755 });
    await writeSnapshotSettings(resolved, undefined, locations);
  } catch {
    io.err(`[X] ${resolved} could not be used. Its parent folder must exist and be writable.\n`);
    return 1;
  }
  io.err(`[OK] Usage snapshot on: the dashboard writes into ${resolved} after its next refresh.\n`);
  if (locations.length)
    io.err(
      `[i] Locations: ${locations.map((entry) => `${entry.where} ${entry.path}`).join('; ')}\n`
    );
  return 0;
}

export async function handleUsageSnapshotCommand(
  args: string[],
  io: UsageSnapshotCommandIo = defaultIo
): Promise<number> {
  const [first, ...rest] = args;
  if (first === undefined || ['help', '--help', '-h'].includes(first)) {
    io.out(usageSnapshotHelp());
    return 0;
  }
  if (!COMMANDS.includes(first as Command)) {
    io.err(`[X] Unknown usage-snapshot command: ${first}\n${usageSnapshotHelp()}`);
    return 1;
  }
  const command = first as Command;
  if (command === 'set') return setFolder(rest, io);
  if (rest.length > 0) {
    io.err(`[X] Unexpected arguments for usage-snapshot ${command}: ${rest.join(' ')}\n`);
    return 1;
  }
  if (command === 'status') return showStatus(io);
  try {
    await writeSnapshotSettings(null);
    io.err('[OK] Usage snapshot off. Files already written stay until you remove them.\n');
    return 0;
  } catch {
    io.err('[X] The usage snapshot setting could not be removed.\n');
    return 1;
  }
}
