/**
 * The sign-in note names "Open on Windows" as the way to sign in. That control posts
 * /api/claude/desktop-profiles/<id>/open with platform "windows", which reaches
 * openClaudeDesktopProfile and then the Windows launcher for exactly this account's Claude
 * Desktop profile. Claude Desktop shows its own sign-in window when that profile is signed
 * out. The launcher is mocked: nothing opens on any desktop.
 */
import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as transport from '../../../src/web-server/services/claude-desktop-transport';
import { openClaudeDesktopProfile } from '../../../src/web-server/services/claude-desktop-open-service';
import { listClaudeDesktopProfileMetadata } from '../../../src/web-server/services/claude-desktop-profile-service';

describe('Open on Windows for a signed-out Claude Desktop profile', () => {
  let directory: string;
  let previous: string | undefined;
  const windows = {
    launcherName: 'Claude (fixture-signed-out)',
    launcherPath: 'C:\\Users\\fixture\\Desktop\\Claude (fixture-signed-out).lnk',
    profilePath: 'C:\\Users\\fixture\\AppData\\Roaming\\Claude-fixture-signed-out',
    sshHost: 'fixture-windows',
  };
  beforeEach(() => {
    directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-signin-open-'));
    previous = process.env.CCS_DIR;
    process.env.CCS_DIR = directory;
    fs.writeFileSync(
      path.join(directory, 'claude-desktop-profiles.json'),
      JSON.stringify({
        version: 1,
        profiles: [
          {
            id: 'fixture-other',
            email: 'other@example.test',
            windows: { ...windows, sshHost: 'fixture-windows' },
          },
          { id: 'fixture-signed-out', email: 'signed-out@example.test', windows },
        ],
      }),
      { mode: 0o600 }
    );
  });
  afterEach(() => {
    mock.restore();
    if (previous === undefined) delete process.env.CCS_DIR;
    else process.env.CCS_DIR = previous;
    fs.rmSync(directory, { recursive: true, force: true });
  });

  it('the row offers Open on Windows, and it launches only that profile', async () => {
    const launched: Array<{ launcher: unknown; id: string }> = [];
    spyOn(transport, 'openClaudeWindowsLauncher').mockImplementation(async (launcher, id) => {
      launched.push({ launcher, id });
    });
    spyOn(transport, 'openClaudeMacLauncher').mockImplementation(async () => {
      throw new Error('Mac launcher must not run');
    });
    const metadata = await listClaudeDesktopProfileMetadata();
    const row = metadata.find((profile) => profile.id === 'fixture-signed-out');
    expect(row?.windows).toMatchObject({ canOpen: true });

    await openClaudeDesktopProfile('fixture-signed-out', 'windows');
    expect(launched).toHaveLength(1);
    expect(launched[0].id).toBe('fixture-signed-out');
    expect(launched[0].launcher).toMatchObject({
      sshHost: 'fixture-windows',
      profilePath: windows.profilePath,
      launcherPath: windows.launcherPath,
    });
  });
});
