import assert from 'node:assert/strict';
import http from 'node:http';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { mkdtemp, rm } from 'node:fs/promises';
import { registerRustAgentConnection } from '../../src/web-server/rust-service-transport';
import { nativeSignInSession } from '../../src/web-server/services/lifecycle-signin-session';
import { SignInJobRunner } from '../../src/web-server/services/signin-jobs';
import type { SignInFlowSpec } from '../../src/web-server/services/signin-job-types';
import { MuseAccountLifecycle } from '../../src/web-server/services/muse-account-lifecycle';
import { readMuseLoginThroughAgent } from '../../src/web-server/services/lifecycle-signin-session';

const root = await mkdtemp(path.join(tmpdir(), 'aac-signin-dispatch-'));
const socketPath = path.join(root, 'agent.sock');
const calls: Array<{ path: string; action: string | null }> = [];
let startOutcome: 'install' | 'lost' | 'unavailable' = 'install';
let identityOutcome: 'found' | 'missing' | 'unsupported' = 'found';
const stamp = '2026-11-19T00:00:00.000Z';
let finishSettlement!: () => void;
const settled = new Promise<void>((resolve) => (finishSettlement = resolve));
const server = http.createServer(async (request, response) => {
  let input = '';
  for await (const chunk of request) input += chunk;
  const call = JSON.parse(input);
  const route = request.url ?? '';
  calls.push({ path: route, action: call.args?.action ?? null });
  if (route === '/internal/agent/lifecycle-signin' && startOutcome === 'lost') {
    response.destroy();
    return;
  }
  response.writeHead(200, {
    'x-aac-service-boot': 'fixture',
    'content-type': 'application/x-ndjson',
  });
  const base = { protocol: 1, id: call.callId };
  if (route === '/internal/agent/lifecycle-signin') {
    assert.equal(call.args.provider, 'muse');
    assert.equal(call.args.platform, 'mac');
    assert.equal(call.args.ccs, undefined);
    if (startOutcome === 'unavailable') {
      response.end(JSON.stringify({ ...base, type: 'fallback', reason: 'unavailable' }) + '\n');
      return;
    }
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
  } else if (route === '/internal/agent/muse-login-identity') {
    assert.deepEqual(call.computer, { platform: 'mac', sshHost: 'fixture-mac' });
    assert.deepEqual(call.args, { homeId: '9f2c41d0' });
    const identity =
      identityOutcome === 'unsupported'
        ? { type: 'fallback', reason: 'unsupported' }
        : { type: 'result', value: { email: identityOutcome === 'found' ? 'muse@example.test' : null } };
    response.end(JSON.stringify({ ...base, ...identity }) + '\n');
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
    startOutcome = 'lost';
    let lostCleanup!: () => void;
    const lost = new Promise<void>((resolve) => (lostCleanup = resolve));
    const uncertain = runner.start({ ...spec, cleanup: async () => lostCleanup() });
    await lost;
    assert.equal(runner.get(uncertain.id)?.error?.code, 'server_restarted');
    assert.equal(installed, 1);

    startOutcome = 'unavailable';
    let startedLegacy!: () => void;
    const legacy = new Promise<void>((resolve) => (startedLegacy = resolve));
    const fallback = new SignInJobRunner({
      nativeSession: () => remote,
      spawn: () => {
        startedLegacy();
        return {
          onData: () => undefined,
          onExit: () => undefined,
          write: () => false,
          kill: () => undefined,
        };
      },
    });
    const legacyJob = fallback.start({ ...spec, cleanup: async () => {} });
    await legacy;
    assert.equal(fallback.cancel(legacyJob.id)?.state, 'cancelled');
    assert.equal(installed, 1);
    let legacyReads = 0;
    let readNative = false;
    const muse = new MuseAccountLifecycle({
      enabled: true,
      identity: (host, homeId) => readMuseLoginThroughAgent(root, host, homeId, readNative),
      ssh: async () => {
        legacyReads++;
        return 'legacy@example.test';
      },
    }).signInAgainFlow({
      id: 'muse:acct:9f2c41d0',
      provider: 'muse',
      platform: 'mac',
      sshHost: 'fixture-mac',
      label: null,
      credential: { kind: 'config-home', homeId: '9f2c41d0' },
      createdAt: stamp,
      createdBy: 'migration',
    });
    const complete = () => muse.complete(job.id, { stopped: () => false });
    assert.equal((await complete()).email, 'legacy@example.test');
    assert.equal(legacyReads, 1);
    readNative = true;
    assert.equal((await complete()).email, 'muse@example.test');
    identityOutcome = 'missing';
    await assert.rejects(complete, { code: 'write_failed' });
    assert.equal(legacyReads, 1);
    identityOutcome = 'unsupported';
    assert.equal((await complete()).email, 'legacy@example.test');
    assert.equal(legacyReads, 2);
    console.log(
      'sign-in dispatch: start, install, settle, no replay, fallback and Muse verification passed'
    );
  } finally {
    release();
  }
} finally {
  await new Promise<void>((resolve) => server.close(() => resolve()));
  await rm(root, { recursive: true, force: true });
}
