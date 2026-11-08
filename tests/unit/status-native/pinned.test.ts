import { describe, expect, it } from 'bun:test';
import { Database } from 'bun:sqlite';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { nativeStatusModuleName } from '../../../src/status-native/native-loader';
import { readPinnedThreads } from '../../../src/status-native/pinned';

/** The module `bun run status:build` leaves behind; the fixture tests need the real reader. */
const PREBUILT = path.join(
  import.meta.dir,
  '../../../native/aac-status/prebuilt',
  nativeStatusModuleName()
);
const hasModule = fs.existsSync(PREBUILT);
const withModule = hasModule ? it : it.skip;

/** A fixture T3 state database in a temporary folder. Never the real `~/.t3`. */
function fixtureDb(rows: Array<Record<string, unknown>>): { file: string; cleanup: () => void } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-status-pins-'));
  const file = path.join(dir, 'statev2.sqlite');
  const db = new Database(file, { create: true });
  db.exec(`CREATE TABLE orchestration_v2_projection_threads (
    thread_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, title TEXT NOT NULL,
    default_provider TEXT NOT NULL, runtime_mode TEXT NOT NULL, interaction_mode TEXT NOT NULL,
    active_provider_thread_id TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
    archived_at TEXT, deleted_at TEXT, payload_json TEXT NOT NULL, provider_instance_id TEXT)`);
  const insert = db.prepare(
    `INSERT INTO orchestration_v2_projection_threads
     (thread_id, project_id, title, default_provider, runtime_mode, interaction_mode,
      created_at, updated_at, archived_at, deleted_at, payload_json)
     VALUES (?, 'proj', ?, 'claude', 'auto', 'default', '2026-11-01T00:00:00Z',
             '2026-11-02T00:00:00Z', ?, ?, ?)`
  );
  for (const row of rows) {
    insert.run(row.id, row.title, row.archivedAt ?? null, row.deletedAt ?? null, JSON.stringify(row.payload));
  }
  db.close();
  return { file, cleanup: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

function pinnedPayload(id: string, title: string, pinOrderKey: string, parent: string | null = null) {
  return {
    id,
    title,
    pinnedAt: '2026-11-08T10:00:00Z',
    pinOrderKey,
    lineage: { parentThreadId: parent },
  };
}

describe('pinned threads through the native module', () => {
  it('without a module the answer is unavailable with its reason, never a throw', async () => {
    const missing = await readPinnedThreads({ env: {}, file: '/nope/aac-status.node' });
    expect(missing).toEqual({ unavailable: 'the aac-status native module is missing' });
    const off = await readPinnedThreads({ env: { AAC_STATUS_NATIVE: 'off' } });
    expect(off).toEqual({ unavailable: 'the aac-status native module is disabled' });
  });

  it('a native answer of the wrong shape is unreadable, not trusted', async () => {
    const bogus = await readPinnedThreads({
      handle: { call: async () => JSON.stringify({ pinned: [{ id: 7 }] }) },
    });
    expect(bogus).toEqual({ unavailable: "T3's state file could not be read" });
    const thrown = await readPinnedThreads({
      handle: {
        call: async () => {
          throw new Error('boom');
        },
      },
    });
    expect(thrown).toEqual({ unavailable: "T3's state file could not be read" });
  });

  withModule('reads the pinned top-level threads from a fixture database, in pin order', async () => {
    const { file, cleanup } = fixtureDb([
      { id: 'second', title: 'Second', payload: pinnedPayload('second', 'Second', 'b') },
      { id: 'first', title: 'First', payload: pinnedPayload('first', 'First', 'a') },
      { id: 'child', title: 'Child', payload: pinnedPayload('child', 'Child', 'a0', 'first') },
      {
        id: 'archived',
        title: 'Archived',
        archivedAt: '2026-11-07T00:00:00Z',
        payload: pinnedPayload('archived', 'Archived', 'c'),
      },
    ]);
    try {
      const result = await readPinnedThreads({ dbPath: file, file: PREBUILT, env: {} });
      expect(result).toEqual({
        pinned: [
          { id: 'first', title: 'First', pinnedAt: '2026-11-08T10:00:00Z', pinOrderKey: 'a' },
          { id: 'second', title: 'Second', pinnedAt: '2026-11-08T10:00:00Z', pinOrderKey: 'b' },
        ],
      });
    } finally {
      cleanup();
    }
  });

  withModule('a missing database and a changed shape each say so', async () => {
    const missing = await readPinnedThreads({
      dbPath: path.join(os.tmpdir(), 'aac-status-no-such-dir', 'statev2.sqlite'),
      file: PREBUILT,
      env: {},
    });
    expect(missing).toEqual({ unavailable: "T3's state file is missing" });

    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-status-shape-'));
    const file = path.join(dir, 'statev2.sqlite');
    const db = new Database(file, { create: true });
    db.exec('CREATE TABLE something_else (id TEXT)');
    db.close();
    try {
      const changed = await readPinnedThreads({ dbPath: file, file: PREBUILT, env: {} });
      expect(changed).toEqual({ unavailable: "T3's state file changed shape" });
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
