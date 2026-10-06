/**
 * Claude host steps (CONTRACT-registry-lifecycle 6.2, 6.7, 6.8). The Mac
 * program runs here for real against a temporary HOME through a local runner
 * (no ssh, no real host); the Windows script is checked as generated text.
 * Spawns child processes, so it is in the slow bucket.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import { spawn, type ChildProcess } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  SshClaudeHostTransport,
  windowsHostScript,
  type ClaudeHostRunner,
} from '../../../src/web-server/services/claude-host-transport';
import type { ClaudeHostLauncher } from '../../../src/web-server/services/claude-account-stores';

const dirs: string[] = [];
const children: ChildProcess[] = [];
afterEach(() => {
  for (const child of children.splice(0)) child.kill('SIGKILL');
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

function macHome() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-claude-mac-'));
  dirs.push(home);
  const calls: Array<{ sshHost: string; command: string; input: string }> = [];
  const runner: ClaudeHostRunner = (sshHost, command, input) =>
    new Promise((resolve, reject) => {
      calls.push({ sshHost, command, input });
      const child = spawn('/bin/sh', ['-c', command], {
        env: { PATH: process.env.PATH ?? '/usr/bin:/bin', HOME: home },
        stdio: ['pipe', 'pipe', 'pipe'],
      });
      let stdout = '';
      child.stdout.on('data', (chunk) => (stdout += chunk));
      child.on('close', (code) =>
        code === 0 ? resolve(stdout) : reject(new Error(`exit ${code}`))
      );
      child.stdin.end(input);
    });
  return { home, calls, transport: new SshClaudeHostTransport(runner) };
}

const support = (home: string) => path.join(home, 'Library', 'Application Support');

describe('Mac host program', () => {
  it('creates a data folder and launcher, and refuses to create them twice', async () => {
    const { home, calls, transport } = macHome();
    const launcher = await transport.create('mac', { profileId: 'work2', sshHost: 'jared-mac' });
    expect(launcher).toEqual({
      launcherName: 'Claude (work2)',
      launcherPath: path.join(home, 'Applications', 'Claude (work2).app'),
      profilePath: path.join(support(home), 'Claude-work2'),
      sshHost: 'jared-mac',
    });
    expect(fs.statSync(launcher.profilePath).mode & 0o777).toBe(0o700);
    const script = fs.readFileSync(
      path.join(launcher.launcherPath as string, 'Contents', 'MacOS', 'launch'),
      'utf8'
    );
    expect(script).toBe(
      `#!/bin/sh\nexec /usr/bin/open -n -a Claude --args '--user-data-dir=${launcher.profilePath}'\n`
    );
    const plist = fs.readFileSync(
      path.join(launcher.launcherPath as string, 'Contents', 'Info.plist'),
      'utf8'
    );
    expect(plist).toContain('com.aac.claudeprofile.work2');
    await expect(
      transport.create('mac', { profileId: 'work2', sshHost: 'jared-mac' })
    ).rejects.toThrow();
    // The command line holds only the fixed bootstrap; the request travels on stdin.
    expect(calls[0].command.startsWith('/usr/bin/python3 -c "import sys,json;')).toBe(true);
    expect(calls[0].command).not.toContain('work2');
    expect(JSON.parse(calls[0].input).request).toEqual({ op: 'create', profileId: 'work2' });
  });

  it('trashes on the same volume, restores, purges, and keeps a used folder on undo', async () => {
    const { home, transport } = macHome();
    const launcher = await transport.create('mac', { profileId: 'work2', sshHost: 'jared-mac' });
    fs.writeFileSync(path.join(launcher.profilePath, 'Local State'), 'signed in');
    const trashName = 'work2-20261002T080000Z';
    expect(await transport.trash('mac', { profileId: 'work2', launcher, trashName })).toBe('moved');
    const trashed = path.join(home, '.ccs', 'trash', 'claude', trashName);
    expect(fs.readFileSync(path.join(trashed, 'Local State'), 'utf8')).toBe('signed in');
    expect(fs.existsSync(launcher.profilePath)).toBe(false);
    expect(fs.existsSync(launcher.launcherPath as string)).toBe(false);
    await transport.restore('mac', { profileId: 'work2', launcher, trashName });
    expect(fs.readFileSync(path.join(launcher.profilePath, 'Local State'), 'utf8')).toBe(
      'signed in'
    );
    expect(
      fs.existsSync(path.join(launcher.launcherPath as string, 'Contents', 'Info.plist'))
    ).toBe(true);
    expect(fs.existsSync(trashed)).toBe(false);
    await transport.trash('mac', { profileId: 'work2', launcher, trashName });
    await transport.purge('mac', { sshHost: 'jared-mac', trashName });
    expect(fs.existsSync(trashed)).toBe(false);
    expect(fs.readdirSync(path.join(home, '.ccs', 'trash', 'claude'))).toEqual([]);
    // Undo removes the launcher, and the data folder only while it is empty.
    const second = await transport.create('mac', { profileId: 'work3', sshHost: 'jared-mac' });
    fs.writeFileSync(path.join(second.profilePath, 'keep'), 'x');
    await transport.undoCreate('mac', { profileId: 'work3', launcher: second });
    expect(fs.existsSync(second.launcherPath as string)).toBe(false);
    expect(fs.existsSync(second.profilePath)).toBe(true);
  });

  it('reports whether the profile app runs', async () => {
    const { home, transport } = macHome();
    const launcher = await transport.create('mac', { profileId: 'work2', sshHost: 'jared-mac' });
    expect(await transport.appState('mac', { launcher })).toBe('stopped');
    const child = spawn('/usr/bin/python3', [
      '-c',
      'import time; time.sleep(30)',
      `--user-data-dir=${launcher.profilePath}`,
    ]);
    children.push(child);
    await new Promise((resolve) => setTimeout(resolve, 200));
    expect(await transport.appState('mac', { launcher })).toBe('running');
    const sibling: ClaudeHostLauncher = {
      ...launcher,
      profilePath: path.join(support(home), 'Claude-work'),
    };
    expect(await transport.appState('mac', { launcher: sibling })).toBe('stopped');
  });

  it('reports the sign-in marker without decrypting or printing any token', async () => {
    const { transport } = macHome();
    const launcher = await transport.create('mac', { profileId: 'work2', sshHost: 'jared-mac' });
    const config = path.join(launcher.profilePath, 'config.json');
    // Fresh profile: no session file at all.
    expect(await transport.sessionState('mac', { launcher })).toBe('signed-out');
    // A session: account uuid plus an encrypted token cache (never printed).
    fs.writeFileSync(
      config,
      JSON.stringify({
        lastKnownAccountUuid: '12345678-90ab-cdef-1234-567890abcdef',
        'oauth:tokenCacheV2': 'djEwdGhpcyBpcyBub3QgYSByZWFsIHRva2Vu',
      })
    );
    expect(await transport.sessionState('mac', { launcher })).toBe('signed-in');
    // Either half missing reads as signed out, never as an error.
    fs.writeFileSync(
      config,
      JSON.stringify({ lastKnownAccountUuid: '12345678-90ab-cdef-1234-567890abcdef' })
    );
    expect(await transport.sessionState('mac', { launcher })).toBe('signed-out');
    fs.writeFileSync(config, '{oops');
    expect(await transport.sessionState('mac', { launcher })).toBe('signed-out');
  });

  it('refuses paths outside the fixed folders before touching anything', async () => {
    const { home, transport } = macHome();
    const outside = path.join(home, 'outside');
    fs.mkdirSync(outside);
    fs.writeFileSync(path.join(outside, 'precious'), 'x');
    const bad: ClaudeHostLauncher[] = [
      {
        launcherName: 'x',
        launcherPath: path.join(home, 'Applications', 'Claude (x).app'),
        profilePath: outside,
        sshHost: 'jared-mac',
      },
      {
        launcherName: 'x',
        launcherPath: path.join(home, 'Applications', 'Claude (x).app'),
        profilePath: path.join(support(home), '..', 'outside'),
        sshHost: 'jared-mac',
      },
      {
        launcherName: 'x',
        launcherPath: outside,
        profilePath: path.join(support(home), 'Claude-x'),
        sshHost: 'jared-mac',
      },
    ];
    for (const launcher of bad) {
      await expect(
        transport.trash('mac', {
          profileId: 'work2',
          launcher,
          trashName: 'work2-20261002T080000Z',
        })
      ).rejects.toThrow();
    }
    await expect(
      transport.purge('mac', { sshHost: 'jared-mac', trashName: '../../outside' })
    ).rejects.toThrow();
    expect(fs.readFileSync(path.join(outside, 'precious'), 'utf8')).toBe('x');
  });
});

describe('Windows host script', () => {
  const launcher: ClaudeHostLauncher = {
    launcherName: 'Claude (party)',
    launcherPath: 'C:\\Users\\x\\Desktop\\Claude (party@example.com).lnk',
    startMenuPath:
      'C:\\Users\\x\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Claude (party).lnk',
    profilePath: 'C:\\Users\\x\\AppData\\Roaming\\Claude-party',
    sshHost: 'jared-windows',
  };

  it('embeds only validated values and checks them again on the host', () => {
    const script = windowsHostScript('trash', {
      profileId: 'party',
      trashName: 'party-20261002T080000Z',
      launcher,
    });
    expect(script).toContain("$id = 'party'");
    expect(script).toContain("Join-Path $trashRoot 'party-20261002T080000Z'");
    expect(script).toContain(`$profile = Check-Profile '${launcher.profilePath}'`);
    // Letters beyond ASCII still pass the allowlist.
    expect(
      windowsHostScript('state', {
        launcher: { ...launcher, profilePath: 'C:\\Users\\José\\AppData\\Roaming\\Claude-party' },
      })
    ).toContain("Check-Profile 'C:\\Users\\José\\AppData\\Roaming\\Claude-party'");
    expect(script).toContain('[IO.Directory]::Move($profile, $target)');
    expect(script).toContain("Done @{ result = 'cross_volume' }");
    const create = windowsHostScript('create', { profileId: 'work2' });
    expect(create).toContain('Make-Task $id');
    expect(create).toContain("'ccs-claude://launch/' + $id");
  });

  it('keeps the helper allowlist with the profile: create adds the id, undo drops it', () => {
    const create = windowsHostScript('create', { profileId: 'work2' });
    // The allowlist lives beside the helper, under the name the C# helper reads.
    expect(create).toContain(
      "$accountsFile = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude', 'ccs-claude-accounts.txt')"
    );
    // Only the validated $id reaches the file lines.
    expect(create).toContain("$id = 'work2'");
    expect(create).toContain('Make-Task $id; Add-AccountId $id');
    expect(create).toContain('Drop-Task $id; Drop-AccountId $id');
    // Append, never reorder: an id already listed stays where it is (the
    // default stays first), and a file without a trailing newline is fixed first.
    expect(create).toContain('if ($line.Trim() -ceq $id) { return }');
    expect(create).toContain('Add-Content -LiteralPath $accountsFile -Value $id -Encoding UTF8');
    // A missing list is never started by a new profile (it would become the
    // default Store profile); the next Open rebuilds the list instead.
    expect(create).toContain(
      'function Add-AccountId([string]$id) { if (-not (Test-Path -LiteralPath $accountsFile -PathType Leaf)) { return }'
    );
    expect(create).not.toContain('New-Item -ItemType Directory -Path $dir');
    const undo = windowsHostScript('undo', { profileId: 'work2', launcher });
    expect(undo).toContain('Drop-AccountId $id');
    // Undo takes out only that exact line, and deletes the file when emptied.
    expect(undo).toContain('$_.Trim() -cne $id');
    expect(undo).toContain('Remove-Item -LiteralPath $accountsFile -Force');
    // Nothing else touches the allowlist: trash keeps the id listed.
    for (const op of ['trash', 'restore'] as const) {
      const script = windowsHostScript(op, {
        profileId: 'party',
        trashName: 'party-20261002T080000Z',
        launcher,
      });
      expect(script).not.toContain('Add-AccountId $id');
      expect(script).not.toContain('Drop-AccountId $id');
    }
  });

  it('rejects quotes, variables and malformed ids before building anything', () => {
    for (const profilePath of [
      "C:\\x\\Claude-a'; Remove-Item C:\\",
      'C:\\x\\$env:TEMP',
      'C:\\x\\`n',
      // PowerShell also ends a single-quoted string at the typographic single quotes.
      'C:\\x\\Claude-a\u2018; Remove-Item C:\\',
      'C:\\x\\Claude-a\u2019; Remove-Item C:\\',
      'C:\\x\\Claude-a\u201a; exit',
      'C:\\x\\Claude-a\u201b; exit',
      'C:\\x\\Claude-a\u201c\u201d',
      'C:\\x\\Claude\r\nexit',
    ]) {
      expect(() =>
        windowsHostScript('state', { launcher: { ...launcher, profilePath } })
      ).toThrow();
    }
    expect(() => windowsHostScript('create', { profileId: "x'; exit" })).toThrow();
    expect(() => windowsHostScript('purge', { trashName: '..\\..\\Windows' })).toThrow();
  });

  it('sends the script on stdin behind a fixed encoded bootstrap', async () => {
    const calls: Array<{ command: string; input: string }> = [];
    const transport = new SshClaudeHostTransport(async (_host, command, input) => {
      calls.push({ command, input });
      return '{"running":false}';
    });
    expect(await transport.appState('windows', { launcher })).toBe('stopped');
    expect(calls[0].command).toMatch(
      /^powershell\.exe -NoProfile -NonInteractive -EncodedCommand [A-Za-z0-9+/=]+$/
    );
    const bootstrap = Buffer.from(calls[0].command.split(' ').pop() ?? '', 'base64').toString(
      'utf16le'
    );
    expect(bootstrap).toBe(
      '$s = [Console]::In.ReadToEnd(); & ([scriptblock]::Create($s)); exit $LASTEXITCODE'
    );
    expect(calls[0].input).toContain(
      "Check-Profile 'C:\\Users\\x\\AppData\\Roaming\\Claude-party'"
    );
  });
});
