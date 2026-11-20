import assert from 'node:assert/strict';
import http from 'node:http';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { registerRustAgentConnection } from '../../src/web-server/rust-service-transport';
import { openCursorThroughAgent } from '../../src/web-server/services/cursor-open';

const root = await mkdtemp(path.join(tmpdir(), 'aac-cursor-'));
const socketPath = path.join(root, 'agent.sock');
let requests = 0;
let reply: 'opened' | 'unavailable' | 'lost' | 'refused' = 'opened';
const server = http.createServer(async (request, response) => {
  requests++;
  let input = '';
  for await (const chunk of request) input += chunk;
  const call = JSON.parse(input);
  assert.equal(request.url, '/internal/agent/cursor-open');
  assert.deepEqual(call.computer, { platform: 'mac', sshHost: 'fixture-mac' });
  assert.deepEqual(call.args, {});
  if (reply === 'lost') {
    response.destroy();
    return;
  }
  response.writeHead(200, {
    'x-aac-service-boot': 'test',
    'content-type': 'application/x-ndjson',
  });
  const base = { protocol: 1, id: call.callId };
  response.end(
    JSON.stringify(
      reply === 'unavailable'
        ? { ...base, type: 'fallback', reason: 'unavailable' }
        : { ...base, type: 'result', value: { opened: reply === 'opened' } }
    ) + '\n'
  );
});
try {
  await new Promise<void>((resolve) => server.listen(socketPath, resolve));
  assert.equal(await openCursorThroughAgent(root, 'fixture-mac', true), false);
  const release = registerRustAgentConnection(
    root,
    { socketPath, secret: 'fixture', bootId: 'test' },
    new Set()
  );
  try {
    for (const mode of ['off', 'shadow']) {
      assert.equal(await openCursorThroughAgent(root, 'fixture-mac', false), false);
      assert.equal(requests, 0);
      console.log('Cursor Open ' + mode + ': legacy owner');
    }
    assert.equal(await openCursorThroughAgent(root, 'fixture-mac', true), true);
    reply = 'unavailable';
    assert.equal(await openCursorThroughAgent(root, 'fixture-mac', true), false);
    reply = 'refused';
    await assert.rejects(openCursorThroughAgent(root, 'fixture-mac', true));
    reply = 'lost';
    await assert.rejects(openCursorThroughAgent(root, 'fixture-mac', true));
    assert.equal(requests, 4);
    console.log('Cursor Open on: 4/4 outcomes');
  } finally {
    release();
  }
} finally {
  await new Promise<void>((resolve) => server.close(() => resolve()));
  await rm(root, { recursive: true, force: true });
}
