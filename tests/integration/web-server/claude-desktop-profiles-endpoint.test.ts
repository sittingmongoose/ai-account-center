import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import * as fs from 'fs';
import * as http from 'http';
import * as os from 'os';
import * as path from 'path';
import { authMiddleware } from '../../../src/web-server/middleware/auth-middleware';
import claudeDesktopRoutes from '../../../src/web-server/routes/claude-desktop-routes';
import { runWithScopedConfigDir } from '../../../src/utils/config-manager';

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

async function request(method = 'GET'): Promise<{ status: number; body: unknown }> {
  const response = await fetch(`http://127.0.0.1:${port}/api/claude/desktop-profiles`, { method });
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

  const app = express();
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
  if (originalAuthEnabled === undefined) delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  else process.env.CCS_DASHBOARD_AUTH_ENABLED = originalAuthEnabled;
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
