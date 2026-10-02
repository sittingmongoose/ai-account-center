import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as transport from '../../../src/web-server/services/claude-desktop-transport';
import {
  getClaudeDesktopUsage,
  invalidateClaudeDesktopUsageCache,
} from '../../../src/web-server/services/claude-desktop-usage-service';

const profile = {
  id: 'fixture-account',
  email: 'fixture@example.com',
  mac: {
    launcherName: 'Fixture Mac',
    sshHost: 'fixture-mac',
    profilePath: '/fixture/claude',
  },
  windows: {
    launcherName: 'Fixture Windows',
    sshHost: 'fixture-windows',
    profilePath: 'C:\\fixture\\claude',
  },
};

function history(samples: unknown[]): string {
  return JSON.stringify({ version: 2, samples });
}

describe('Claude desktop cached usage', () => {
  let tempRoot: string;
  let originalCcsDir: string | undefined;
  let readHistory: ReturnType<typeof spyOn<typeof transport, 'readClaudeDesktopUsageHistory'>>;

  function writeProfiles(profiles: unknown[] = [profile]): void {
    const configDir = process.env.CCS_DIR as string;
    fs.mkdirSync(configDir, { recursive: true });
    fs.writeFileSync(
      path.join(configDir, 'claude-desktop-profiles.json'),
      JSON.stringify({ version: 1, profiles })
    );
  }

  beforeEach(() => {
    tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-desktop-usage-test-'));
    originalCcsDir = process.env.CCS_DIR;
    process.env.CCS_DIR = path.join(tempRoot, 'first');
    writeProfiles();
    invalidateClaudeDesktopUsageCache();
    readHistory = spyOn(transport, 'readClaudeDesktopUsageHistory').mockResolvedValue(
      history([{ t: 1000, u: { fh: 10 } }])
    );
  });

  afterEach(() => {
    invalidateClaudeDesktopUsageCache();
    mock.restore();
    if (originalCcsDir === undefined) delete process.env.CCS_DIR;
    else process.env.CCS_DIR = originalCcsDir;
    fs.rmSync(tempRoot, { recursive: true, force: true });
  });

  it('uses the newest valid timestamp, without merging older usage or leaking raw fields', async () => {
    readHistory.mockResolvedValue(
      history([
        { t: 3000, org: 'private-org-sentinel', u: { fh: 0, sd: 100, so: 32, sn: 24, xu: 150 } },
        { t: 1000, u: { fh: 75, sd: 45, unrecognized: 'credential-sentinel' } },
        { t: '9000', u: { fh: 99 } },
        { t: 2000, u: { fh: 11 }, resetTime: 'private-reset-sentinel' },
      ])
    );

    const result = await getClaudeDesktopUsage('mac');
    expect(result.platform).toBe('mac');
    expect(new Date(result.fetchedAt).toISOString()).toBe(result.fetchedAt);
    expect(result.profiles).toEqual([
      {
        id: profile.id,
        email: profile.email,
        status: 'cached',
        cached: true,
        fetchedAt: result.fetchedAt,
        sampledAt: '1970-01-01T00:00:03.000Z',
        utilization: { fiveHour: 0, weekly: 100, weeklyOpus: 32, weeklySonnet: 24, extra: 150 },
      },
    ]);
    expect(readHistory).toHaveBeenCalledWith(profile.mac, 'mac');
    const serialized = JSON.stringify(result);
    for (const forbidden of [
      'private-org-sentinel',
      'credential-sentinel',
      'private-reset-sentinel',
      'sshHost',
      'profilePath',
      'resetTime',
      'unrecognized',
    ]) {
      expect(serialized).not.toContain(forbidden);
    }
  });

  it('keeps absent fields absent while preserving a real zero', async () => {
    readHistory.mockResolvedValue(
      history([
        { t: 1000, u: { fh: 90, sd: 90, so: 90, sn: 90 } },
        { t: 2000, u: { fh: 0, sd: null, so: -1, sn: '0', xu: 2.5 } },
      ])
    );
    const [usage] = (await getClaudeDesktopUsage('mac')).profiles;
    expect(usage.status).toBe('cached');
    expect(usage.utilization).toEqual({ fiveHour: 0, extra: 2.5 });
    expect(Object.hasOwn(usage.utilization, 'weekly')).toBe(false);
  });

  it('ignores negative, nonfinite, fractional, and out-of-range timestamps', async () => {
    readHistory.mockResolvedValue(
      history([
        { t: 1000, u: { fh: 7 } },
        { t: -2000, u: { fh: 10 } },
        { t: Infinity, u: { fh: 20 } },
        { t: 2500.5, u: { fh: 30 } },
        { t: 9e20, u: { fh: 40 } },
      ])
    );
    const [usage] = (await getClaudeDesktopUsage('mac')).profiles;
    expect(usage.sampledAt).toBe('1970-01-01T00:00:01.000Z');
    expect(usage.utilization).toEqual({ fiveHour: 7 });
  });

  it('does not fall back to older metrics when the latest sample has no usable values', async () => {
    readHistory.mockResolvedValue(
      history([
        { t: 1000, u: { fh: 7 } },
        { t: 2000, u: { fh: null, sd: -1, so: '0', sn: false, xu: -4 } },
      ])
    );
    const [usage] = (await getClaudeDesktopUsage('mac')).profiles;
    expect(usage.status).toBe('unavailable');
    expect(usage.sampledAt).toBeNull();
    expect(usage.utilization).toEqual({});
  });

  it.each([null, history([])])('reports absent cache as needs-sign-in (%j)', async (contents) => {
    readHistory.mockResolvedValue(contents);
    const [usage] = (await getClaudeDesktopUsage('mac')).profiles;
    expect(usage.status).toBe('needs-sign-in');
    expect(usage.cached).toBe(true);
    expect(usage.sampledAt).toBeNull();
    expect(usage.utilization).toEqual({});
  });

  it.each([
    '{broken',
    JSON.stringify({ version: 1, samples: [] }),
    JSON.stringify({ version: 2, samples: {} }),
    history([null, { t: 'invalid', u: { fh: 4 } }]),
    history([{ t: 1000 }]),
    history([{ t: 1000, u: { fh: 1e400 } }]),
    ' '.repeat(1024 * 1024 + 1),
  ])('reports malformed or unusable history as unavailable (case %#)', async (contents) => {
    readHistory.mockResolvedValue(contents);
    const [usage] = (await getClaudeDesktopUsage('mac')).profiles;
    expect(usage.status).toBe('unavailable');
    expect(usage.cached).toBe(true);
    expect(usage.sampledAt).toBeNull();
    expect(usage.utilization).toEqual({});
  });

  it('sanitizes transport failures without failing other profiles', async () => {
    writeProfiles([profile, { ...profile, id: 'second', email: 'second@example.com' }]);
    readHistory
      .mockRejectedValueOnce(new Error('SSH credentials/path-private-sentinel'))
      .mockResolvedValueOnce(history([{ t: 1000, u: { fh: 35 } }]));
    const result = await getClaudeDesktopUsage('mac');
    expect(result.profiles.map((usage) => usage.status)).toEqual(['unavailable', 'cached']);
    expect(JSON.stringify(result)).not.toContain('private-sentinel');
  });

  it('does not attempt transport when a platform or its SSH configuration is missing', async () => {
    writeProfiles([
      { email: 'no-mac@example.com', windows: profile.windows },
      { email: 'no-ssh@example.com', mac: { launcherName: 'No SSH', profilePath: '/fixture' } },
      { email: 'no-path@example.com', mac: { launcherName: 'No path', sshHost: 'fixture-mac' } },
    ]);
    const result = await getClaudeDesktopUsage('mac');
    expect(result.profiles.map((usage) => usage.status)).toEqual([
      'unavailable',
      'unavailable',
      'unavailable',
    ]);
    expect(readHistory).not.toHaveBeenCalled();
    expect(Object.hasOwn(result.profiles[0], 'id')).toBe(false);
  });

  it('fetches all profiles concurrently and coalesces overlapping dashboard requests', async () => {
    writeProfiles([profile, { ...profile, id: 'second', email: 'second@example.com' }]);
    const releases: Array<(contents: string) => void> = [];
    readHistory.mockImplementation(() => new Promise<string>((resolve) => releases.push(resolve)));
    const first = getClaudeDesktopUsage('mac');
    const second = getClaudeDesktopUsage('mac');
    // One setTimeout(0) raced the manifest read when many test files share one
    // process. Wait (bounded) for both reads, then settle once more so a third
    // or fourth read from a broken coalescer would still be counted.
    for (let tries = 0; readHistory.mock.calls.length < 2 && tries < 200; tries += 1) {
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(readHistory).toHaveBeenCalledTimes(2);
    expect(releases).toHaveLength(2);
    for (const release of releases) release(history([{ t: 1000, u: { fh: 4 } }]));
    expect(await second).toBe(await first);
  });

  it('reuses results for 30 seconds and refreshes once the TTL expires', async () => {
    let now = 1000;
    spyOn(Date, 'now').mockImplementation(() => now);
    const first = await getClaudeDesktopUsage('mac');
    now += 29_999;
    expect(await getClaudeDesktopUsage('mac')).toBe(first);
    expect(readHistory).toHaveBeenCalledTimes(1);
    now += 1;
    expect(await getClaudeDesktopUsage('mac')).not.toBe(first);
    expect(readHistory).toHaveBeenCalledTimes(2);
  });

  it('invalidates on request and on manifest changes', async () => {
    const first = await getClaudeDesktopUsage('mac');
    invalidateClaudeDesktopUsageCache();
    expect(await getClaudeDesktopUsage('mac')).not.toBe(first);
    writeProfiles([{ ...profile, mac: { ...profile.mac, profilePath: '/changed/fixture' } }]);
    await getClaudeDesktopUsage('mac');
    expect(readHistory).toHaveBeenCalledTimes(3);
    expect(readHistory.mock.calls[2][0].profilePath).toBe('/changed/fixture');
  });

  it('scopes the cache by platform and configuration directory', async () => {
    const mac = await getClaudeDesktopUsage('mac');
    const windows = await getClaudeDesktopUsage('windows');
    expect(windows.platform).toBe('windows');
    expect(readHistory).toHaveBeenCalledWith(profile.windows, 'windows');
    process.env.CCS_DIR = path.join(tempRoot, 'second');
    writeProfiles();
    expect(await getClaudeDesktopUsage('mac')).not.toBe(mac);
    expect(readHistory).toHaveBeenCalledTimes(3);
  });

  it('bounds cached manifest revisions to 16 entries', async () => {
    const first = await getClaudeDesktopUsage('mac');
    for (let i = 1; i <= 16; i += 1) {
      writeProfiles([{ ...profile, id: `revision-${i}` }]);
      await getClaudeDesktopUsage('mac');
    }
    writeProfiles();
    expect(await getClaudeDesktopUsage('mac')).not.toBe(first);
    expect(readHistory).toHaveBeenCalledTimes(18);
  });

  it('returns an empty inventory when the manifest is absent', async () => {
    fs.unlinkSync(path.join(process.env.CCS_DIR as string, 'claude-desktop-profiles.json'));
    expect((await getClaudeDesktopUsage('mac')).profiles).toEqual([]);
    expect(readHistory).not.toHaveBeenCalled();
  });

  it('does not serve a cached success after the manifest becomes malformed', async () => {
    await getClaudeDesktopUsage('mac');
    fs.writeFileSync(
      path.join(process.env.CCS_DIR as string, 'claude-desktop-profiles.json'),
      '{broken'
    );
    await expect(getClaudeDesktopUsage('mac')).rejects.toThrow();
    expect(readHistory).toHaveBeenCalledTimes(1);
  });
});
