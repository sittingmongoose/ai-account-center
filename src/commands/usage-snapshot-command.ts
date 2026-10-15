/**
 * `ai-account-center dashboard usage-snapshot <command>`: where the dashboard publishes account
 * usage for agents (usage.json, usage.md and history.json, rewritten after every refresh).
 *
 *   status          Show the folder and how old its snapshot is
 *   set <folder>    Publish into <folder> (an absolute path; created when its parent exists)
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
  readSnapshotSettings,
  SNAPSHOT_JSON,
  span,
  writeSnapshotSettings,
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

export function usageSnapshotHelp(): string {
  return [
    'Agent usage snapshot (account usage for agents, in a shared folder)',
    '',
    'Usage: ai-account-center dashboard usage-snapshot <command>',
    '',
    'Commands:',
    '  status          Show the folder and how old its snapshot is',
    '  set <folder>    Publish usage.json, usage.md and history.json into <folder>',
    '  off             Stop publishing',
    '',
    'The dashboard rewrites the files after every refresh (every minute by default)',
    'from its own readings. Agents read them with the aac-usage command in that folder.',
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

async function setFolder(folder: string, io: UsageSnapshotCommandIo): Promise<number> {
  const resolved = path.resolve(folder);
  if (!isSnapshotDirectory(folder) || resolved !== path.normalize(folder)) {
    io.err('[X] Give an absolute folder path without "..".\n');
    return 1;
  }
  try {
    const stat = await fs.promises.stat(resolved).catch(() => null);
    if (stat && !stat.isDirectory()) {
      io.err(`[X] ${resolved} is not a folder.\n`);
      return 1;
    }
    if (!stat) await fs.promises.mkdir(resolved, { mode: 0o755 });
    await writeSnapshotSettings(resolved);
  } catch {
    io.err(`[X] ${resolved} could not be used. Its parent folder must exist and be writable.\n`);
    return 1;
  }
  io.err(`[OK] Usage snapshot on: the dashboard writes into ${resolved} after its next refresh.\n`);
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
  if (command === 'set') {
    if (rest.length !== 1) {
      io.err('[X] Usage: ai-account-center dashboard usage-snapshot set <folder>\n');
      return 1;
    }
    return setFolder(rest[0], io);
  }
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
