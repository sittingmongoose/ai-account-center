import { afterEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as childProcess from 'child_process';
import {
  ClaudeDesktopTransportError,
  openClaudeMacLauncher,
  openClaudeWindowsLauncher,
  readClaudeDesktopUsageHistory,
} from '../../../src/web-server/services/claude-desktop-transport';

const mac = {
  launcherName: 'Claude (work@example.com).app',
  launcherPath: "/Users/example/Applications/Claude (work's account).app",
  profilePath: '/Users/example/Library/Application Support/Claude-work',
  sshHost: 'example-mac',
};
const windows = {
  launcherName: 'Claude - Work.lnk',
  profilePath: "C:\\Users\\example\\AppData\\Roaming\\Claude-work's account",
  sshHost: 'example-windows',
};

function mockSsh(stdout = '', failure?: Partial<childProcess.ExecFileException>) {
  return spyOn(childProcess, 'execFile').mockImplementation((...args: unknown[]) => {
    const callback = args.at(-1) as (
      error: childProcess.ExecFileException | null,
      stdout: string,
      stderr: string
    ) => void;
    callback(
      failure ? Object.assign(new Error('PRIVATE_SSH_ERROR'), failure) : null,
      stdout,
      'PRIVATE_STDERR'
    );
    return {} as childProcess.ChildProcess;
  });
}

afterEach(() => mock.restore());

describe('Claude desktop transport', () => {
  it('opens only a quoted existing .app as the matching console user', async () => {
    const exec = mockSsh();
    await openClaudeMacLauncher(mac);
    expect(exec).toHaveBeenCalledTimes(1);
    const [binary, args, options] = exec.mock.calls[0]!;
    expect(binary).toBe('ssh');
    expect(args).toContain('BatchMode=yes');
    expect(args).toContain('ConnectTimeout=5');
    expect(args).toContain('--');
    expect(args).toContain('example-mac');
    const command = (args as string[]).at(-1)!;
    expect(command).toContain('test -d ');
    expect(command).toContain(`test "$console_uid" = "$current_uid" || exit 1`);
    expect(command).toContain(
      `exec /usr/bin/open '/Users/example/Applications/Claude (work'"'"'s account).app'`
    );
    expect(command).not.toContain('sudo');
    expect(command).not.toContain('launchctl');
    expect(command).not.toContain('kill');
    expect(options).toMatchObject({ timeout: 8000, maxBuffer: 1024 * 1024 });
  });

  it('reads only the fixed, size-bounded Mac usage history path', async () => {
    const sample = JSON.stringify({ version: 2, samples: [] });
    const exec = mockSsh(sample);
    expect(await readClaudeDesktopUsageHistory(mac, 'mac')).toBe(sample);
    const command = (exec.mock.calls[0]![1] as string[]).at(-1)!;
    expect(command).toContain(
      "'/Users/example/Library/Application Support/Claude-work/plan-usage-history.json'"
    );
    expect(command).toContain('test "$history_size" -le 1048576');
    expect(command).not.toContain('auth.json');
    expect(command).not.toContain('credentials');
  });

  it.each(['gmail', 'platyr', 'party', 'me', 'work', 'added-profile'])(
    'starts only the fixed same-user limited interactive Windows task for %s',
    async (id) => {
      const exec = mockSsh();
      await openClaudeWindowsLauncher(windows, id);
      const [binary, args] = exec.mock.calls[0]!;
      expect(binary).toBe('ssh');
      expect(args).toContain('example-windows');
      const command = (args as string[]).at(-1)!;
      const script = Buffer.from(command.split(' ').at(-1)!, 'base64').toString('utf16le');
      expect(script).toContain(
        "Get-ScheduledTask -TaskPath '\\' -TaskName 'ccs-claude-" + id + "'"
      );
      expect(script).toContain('$taskSid -ne $currentSid');
      expect(script).toContain("LogonType -notin @('Interactive', 'InteractiveToken')");
      expect(script).toContain("RunLevel -ne 'Limited'");
      expect(script).toContain("'CCS-Claude', 'ccs-claude.exe'");
      expect(script).toContain("'ccs-claude://launch/" + id + "'");
      expect(script).toContain('Start-ScheduledTask -InputObject $task');
      for (const forbidden of [
        'Start-Process',
        'Register-ScheduledTask',
        'password',
        windows.profilePath,
      ])
        expect(script).not.toContain(forbidden);
    }
  );

  // Manifest membership is enforced by the callers, which resolve the launcher
  // from the manifest; the transport only rejects unsafe strings before SSH.
  it.each(['gmail;anything', "gmail'", '../gmail', '', '-bad', 'bad id', 'x'.repeat(65)])(
    'rejects unsafe Windows profile %s before SSH',
    async (id) => {
      const exec = mockSsh();
      await expect(openClaudeWindowsLauncher(windows, id)).rejects.toThrow();
      expect(exec).not.toHaveBeenCalled();
    }
  );

  it('uses a safely encoded PowerShell literal path for Windows history', async () => {
    const exec = mockSsh('{"version":2,"samples":[]}');
    await readClaudeDesktopUsageHistory(windows, 'windows');
    const command = (exec.mock.calls[0]![1] as string[]).at(-1)!;
    expect(command).toStartWith('powershell.exe -NoProfile -NonInteractive -EncodedCommand ');
    const script = Buffer.from(command.split(' ').at(-1)!, 'base64').toString('utf16le');
    expect(script).toContain(
      "$history = 'C:\\Users\\example\\AppData\\Roaming\\Claude-work''s account\\plan-usage-history.json'"
    );
    expect(script).toContain('Get-Item -LiteralPath $history');
    expect(script).toContain('[IO.File]::ReadAllText($history)');
    expect(script).toContain('1048576');
    expect(script).not.toContain('Invoke-Expression');
    expect(script).not.toContain('credentials');
  });

  it('returns null for a remote missing file without inventing zero usage', async () => {
    mockSsh('CCS_DESKTOP_USAGE_MISSING\r\n');
    expect(await readClaudeDesktopUsageHistory(windows, 'windows')).toBeNull();
  });

  it('sanitizes failures and distinguishes timeout', async () => {
    let exec = mockSsh('', { code: 255 });
    const failure = await readClaudeDesktopUsageHistory(mac, 'mac').catch((error) => error);
    expect(failure).toBeInstanceOf(ClaudeDesktopTransportError);
    expect(failure.message).toBe('Claude desktop request failed.');
    expect(failure.timedOut).toBe(false);
    expect(failure.message).not.toContain('PRIVATE');
    exec.mockRestore();
    exec = mockSsh('', { killed: true, signal: 'SIGTERM' });
    const timeout = await openClaudeMacLauncher(mac).catch((error) => error);
    expect(timeout).toBeInstanceOf(ClaudeDesktopTransportError);
    expect(timeout.message).toBe('Claude desktop request timed out.');
    expect(timeout.timedOut).toBe(true);
  });

  it.each(['-oProxyCommand=anything', 'example;anything', 'user@example', 'example\nother'])(
    'rejects unsafe SSH aliases before executing (%s)',
    async (sshHost) => {
      const exec = mockSsh();
      await expect(openClaudeMacLauncher({ ...mac, sshHost })).rejects.toThrow();
      expect(exec).not.toHaveBeenCalled();
    }
  );

  it.each(['/tmp/launcher.sh', 'relative.app', '/tmp/bad\npath.app'])(
    'rejects unsafe Mac launcher paths before executing (%s)',
    async (launcherPath) => {
      const exec = mockSsh();
      await expect(openClaudeMacLauncher({ ...mac, launcherPath })).rejects.toThrow();
      expect(exec).not.toHaveBeenCalled();
    }
  );

  it('rejects relative remote usage directories', async () => {
    const exec = mockSsh();
    await expect(
      readClaudeDesktopUsageHistory({ ...windows, profilePath: 'relative' }, 'windows')
    ).rejects.toThrow();
    expect(exec).not.toHaveBeenCalled();
  });
});
