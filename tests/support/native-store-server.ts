import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import path from 'node:path';
import bcrypt from 'bcrypt';
import { startRustFixture } from '../spec/m6-rust-fixture';

const binary =
  process.env.AAC_M6_TARGET_CLI ??
  path.resolve(
    __dirname,
    '../../dist/native',
    process.platform === 'win32' ? 'ai-account-center.exe' : 'ai-account-center'
  );
export const haveNativeStoreServer = existsSync(binary);

const credentials = { username: 'fixture-admin', password: 'fixture-password-only' };

export async function startNativeStoreServer(root: string, store = 'on') {
  await writeFile(
    path.join(root, 'config.yaml'),
    JSON.stringify({
      version: 10,
      dashboard_auth: {
        enabled: true,
        username: credentials.username,
        password_hash: await bcrypt.hash(credentials.password, 4),
      },
    }),
    { mode: 0o600 }
  );
  const server = await startRustFixture(
    binary,
    root,
    {
      PATH: process.env.PATH,
      SystemRoot: process.env.SystemRoot,
      CCS_HOME: root,
      HOME: root,
      USERPROFILE: root,
      AAC_STORE: store,
      AAC_STORE_SHARE_BACKUP_DIR: 'off',
      AAC_RUST_GROUP_COLLECTION: 'off',
      AAC_RUST_GROUP_APP_UPDATES: 'off',
      AAC_RUST_GROUP_STATUS: 'off',
      AAC_RUST_GROUP_SCOREBOARD: 'off',
      AAC_AGENT_TRANSPORT: 'off',
      AAC_AGENT_VERBS: 'off',
      AAC_STATUS_DOORBELL: 'off',
    },
    false,
    'cut-rust-only'
  );
  const send = (route: string, options: RequestInit = {}) =>
    fetch(server.url + route, {
      ...options,
      redirect: 'manual',
      signal: AbortSignal.timeout(10_000),
    });
  const post = (route: string, body: unknown) =>
    send(route, {
      method: 'POST',
      headers: { origin: server.url, 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });
  try {
    const login = await post('/api/auth/login', credentials);
    assert.equal(login.status, 200, await login.text());
    const cookie = login.headers.get('set-cookie')?.split(';')[0];
    assert.ok(cookie);
    return {
      ...server,
      send,
      cookie,
      async device() {
        const pair = await post('/api/auth/devices/pair', {
          ...credentials,
          deviceName: 'Store fixture',
          platform: 'mac',
          installId: '00000000-0000-4000-8000-000000000001',
          appVersion: '0.0.0',
        });
        assert.equal(pair.status, 201);
        return 'Bearer ' + (await pair.json()).token;
      },
    };
  } catch (error) {
    await server.stop();
    throw error;
  }
}
