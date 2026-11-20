/** Two fake native trays exercise pairing, reconnect and per-device revoke on real Rust. */
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import bcrypt from 'bcrypt';
import { fixtureEnvironment, startRustFixture } from './m6-rust-fixture';

const binary = path.resolve(process.argv[2] ?? '');
const root = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-m6-trays-'));
const username = 'fixture-admin';
const password = 'fixture-password';
const nonce = '37a15c2ee109e3ad';
const env = fixtureEnvironment();
type Platform = 'mac' | 'windows';
type Tray = { deviceId: string; token: string; name: string; platform: Platform };

const call = (base: string, route: string, request: RequestInit = {}) =>
  fetch(base + route, { ...request, signal: AbortSignal.timeout(5000) });

async function pair(base: string, platform: Platform, index: number): Promise<Tray> {
  const response = await call(base, '/api/auth/devices/pair', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      username,
      password,
      platform,
      deviceName: platform === 'mac' ? 'Fixture Mac tray' : 'Fixture Windows tray',
      installId: '00000000-0000-4000-8000-00000000000' + index,
      appVersion: '8.10.0',
    }),
  });
  assert.equal(response.status, 201, platform + ' pairing');
  const tray: Tray = await response.json();
  assert.equal(tray.platform, platform);
  assert.match(tray.deviceId, /^dev_[a-f0-9]{16}$/);
  assert.match(tray.token, /^aacd_[A-Za-z0-9_-]{43}$/);
  assert.equal(response.headers.get('set-cookie'), null);
  return tray;
}

async function reconnect(base: string, tray: Tray) {
  const headers = { authorization: 'Bearer ' + tray.token, 'x-ccs-bar-nonce': nonce };
  const result = await call(base, '/api/auth/devices/me', { headers });
  assert.equal(result.status, 200, tray.platform + ' bearer reconnect');
  const device = await result.json();
  assert.deepEqual(
    [device.id, device.name, device.platform],
    [tray.deviceId, tray.name, tray.platform]
  );
  const probe = await call(base, '/api/bar/auth', { headers });
  assert.equal(probe.status, 200);
  const secret = (await fs.readFile(path.join(root, 'bar', '.auth-token'), 'utf8')).trim();
  assert.equal(
    probe.headers.get('x-ccs-bar-token'),
    createHmac('sha256', secret).update(nonce).digest('hex')
  );
  assert.deepEqual(await probe.json(), { status: 'ok' });
}

try {
  await fs.writeFile(
    path.join(root, 'config.yaml'),
    JSON.stringify({
      dashboard_auth: { enabled: true, username, password_hash: await bcrypt.hash(password, 4) },
    }),
    { mode: 0o600 }
  );
  let rust = await startRustFixture(binary, root, env);
  try {
    const mac = await pair(rust.url, 'mac', 1);
    const windows = await pair(rust.url, 'windows', 2);
    const trays = [mac, windows];
    for (const tray of trays) await reconnect(rust.url, tray);
    const login = await call(rust.url, '/api/auth/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username, password }),
    });
    assert.equal(login.status, 200);
    await login.arrayBuffer();
    const cookie = login.headers.get('set-cookie')!.split(';')[0];

    await rust.stop();
    rust = await startRustFixture(binary, root, env);
    for (const tray of trays) await reconnect(rust.url, tray);
    const list = await call(rust.url, '/api/auth/devices', { headers: { cookie } });
    assert.equal(list.status, 200);
    assert.deepEqual(
      (await list.json()).devices.map((entry: { id: string }) => entry.id),
      trays.map((tray) => tray.deviceId)
    );
    const stored = await fs.readFile(path.join(root, 'auth', 'devices.json'), 'utf8');
    assert(
      trays.every((tray) => !stored.includes(tray.token)),
      'device file holds token hashes'
    );

    for (const tray of trays) {
      const revoke = await call(rust.url, '/api/auth/devices/' + tray.deviceId, {
        method: 'DELETE',
        headers: { cookie, origin: rust.url },
      });
      assert.equal(revoke.status, 204);
      const refused = await call(rust.url, '/api/auth/devices/me', {
        headers: { authorization: 'Bearer ' + tray.token },
      });
      assert.equal(refused.status, 401);
      assert.equal((await refused.json()).code, 'device_revoked');
      if (tray === mac) await reconnect(rust.url, windows);
    }
  } finally {
    await rust.stop();
  }
} finally {
  await fs.rm(root, { recursive: true, force: true });
}
console.log(
  'M6 tray fakes: Mac and Windows paired, reconnected after Rust restart, and were revoked independently.'
);
