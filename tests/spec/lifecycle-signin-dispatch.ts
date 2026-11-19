import assert from 'node:assert/strict';
import http from 'node:http';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { mkdtemp, rm } from 'node:fs/promises';
import { registerRustAgentConnection } from '../../src/web-server/rust-service-transport';
import { nativeSignInSession } from '../../src/web-server/services/lifecycle-signin-session';
import { SignInJobRunner } from '../../src/web-server/services/signin-jobs';
import type { SignInFlowSpec } from '../../src/web-server/services/signin-job-types';

const root = await mkdtemp(path.join(tmpdir(), 'aac-signin-dispatch-'));
const socketPath = path.join(root, 'agent.sock');
const calls: Array<{ path: string; action: string | null }> = [];
const stamp = '2026-11-19T00:00:00.000Z';
let finishSettlement!: () => void;
const settled = new Promise<void>((resolve) => (finishSettlement = resolve));
const server = http.createServer(async (request, response) => {
  let input = '';
  for await (const chunk of request) input += chunk;
  const call = JSON.parse(input);
  const route = request.url ?? '';
  calls.push({ path: route, action: call.args?.action ?? null });
  response.writeHead(200, {
    'x-aac-service-boot': 'fixture',
    'content-type': 'application/x-ndjson',
  });
  const base = { protocol: 1, id: call.callId };
  if (route === '/internal/agent/lifecycle-signin') {
    assert.equal(call.args.provider, 'muse');
    assert.equal(call.args.platform, 'mac');
    assert.equal(call.args.ccs, undefined);
    const job = {
      ...call.args,
      id: call.args.jobId,
      state: 'verifying',
      verification: null,
      result: null,
      error: null,
      startedAt: stamp,
      updatedAt: stamp,
      expiresAt: stamp,
    };
    response.write(JSON.stringify({ ...base, type: 'progress', data: { job } }) + '\n');
    response.end(
      JSON.stringify({ ...base, type: 'result', value: { job, installPending: true } }) + '\n'
    );
  } else if (route === '/internal/agent/signin-session' && call.args.action === 'settle') {
    assert.deepEqual(call.computer, { platform: 'ubuntu', sshHost: null });
    assert.equal(call.args.ccs, root);
    assert.equal(call.args.state, 'succeeded');
    response.end(JSON.stringify({ ...base, type: 'result', value: { ok: true } }) + '\n');
    finishSettlement();
  } else {
    assert.fail('unexpected sign-in dispatch');
  }
});
try {
  await new Promise<void>((resolve) => server.listen(socketPath, resolve));
  const release = registerRustAgentConnection(
    root,
    { socketPath, secret: 'fixture', bootId: 'fixture' },
    new Set()
  );
  try {
    assert.equal(nativeSignInSession(root, false), undefined);
    const remote = nativeSignInSession(root, true);
    assert.ok(remote);
    let installed = 0;
    let cleaned!: () => void;
    const cleanup = new Promise<void>((resolve) => (cleaned = resolve));
    const runner = new SignInJobRunner({
      nativeSession: () => remote,
      spawn: () => assert.fail('native sign-in replayed through the legacy CLI'),
    });
    const spec: SignInFlowSpec = {
      provider: 'muse',
      kind: 'device-code',
      mode: 'signin-again',
      accountId: 'muse:usage',
      profileName: 'muse:usage',
      platform: 'mac',
      allowedOrigins: ['https://auth.meta.com'],
      timeoutMs: 300_000,
      prepare: async () => ({ file: '/fixture/ssh', args: [], env: {}, pty: false }),
      complete: async (jobId, control) => {
        assert.equal(control.stopped(), false);
        assert.equal(runner.get(jobId)?.state, 'verifying');
        installed++;
        return { accountId: 'muse:usage', email: null, plan: null };
      },
      cleanup: async () => cleaned(),
    };
    const job = runner.start(spec);
    await Promise.all([settled, cleanup]);
    assert.equal(runner.get(job.id)?.state, 'succeeded');
    assert.equal(installed, 1);
    assert.deepEqual(calls, [
      { path: '/internal/agent/lifecycle-signin', action: null },
      { path: '/internal/agent/signin-session', action: 'settle' },
    ]);
    console.log('sign-in dispatch: native start, guarded install and journal settlement passed');
  } finally {
    release();
  }
} finally {
  await new Promise<void>((resolve) => server.close(() => resolve()));
  await rm(root, { recursive: true, force: true });
}
