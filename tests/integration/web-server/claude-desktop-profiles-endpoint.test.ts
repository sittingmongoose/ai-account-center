import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import express from 'express';
import * as fs from 'fs';
import * as http from 'http';
import * as os from 'os';
import * as path from 'path';
import { authMiddleware } from '../../../src/web-server/middleware/auth-middleware';
import claudeDesktopRoutes from '../../../src/web-server/routes/claude-desktop-routes';
import { runWithScopedConfigDir } from '../../../src/utils/config-manager';
import * as transport from '../../../src/web-server/services/claude-desktop-transport';
import { invalidateClaudeDesktopUsageCache } from '../../../src/web-server/services/claude-desktop-usage-service';

let tempDir: string;
let server: http.Server;
let port: number;
let authenticated = false;
let originalAuthEnabled: string | undefined;

const fakeProfiles = [
  {
    email: 'work@example.com',
    mac: {
      launcherName: 'Claude (work@example.com).app',
      launcherPath: '/Users/example/Applications/Claude (work@example.com).app',
      profilePath: '/Users/example/Library/Application Support/ClaudeProfiles/work',
      isDefault: true,
    },
    windows: {
      launcherName: 'Claude - work@example.com.lnk',
      launcherPath: 'C:\\Users\\example\\Desktop\\Claude - work@example.com.lnk',
      startMenuPath: 'C:\\Users\\example\\Start Menu\\Claude - work@example.com.lnk',
      profilePath: 'C:\\Users\\example\\AppData\\Roaming\\ClaudeProfiles\\work',
      isDefault: false,
    },
  },
];

function writeManifest(value: unknown): void {
  fs.writeFileSync(path.join(tempDir, 'claude-desktop-profiles.json'), JSON.stringify(value));
}

async function request(
  method = 'GET',
  suffix = '',
  options: { body?: string; headers?: Record<string, string> } = {}
): Promise<{ status: number; body: unknown }> {
  const response = await fetch(`http://127.0.0.1:${port}/api/claude/desktop-profiles${suffix}`, {
    method,
    ...options,
  });
  const contents = await response.text();
  return {
    status: response.status,
    body: response.headers.get('content-type')?.includes('application/json')
      ? JSON.parse(contents)
      : contents,
  };
}

beforeEach(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-claude-desktop-'));
  originalAuthEnabled = process.env.CCS_DASHBOARD_AUTH_ENABLED;
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
  authenticated = false;
  invalidateClaudeDesktopUsageCache();

  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    if (authenticated) req.session = { authenticated: true } as express.Request['session'];
    void runWithScopedConfigDir(tempDir, () => next());
  });
  app.use(authMiddleware);
  app.use('/api/claude', claudeDesktopRoutes);

  server = await new Promise<http.Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  port = (server.address() as { port: number }).port;
});

afterEach(async () => {
  await new Promise<void>((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
  fs.rmSync(tempDir, { recursive: true, force: true });
  mock.restore();
  if (originalAuthEnabled === undefined) delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  else process.env.CCS_DASHBOARD_AUTH_ENABLED = originalAuthEnabled;
});

describe('Claude desktop launch and cached usage', () => {
  function writeLaunchProfile() {
    writeManifest({
      version: 1,
      profiles: [
        {
          ...fakeProfiles[0],
          id: 'work',
          mac: { ...fakeProfiles[0]!.mac, sshHost: 'example-mac' },
          windows: { ...fakeProfiles[0]!.windows, sshHost: 'example-windows' },
        },
      ],
    });
  }

  function openOptions(body: unknown = { platform: 'mac' }) {
    return { body: JSON.stringify(body), headers: { 'Content-Type': 'application/json' } };
  }

  it('provides derived launch controls without exposing SSH aliases', async () => {
    writeLaunchProfile();
    const response = await request();
    expect(response.status).toBe(200);
    expect((response.body as { profiles: unknown[] }).profiles).toEqual([
      {
        ...fakeProfiles[0],
        id: 'work',
        mac: { ...fakeProfiles[0]!.mac, canOpen: true },
        windows: {
          ...fakeProfiles[0]!.windows,
          canOpen: true,
          launchUri: 'ccs-claude://launch/work',
        },
        openOperation: null,
      },
    ]);
    expect(JSON.stringify(response.body)).not.toContain('example-mac');
  });

  it('launches only the configured Mac app with an authenticated session', async () => {
    writeLaunchProfile();
    authenticated = true;
    const launch = spyOn(transport, 'openClaudeMacLauncher').mockResolvedValue(undefined);
    expect(await request('POST', '/work/open', openOptions())).toEqual({
      status: 200,
      body: { opened: true, id: 'work', platform: 'mac' },
    });
    expect(launch).toHaveBeenCalledWith({ ...fakeProfiles[0]!.mac, sshHost: 'example-mac' });
  });

  it('requires a session even on localhost with dashboard authentication disabled', async () => {
    writeLaunchProfile();
    const launch = spyOn(transport, 'openClaudeMacLauncher').mockResolvedValue(undefined);
    expect((await request('POST', '/work/open', openOptions())).status).toBe(401);
    expect(launch).not.toHaveBeenCalled();
  });

  it('launches only a manifest-configured Windows task with a same-origin session', async () => {
    authenticated = true;
    writeManifest({
      version: 1,
      profiles: [
        {
          ...fakeProfiles[0],
          id: 'gmail',
          mac: { ...fakeProfiles[0]!.mac, sshHost: 'example-mac' },
          windows: { ...fakeProfiles[0]!.windows, sshHost: 'example-windows' },
        },
      ],
    });
    const launch = spyOn(transport, 'openClaudeWindowsLauncher').mockResolvedValue(undefined);
    expect(await request('POST', '/gmail/open', openOptions({ platform: 'windows' }))).toEqual({
      status: 200,
      body: { opened: true, id: 'gmail', platform: 'windows' },
    });
    expect(launch).toHaveBeenCalledWith(
      { ...fakeProfiles[0]!.windows, sshHost: 'example-windows' },
      'gmail'
    );
    const badOrigin = openOptions({ platform: 'windows' });
    badOrigin.headers = { ...badOrigin.headers, Origin: 'https://attacker.example.com' };
    expect((await request('POST', '/gmail/open', badOrigin)).status).toBe(403);
    expect(
      (
        await request(
          'POST',
          '/gmail/open',
          openOptions({ platform: 'windows', command: 'anything' })
        )
      ).status
    ).toBe(400);
    expect(
      (await request('POST', '/absent-id/open', openOptions({ platform: 'windows' }))).status
    ).toBe(404);
    expect(launch).toHaveBeenCalledTimes(1);
  });

  it('opens a new manifest ID on Windows end to end while an absent ID is refused', async () => {
    authenticated = true;
    writeManifest({
      version: 1,
      profiles: [
        {
          ...fakeProfiles[0],
          id: 'added-profile',
          mac: { ...fakeProfiles[0]!.mac, sshHost: 'example-mac' },
          windows: { ...fakeProfiles[0]!.windows, sshHost: 'example-windows' },
        },
      ],
    });
    const launch = spyOn(transport, 'openClaudeWindowsLauncher').mockResolvedValue(undefined);
    expect(
      await request('POST', '/added-profile/open', openOptions({ platform: 'windows' }))
    ).toEqual({
      status: 200,
      body: { opened: true, id: 'added-profile', platform: 'windows' },
    });
    expect(launch).toHaveBeenCalledWith(
      { ...fakeProfiles[0]!.windows, sshHost: 'example-windows' },
      'added-profile'
    );
    expect(
      (await request('POST', '/no-such-profile/open', openOptions({ platform: 'windows' })))
        .status
    ).toBe(404);
    expect(launch).toHaveBeenCalledTimes(1);
  });

  it('rejects cross-origin and non-JSON launch requests before SSH', async () => {
    writeLaunchProfile();
    authenticated = true;
    const launch = spyOn(transport, 'openClaudeMacLauncher').mockResolvedValue(undefined);
    const crossOrigin = openOptions();
    crossOrigin.headers = { ...crossOrigin.headers, Origin: 'https://attacker.example.com' };
    expect((await request('POST', '/work/open', crossOrigin)).status).toBe(403);
    expect((await request('POST', '/work/open', { body: 'platform=mac' })).status).toBe(415);
    expect(launch).not.toHaveBeenCalled();
  });

  it('rejects client hosts, paths, commands and identifiers', async () => {
    writeLaunchProfile();
    authenticated = true;
    const launch = spyOn(transport, 'openClaudeMacLauncher').mockResolvedValue(undefined);
    const windowsLaunch = spyOn(transport, 'openClaudeWindowsLauncher').mockResolvedValue(
      undefined
    );
    for (const body of [
      { platform: 'windows', host: 'attacker' },
      { platform: 'mac', host: 'attacker' },
      { platform: 'mac', path: '/tmp/untrusted.app' },
      { platform: 'mac', command: 'anything' },
    ]) {
      expect((await request('POST', '/work/open', openOptions(body))).status).toBe(400);
    }
    expect((await request('POST', '/bad%3Bid/open', openOptions())).status).toBe(400);
    expect(launch).not.toHaveBeenCalled();
    expect(windowsLaunch).not.toHaveBeenCalled();
  });

  it('distinguishes missing and unconfigured profiles', async () => {
    authenticated = true;
    writeManifest({ version: 1, profiles: [{ ...fakeProfiles[0], id: 'work' }] });
    const launch = spyOn(transport, 'openClaudeMacLauncher').mockResolvedValue(undefined);
    expect((await request('POST', '/unknown/open', openOptions())).status).toBe(404);
    expect((await request('POST', '/work/open', openOptions())).status).toBe(409);
    expect(launch).not.toHaveBeenCalled();
  });

  it('returns safe upstream failure and timeout statuses', async () => {
    writeLaunchProfile();
    authenticated = true;
    const launch = spyOn(transport, 'openClaudeMacLauncher');
    launch.mockRejectedValueOnce(new transport.ClaudeDesktopTransportError());
    expect(await request('POST', '/work/open', openOptions())).toEqual({
      status: 502,
      body: { error: 'Claude desktop request failed.' },
    });
    launch.mockRejectedValueOnce(new transport.ClaudeDesktopTransportError(true));
    expect(await request('POST', '/work/open', openOptions())).toEqual({
      status: 504,
      body: { error: 'Claude desktop request timed out.' },
    });
  });

  it('coalesces double clicks and does not close existing applications', async () => {
    writeLaunchProfile();
    authenticated = true;
    const launch = spyOn(transport, 'openClaudeMacLauncher').mockImplementation(
      () => new Promise((resolve) => setTimeout(resolve, 30))
    );
    const responses = await Promise.all([
      request('POST', '/work/open', openOptions()),
      request('POST', '/work/open', openOptions()),
    ]);
    expect(responses.map((response) => response.status)).toEqual([200, 200]);
    expect(launch).toHaveBeenCalledTimes(1);
    expect((await request('POST', '/work/open', openOptions())).status).toBe(200);
    expect(launch).toHaveBeenCalledTimes(1);
  });

  it('validates usage platform and returns whitelisted cached percentages', async () => {
    writeLaunchProfile();
    const read = spyOn(transport, 'readClaudeDesktopUsageHistory').mockResolvedValue(
      JSON.stringify({
        version: 2,
        samples: [{ t: 1790000000000, org: 'PRIVATE_ORGANIZATION', u: { fh: 0, sd: 100 } }],
      })
    );
    expect((await request('GET', '/usage?platform=other')).status).toBe(400);
    const response = await request('GET', '/usage?platform=windows');
    expect(response.status).toBe(200);
    const body = response.body as {
      profiles: Array<{ status: string; utilization: unknown; sampledAt: string }>;
    };
    expect(body.profiles[0]?.status).toBe('cached');
    expect(body.profiles[0]?.utilization).toEqual({ fiveHour: 0, weekly: 100 });
    expect(body.profiles[0]?.sampledAt).toBe(new Date(1790000000000).toISOString());
    expect(JSON.stringify(response.body)).not.toContain('PRIVATE_ORGANIZATION');
    expect(read).toHaveBeenCalledWith(
      { ...fakeProfiles[0]!.windows, sshHost: 'example-windows' },
      'windows'
    );
  });

  it('protects metadata and usage from non-local access when authentication is disabled', async () => {
    writeLaunchProfile();
    const read = spyOn(transport, 'readClaudeDesktopUsageHistory').mockResolvedValue(null);
    const options = { headers: { Host: '192.0.2.1:3000' } };
    expect((await request('GET', '', options)).status).toBe(403);
    expect((await request('GET', '/usage?platform=mac', options)).status).toBe(403);
    expect(read).not.toHaveBeenCalled();
  });

  it.each([
    { id: 'bad;id' },
    { id: '-bad' },
    { id: 'work', mac: { ...fakeProfiles[0]!.mac, sshHost: '-oProxyCommand=anything' } },
  ])(
    'rejects malformed identifiers and aliases without exposing their values (%#)',
    async (changes) => {
      writeManifest({ version: 1, profiles: [{ ...fakeProfiles[0], ...changes }] });
      expect(await request()).toEqual({
        status: 500,
        body: { error: 'Claude desktop profiles could not be read safely.' },
      });
    }
  );

  it('rejects duplicate profile IDs', async () => {
    writeManifest({
      version: 1,
      profiles: [
        { ...fakeProfiles[0], id: 'work' },
        { ...fakeProfiles[0], id: 'work' },
      ],
    });
    expect((await request()).status).toBe(500);
  });
});

describe('GET /api/claude/desktop-profiles', () => {
  it('returns an empty inventory when the manifest is absent', async () => {
    expect(await request()).toEqual({ status: 200, body: { profiles: [] } });
  });

  it('returns Mac and Windows metadata without checking remote paths', async () => {
    writeManifest({ version: 1, profiles: fakeProfiles });
    expect(await request()).toEqual({ status: 200, body: { profiles: fakeProfiles } });
  });

  it('accepts one platform and optional fields', async () => {
    const profiles = [{ email: 'personal@example.com', windows: { launcherName: 'Claude.lnk' } }];
    writeManifest({ version: 1, profiles });
    expect(await request()).toEqual({ status: 200, body: { profiles } });
  });

  it('never exposes unknown manifest, profile, or launcher fields', async () => {
    const secret = 'PRIVATE_METADATA_MUST_NOT_APPEAR';
    writeManifest({
      version: 1,
      password: secret,
      profiles: [
        {
          ...fakeProfiles[0],
          credentials: secret,
          mac: { ...fakeProfiles[0]!.mac, token: secret },
        },
      ],
    });
    const response = await request();
    expect(response).toEqual({ status: 200, body: { profiles: fakeProfiles } });
    expect(JSON.stringify(response.body)).not.toContain(secret);
  });

  it('requires authentication when dashboard authentication is enabled', async () => {
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
    writeManifest({ version: 1, profiles: fakeProfiles });
    expect(await request()).toEqual({
      status: 401,
      body: { error: 'Authentication required' },
    });
    expect((await request('GET', '/usage?platform=mac')).status).toBe(401);
    authenticated = true;
    expect(await request()).toEqual({ status: 200, body: { profiles: fakeProfiles } });
  });

  it('does not provide a write endpoint', async () => {
    writeManifest({ version: 1, profiles: fakeProfiles });
    const before = fs.readFileSync(path.join(tempDir, 'claude-desktop-profiles.json'), 'utf8');
    expect((await request('POST')).status).toBe(404);
    expect(fs.readFileSync(path.join(tempDir, 'claude-desktop-profiles.json'), 'utf8')).toBe(
      before
    );
  });

  it('returns a sanitized error for invalid JSON', async () => {
    fs.writeFileSync(path.join(tempDir, 'claude-desktop-profiles.json'), 'PRIVATE_INVALID_JSON{');
    expect(await request()).toEqual({
      status: 500,
      body: { error: 'Claude desktop profiles could not be read safely.' },
    });
  });

  it.each([
    { version: 2, profiles: fakeProfiles },
    { version: 1, profiles: {} },
    { version: 1, profiles: [{ email: 'invalid', mac: { launcherName: 'Claude' } }] },
    { version: 1, profiles: [{ email: 'work@example.com' }] },
    { version: 1, profiles: [{ email: 'work@example.com', mac: null }] },
    { version: 1, profiles: [{ email: 'work@example.com', mac: { launcherName: '' } }] },
    {
      version: 1,
      profiles: [{ email: 'work@example.com', mac: { launcherName: 'Claude', launcherPath: 4 } }],
    },
    {
      version: 1,
      profiles: [{ email: 'work@example.com', mac: { launcherName: 'Claude', isDefault: 'yes' } }],
    },
    {
      version: 1,
      profiles: [
        {
          email: 'work@example.com',
          windows: { launcherName: 'Claude', startMenuPath: 'bad\npath' },
        },
      ],
    },
  ])('rejects malformed metadata with a sanitized error (%#)', async (manifest) => {
    writeManifest(manifest);
    expect(await request()).toEqual({
      status: 500,
      body: { error: 'Claude desktop profiles could not be read safely.' },
    });
  });
});
