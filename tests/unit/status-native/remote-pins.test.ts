import { describe, expect, it } from 'bun:test';
import {
  PINNED_THREADS_QUERY,
  readRemotePins,
  remotePinsCommand,
  type CommandRunner,
} from '../../../src/web-server/status/status-collectors';

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
      expect(command).toStartWith('/usr/bin/sqlite3 -readonly -json ');
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
    expect(calls[0].args).toContain('jared-mac');
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
    for (const [machine, alias] of [
      ['mac', 'jared-mac'],
      ['windows', 'jared-windows'],
    ] as const) {
      const { run, calls } = fakeRun({ code: 0, stdout: '' });
      await readRemotePins(machine, run);
      expect(calls[0].args).toContain(alias);
    }
  });
});
