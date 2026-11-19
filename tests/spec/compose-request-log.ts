/** Native request logging through the composed public listener and its TS companion. */
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

const binary = process.env.AAC_RUST_PUBLIC_BINARY;
assert.ok(binary, 'AAC_RUST_PUBLIC_BINARY is required');
const home = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-compose-request-log-'));
const root = path.join(home, '.ccs');
for (const key of Object.keys(process.env)) {
  if (key.startsWith('AAC_') || key.startsWith('CCS_')) delete process.env[key];
}
for (const key of [
  'HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME',
  'XDG_DATA_HOME', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'CCS_HOME',
]) process.env[key] = home;
Object.assign(process.env, {
  CCS_DIR: root,
  AAC_RUST_COMPOSE: '1',
  AAC_AGENT_VERBS: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_STATUS_DOORBELL: 'off',
  AAC_NO_LOCAL_RUST_BUILD: '1',
  AAC_STORE: 'off',
  AAC_AGENT_REPORTS_DIR: path.join(home, 'reports'),
  AAC_PM_MAIL_REGISTRY_DIR: path.join(home, 'registry'),
});

const { startComposedServer } = await import('../../src/web-server/compose-server');
let server: Awaited<ReturnType<typeof startComposedServer>> | undefined;
try {
  const ui = path.join(home, 'ui');
  await fs.mkdir(path.join(ui, 'app'), { recursive: true });
  await fs.mkdir(root, { mode: 0o700 });
  await fs.writeFile(path.join(ui, 'app', 'index.html'), '<main>Public frame</main>');
  await fs.writeFile(path.join(ui, 'view-model.mjs'), 'export const fixture = true;');
  await fs.writeFile(path.join(root, 'config.yaml'), 'version: 14\nlogging:\n  redact: false\n', {
    mode: 0o600,
  });
  server = await startComposedServer({ host: '127.0.0.1', port: 0, staticDir: ui }, path.resolve(binary));
  const origin = `http://127.0.0.1:${server.publicAddress.port}`;
  const headers = { 'x-ccs-request-id': 'client-supplied', 'user-agent': 'aac-log-fixture' };
  const get = (route: string) => fetch(origin + route, { headers });
  const secretFixture = 'aacu_' + 'A'.repeat(43);
  const health = await get('/api/health?hint=' + secretFixture);
  assert.equal(health.status, 200);
  assert.deepEqual(await health.json(), { status: 'ok' });
  const frame = await get('/');
  assert.equal(frame.status, 200);
  assert.equal(await frame.text(), '<main>Public frame</main>');
  const staticAsset = await get('/view-model.mjs');
  assert.equal(staticAsset.status, 200);
  assert.equal(staticAsset.headers.get('x-ccs-request-id'), null);
  await staticAsset.arrayBuffer();
  await (await get('/api/logs/unknown')).arrayBuffer();

  const lines = await fs.readFile(path.join(root, 'logs', 'current.jsonl'), 'utf8');
  const requests = lines.trim().split('\n').map((line) => JSON.parse(line)).filter(
    (line) => line.source === 'web-server:http' && line.event === 'request.completed'
  );
  assert.equal(requests.length, 2);
  assert.deepEqual(requests.map((line) => line.context.path), [
    '/api/health?hint=aacu_[redacted]', '/',
  ]);
  for (const [line, response] of [[requests[0], health], [requests[1], frame]] as const) {
    const id = response.headers.get('x-ccs-request-id');
    assert.match(id ?? '', /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/);
    assert.equal(line.requestId, id);
    assert.equal(line.context.requestId, id);
    assert.equal(line.context.statusCode, 200);
    assert.equal(line.context.method, 'GET');
    assert.equal(line.context.remoteAddress, '127.0.0.1');
    assert.equal(line.context.userAgent, 'aac-log-fixture');
    assert.ok(Number.isInteger(line.context.durationMs));
  }
  assert.ok(!lines.includes(secretFixture));
  console.log('Composed public request log: UUID correlation, one line per request, URL scrub and skips passed.');
} finally {
  if (server) {
    const companion = server;
    companion.cleanup();
    companion.wss.close();
    companion.server.closeAllConnections();
    await new Promise<void>((resolve) => companion.server.close(() => resolve()));
  }
  await fs.rm(home, { recursive: true, force: true });
}
