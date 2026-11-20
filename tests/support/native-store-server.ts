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

export async function startNativeStoreServer(
  root: string,
  store = 'on',
  options: { rustStore?: 'off'; configStore?: 'off'; worker?: string } = {}
) {
  await writeFile(
    path.join(root, 'config.yaml'),
    JSON.stringify({
      version: 10,
      ...(options.configStore ? { rustService: { store: options.configStore } } : {}),
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
      ...(options.rustStore ? { AAC_RUST_STORE: options.rustStore } : {}),
      AAC_STORE_SHARE_BACKUP_DIR: 'off',
      AAC_RUST_GROUP_COLLECTION: 'off',
      AAC_RUST_GROUP_APP_UPDATES: 'off',
      AAC_RUST_GROUP_RENEWAL: 'off',
      AAC_AGENT_REPORTS_DIR: path.join(root, 'reports'),
      AAC_PM_MAIL_REGISTRY_DIR: path.join(root, 'registry'),
      AAC_T3_STATE_DB: path.join(root, 'pins.sqlite'),
      AAC_AGENT_TRANSPORT: 'off',
      AAC_AGENT_VERBS: 'off',
      AAC_STATUS_DOORBELL: 'off',
      ...(options.worker
        ? {
            AAC_TS_SIDECAR: options.worker,
            NODE_OPTIONS:
              '--require=' +
              JSON.stringify(
                path
                  .resolve(__dirname, '../spec/m6-cut-worker-fixture.cjs')
                  .split(path.sep)
                  .join('/')
              ),
          }
        : {}),
    },
    false,
    options.worker ? 'cut' : 'cut-rust-only'
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
