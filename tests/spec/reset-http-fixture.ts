/** Authenticated, worker-free public listener with the caller's synthetic profile root. */
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import bcrypt from 'bcrypt';
import { fixtureEnvironment, startRustFixture } from './m6-rust-fixture';

export async function startResetFixture(repo: string, root: string) {
  const binary = process.env.AAC_RUST_PUBLIC_BINARY;
  assert.ok(binary, 'AAC_RUST_PUBLIC_BINARY must name the built ai-account-center program');
  const configPath = path.join(root, 'config.yaml');
  const config = JSON.parse(
    await readFile(configPath, 'utf8').catch((error) => {
      if (error.code === 'ENOENT') return '{}';
      throw error;
    })
  );
  const username = 'fixture-admin';
  const password = 'fixture-password-only';
  await writeFile(
    configPath,
    JSON.stringify({
      version: 10,
      ...config,
      dashboard_auth: { enabled: true, username, password_hash: await bcrypt.hash(password, 4) },
    }),
    { mode: 0o600 }
  );
  const fixture = await startRustFixture(
    binary,
    root,
    {
      ...fixtureEnvironment(),
      CCS_HOME: process.env.CCS_HOME,
      AAC_AGENT_PACKAGE_ROOT: repo,
      AAC_RUST_GROUP_COLLECTION: 'off',
      AAC_RUST_GROUP_APP_UPDATES: 'off',
      AAC_RUST_CUT_APP_UPDATES_FIXTURE: '0',
      AAC_RUST_GROUP_WRITES: 'off',
      AAC_RUST_STORE: 'off',
      AAC_AGENT_TRANSPORT: 'off',
      AAC_AGENT_VERBS: 'off',
    },
    false,
    'cut-rust-only'
  );
  try {
    const headers = { origin: fixture.url, 'content-type': 'application/json' };
    const login = await fetch(fixture.url + '/api/auth/login', {
      method: 'POST',
      headers,
      body: JSON.stringify({ username, password }),
    });
    assert.equal(login.status, 200, await login.text());
    const cookie = login.headers.get('set-cookie')?.split(';')[0];
    assert.ok(cookie, 'fixture browser session cookie');
    return { ...fixture, cookie };
  } catch (error) {
    await fixture.stop();
    throw error;
  }
}
