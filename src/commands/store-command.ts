/**
 * `ai-account-center dashboard store <command>`: the history store (docs/store.md 8.2).
 *
 *   status                 The store's state, sizes, checkpoints and last backup
 *   check                  Re-hash every sealed segment (the dashboard must be stopped)
 *   backup                 Write a verified backup now (the dashboard must be stopped)
 *   restore <backup-dir>   Restore a backup (the dashboard must be stopped)
 *
 * The running dashboard holds the store's lock. Then `status` reads the read-only route
 * `GET /api/store/status` when AAC_DASHBOARD_URL and AAC_DEVICE_TOKEN (a paired device token)
 * are set, and otherwise says how to read it. Nothing secret is printed.
 */
import * as path from 'path';
import { getCcsDir } from '../utils/config-manager';
import { loadNativeStore } from '../store/native-loader';
import { storeDirectory, storeErrorCode } from '../store/store-service';

export interface StoreCommandIo {
  out: (text: string) => void;
  err: (text: string) => void;
  env: NodeJS.ProcessEnv;
  fetchFn?: typeof fetch;
}

const defaultIo: StoreCommandIo = {
  out: (text) => process.stdout.write(text),
  err: (text) => process.stderr.write(text),
  env: process.env,
};

export function storeCommandHelp(): string {
  return [
    'History store (usage readings, rollups and their backups)',
    '',
    'Usage: ai-account-center dashboard store <command>',
    '',
    'Commands:',
    '  status                 Show the store state, sizes and last backup',
    '  check                  Re-hash every sealed log segment (dashboard stopped)',
    '  backup                 Write a verified backup now (dashboard stopped)',
    '  restore <backup-dir>   Restore a backup folder (dashboard stopped)',
    '',
    'While the dashboard runs it holds the store; status then reads GET /api/store/status',
    'when AAC_DASHBOARD_URL and AAC_DEVICE_TOKEN (a paired device token) are set.',
    '',
  ].join('\n');
}

const CODES: Record<string, string> = {
  lock_held: 'The dashboard is running and holds the store. Stop it first.',
  newer_store:
    'The store was written by a newer AI Account Center. Update, or restore an older backup.',
  unreleased_store:
    'This store was written by an unreleased test build; move it aside and AAC will start a new one.',
  backup_unverified: 'The backup does not match its own hashes. Nothing was changed.',
  invalid: 'That is not an AI Account Center store backup folder.',
};

function line(io: StoreCommandIo, text: string): void {
  io.out(`${text}\n`);
}

async function viaApi(io: StoreCommandIo): Promise<number> {
  const url = io.env.AAC_DASHBOARD_URL?.trim();
  const token = io.env.AAC_DEVICE_TOKEN?.trim();
  if (!url || !token) {
    line(io, '[i] The dashboard is running and holds the store.');
    line(io, '    Read GET /api/store/status in the dashboard, or set AAC_DASHBOARD_URL and');
    line(io, '    AAC_DEVICE_TOKEN (a paired device token) and run this again.');
    return 0;
  }
  const response = await (io.fetchFn ?? fetch)(new URL('/api/store/status', url), {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!response.ok) {
    io.err(`[X] The dashboard answered ${response.status}.\n`);
    return 1;
  }
  line(io, JSON.stringify(await response.json(), null, 2));
  return 0;
}

export async function handleStoreCommand(
  args: string[],
  io: StoreCommandIo = defaultIo
): Promise<number> {
  const [command, ...rest] = args;
  if (!command || ['help', '--help', '-h'].includes(command)) {
    io.out(storeCommandHelp());
    return command ? 0 : 1;
  }
  if (!['status', 'check', 'backup', 'restore'].includes(command)) {
    io.err(`[X] Unknown store command. Run: ai-account-center dashboard store help\n`);
    return 1;
  }
  if ((command === 'restore' && rest.length !== 1) || (command !== 'restore' && rest.length > 0)) {
    io.err('[X] Usage: ai-account-center dashboard store restore <backup-dir>\n');
    return 1;
  }
  const loaded = loadNativeStore({ env: io.env });
  if (!loaded.module) {
    io.err(
      `[X] The history store is not available on this computer (${loaded.reason}). ` +
        'Build it with: bun run store:build\n'
    );
    return 1;
  }
  const directory = storeDirectory(getCcsDir());
  const handle = new loaded.module.NativeStore();
  const call = async (method: string, payload: unknown): Promise<Record<string, unknown>> =>
    JSON.parse(await handle.call(method, JSON.stringify(payload))) as Record<string, unknown>;
  try {
    if (command === 'restore') {
      const result = await call('restore', { dir: directory, backupDir: path.resolve(rest[0]) });
      line(io, `[OK] Restored backup ${String(result.restored)}.`);
      line(io, `     The files it replaced are in ${String(result.quarantine)}.`);
      return 0;
    }
    try {
      await call('open', { dir: directory });
    } catch (error) {
      if (storeErrorCode(error) === 'lock_held' && command === 'status') return viaApi(io);
      throw error;
    }
    try {
      if (command === 'status') line(io, JSON.stringify(await call('status', {}), null, 2));
      if (command === 'check') {
        const result = await call('maintain', { verify: true });
        const findings = ((result.verified as { findings?: unknown[] } | null)?.findings ?? [])
          .length;
        line(
          io,
          findings === 0
            ? '[OK] Every sealed segment matches its hash.'
            : `[!] ${findings} segment(s) failed their hash; see status.`
        );
      }
      if (command === 'backup') {
        const result = await call('backup', {
          roots: [path.join(directory, 'backups')],
          withRedb: true,
        });
        line(
          io,
          `[OK] Backup ${String(result.id)} written and verified (${String(result.files)} files).`
        );
      }
    } finally {
      await call('close', {}).catch(() => undefined);
    }
    return 0;
  } catch (error) {
    const code = storeErrorCode(error);
    io.err(`[X] ${CODES[code] ?? `The store refused this (${code}).`}\n`);
    return 1;
  }
}
