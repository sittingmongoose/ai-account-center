import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import { Database } from 'bun:sqlite';
import * as os from 'os';
import * as path from 'path';
import {
  PINNED_THREADS_QUERY,
  readRemotePins,
  remotePinsCommand,
  type CommandRunner,
} from '../../../src/web-server/status/status-collectors';
import { APP_UPDATE_SSH_HOSTS } from '../../../src/web-server/services/app-update-hosts';
import { openStatusHandle, parseRemotePins } from '../../../src/status-native/pinned';
import { nativeStatusModuleName } from '../../../src/status-native/native-loader';

/** The module `bun run status:build` leaves behind; this check needs the real parser. */
const PREBUILT = path.join(
  import.meta.dir,
  '../../../native/aac-status/prebuilt',
  nativeStatusModuleName()
);
const hasModule = fs.existsSync(PREBUILT);
const withModule = hasModule ? it : it.skip;

/** A runner that records the invocation and answers a fixed result. */
function fakeRun(result: {
  code: number | null;
  stdout?: string;
  timedOut?: boolean;
  missing?: boolean;
}): { run: CommandRunner; calls: Array<{ file: string; args: string[] }> } {
  const calls: Array<{ file: string; args: string[] }> = [];
  const run: CommandRunner = async (file, args) => {
    calls.push({ file, args });
    return {
      code: result.code,
      stdout: result.stdout ?? '',
      timedOut: result.timedOut ?? false,
      missing: result.missing ?? false,
    };
  };
  return { run, calls };
}

describe('remotePinsCommand', () => {
  it('mac and linux run the fixed sqlite3 read-only, with the same query as the local read', () => {
    for (const hostOs of ['mac', 'linux'] as const) {
      const command = remotePinsCommand(hostOs);
      expect(command).toStartWith(
        'for s in /usr/bin/sqlite3 /usr/local/bin/sqlite3 /opt/homebrew/bin/sqlite3 "$(command -v sqlite3)"; do '
      );
      expect(command).toContain(
        'exec "$s" -readonly -json "file:$HOME/.t3/userdata/statev2.sqlite?mode=ro" '
      );
      expect(command).toEndWith('; done; exit 127');
      expect(command).toContain('file:$HOME/.t3/userdata/statev2.sqlite?mode=ro');
      expect(command).toContain('orchestration_v2_projection_threads');
      expect(command).toContain(PINNED_THREADS_QUERY);
      // Read-only both ways: the flag and the URI mode.
      expect(command).toContain('-readonly');
      expect(command).toContain('mode=ro');
    }
  });

  it('windows runs chocolatey sqlite3 through the powershell route, read-only', () => {
    const command = remotePinsCommand('windows');
    expect(command).toStartWith('powershell.exe -NoProfile -NonInteractive -EncodedCommand ');
    const encoded = command.split(' ').pop() ?? '';
    const script = Buffer.from(encoded, 'base64').toString('utf16le');
    expect(script).toContain("C:\\ProgramData\\chocolatey\\bin\\sqlite3.exe' -readonly -json");
    expect(script).toContain("$env:USERPROFILE '.t3\\userdata\\statev2.sqlite'");
    // The query's single quotes are doubled inside the PowerShell literal.
    expect(script).toContain(PINNED_THREADS_QUERY.replace(/'/g, "''"));
  });
});

describe('readRemotePins', () => {
  it('returns the machine printed, empty included', async () => {
    const { run, calls } = fakeRun({ code: 0, stdout: '[{"thread_id":"t1"}]' });
    const json = await readRemotePins('mac', run);
    expect(json).toBe('[{"thread_id":"t1"}]');
    expect(calls[0].file).toBe('ssh');
    expect(calls[0].args).toContain(APP_UPDATE_SSH_HOSTS.mac);
    expect(calls[0].args.at(-1)).toBe(remotePinsCommand('mac'));

    const empty = await readRemotePins('windows', fakeRun({ code: 0, stdout: '' }).run);
    expect(empty).toBe('');
  });

  it('throws a fixed-word error when the machine does not answer', async () => {
    for (const result of [
      { code: 1, stdout: '' },
      { code: null, stdout: '', timedOut: true },
      { code: null, stdout: '', missing: true },
    ]) {
      const { run } = fakeRun(result);
      await expect(readRemotePins('nas1', run)).rejects.toThrow();
    }
  });

  it('uses the fixed alias of each machine', async () => {
    for (const machine of ['mac', 'windows'] as const) {
      const alias = APP_UPDATE_SSH_HOSTS[machine];
      const { run, calls } = fakeRun({ code: 0, stdout: '' });
      await readRemotePins(machine, run);
      expect(calls[0].args).toContain(alias);
    }
  });
});

describe('PINNED_THREADS_QUERY against the real parser', () => {
  /** A fixture database with the projection table and the rows given, as `sqlite3 -json` would print them. */
  function remoteRows(rows: Array<{ id: string; title: string; payload: Record<string, unknown> }>): string {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-remote-pins-'));
    try {
      const db = new Database(path.join(dir, 'statev2.sqlite'));
      db.exec(
        'CREATE TABLE orchestration_v2_projection_threads (thread_id TEXT PRIMARY KEY, title TEXT, archived_at TEXT, deleted_at TEXT, payload_json TEXT)'
      );
      const insert = db.prepare(
        "INSERT INTO orchestration_v2_projection_threads VALUES (?,?,NULL,NULL,?)"
      );
      for (const row of rows) insert.run(row.id, row.title, JSON.stringify(row.payload));
      const printed = JSON.stringify(db.prepare(PINNED_THREADS_QUERY).all());
      db.close();
      return printed;
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }

  withModule('a wrong-typed lineage is a shape refusal, not a top-level pin', async () => {
    const opened = openStatusHandle({ file: PREBUILT });
    expect(!('unavailable' in opened)).toBe(true);
    const handle = 'unavailable' in opened ? null : opened;
    if (!handle) throw new Error('module unavailable');

    // T3's lineage is an object with a parentThreadId, or absent. A pinned row whose lineage
    // is another container once read as top-level, because the parent extract cannot walk
    // into it; the query now carries the shape facts and the parser refuses the machine.
    const printed = remoteRows([
      {
        id: 'child-after-drift',
        title: 'Child after schema drift',
        payload: { pinnedAt: '2026-11-08T10:00:00Z', pinOrderKey: 'a', lineage: [{ parentThreadId: 'parent-fixture' }] },
      },
    ]);
    const refused = await parseRemotePins('mac', printed, { handle });
    expect('unavailable' in refused && refused.unavailable).toBe('shape');
  });

  withModule('a real child and real top-level rows still parse by their lineage shape', async () => {
    const opened = openStatusHandle({ file: PREBUILT });
    expect(!('unavailable' in opened)).toBe(true);
    const handle = 'unavailable' in opened ? null : opened;
    if (!handle) throw new Error('module unavailable');

    const printed = remoteRows([
      { id: 'child', title: 'Child', payload: { pinnedAt: '2026-11-08T10:00:00Z', pinOrderKey: 'a', lineage: { parentThreadId: 'top' } } },
      { id: 'top-null', title: 'Top null parent', payload: { pinnedAt: '2026-11-08T10:00:00Z', pinOrderKey: 'b', lineage: { parentThreadId: null } } },
      { id: 'top-bare', title: 'Top no lineage', payload: { pinnedAt: '2026-11-08T10:00:00Z', pinOrderKey: 'c' } },
    ]);
    const parsed = await parseRemotePins('mac', printed, { handle });
    expect('pinned' in parsed && parsed.pinned.map((pin) => pin.id)).toEqual(['top-null', 'top-bare']);
  });
});
