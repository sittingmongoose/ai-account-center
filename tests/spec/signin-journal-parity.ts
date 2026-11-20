import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { registerRustAgentConnection } from '../../src/web-server/rust-service-transport';
import { readSignInJournal } from '../../src/web-server/services/signin-journal-read';

const id = 'job_0123456789abcdef';
const at = '2026-10-02T08:00:00.000Z';
const dto = {
  id,
  provider: 'codex',
  kind: 'device-code',
  mode: 'signin-again',
  accountId: 'codex:work',
  profileName: 'work',
  platform: 'ubuntu',
  state: 'succeeded',
  verification: null,
  result: { accountId: 'codex:work', email: null, plan: null },
  error: null,
  startedAt: at,
  updatedAt: at,
  expiresAt: at,
};
const modes = process.argv.includes('--ts-only') ? ['off', 'shadow'] : ['off', 'shadow', 'on'];
for (const mode of modes) {
  process.env.AAC_AGENT_VERBS = mode === 'on' ? 'on' : 'off';
  process.env.AAC_AGENT_TRANSPORT = mode === 'on' ? 'on' : 'off';
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-signin-journal-')));
  if (mode !== 'on') {
    try {
      assert.equal(await readSignInJournal(root, id), undefined);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
    continue;
  }
  const secret = randomBytes(32).toString('hex');
  const bootId = randomBytes(16).toString('hex');
  const socketPath = path.join(root, 'service.sock');
  let calls = 0;
  let current: unknown = { ...dto, ignoredField: true };
  const server = http.createServer(async (req, res) => {
    let text = '';
    for await (const chunk of req) text += String(chunk);
    const body = JSON.parse(text) as { args: unknown; callId: string };
    assert.equal(req.url, '/internal/agent/signin-session');
    assert.ok(req.headers['x-aac-boot-secret'] === secret);
    assert.deepEqual(body.args, { action: 'get', jobId: id, ccs: root });
    calls += 1;
    res.writeHead(200, { 'x-aac-service-boot': bootId, 'content-type': 'application/x-ndjson' });
    res.end(
      JSON.stringify({
        protocol: 1,
        id: body.callId,
        type: 'result',
        value: { job: current },
      }) + '\n'
    );
  });
  try {
    await new Promise<void>((resolve, reject) => {
      server.once('error', reject);
      server.listen(socketPath, resolve);
    });
    const forget = registerRustAgentConnection(root, { socketPath, bootId, secret }, new Set());
    try {
      assert.deepEqual(await readSignInJournal(root, id), dto);
      current = null;
      assert.equal(await readSignInJournal(root, id), null);
      current = { ...dto, id: 'job_ffffffffffffffff' };
      assert.equal(await readSignInJournal(root, id), undefined);
      current = {
        ...dto,
        state: 'waiting',
        result: null,
        verification: { url: 'https://fixture.invalid/', userCode: null, expiresAt: null },
      };
      assert.equal(await readSignInJournal(root, id), undefined);
      assert.equal(await readSignInJournal(root, '..'), undefined);
      assert.equal(calls, 4);
    } finally {
      forget();
    }
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
    fs.rmSync(root, { recursive: true, force: true });
  }
}
console.log('sign-in journal socket and reply projection: ' + modes.join('/'));
