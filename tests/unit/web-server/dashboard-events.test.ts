/**
 * /ws hints (CONTRACT-registry-lifecycle 6.6): a sign-in job goes only to
 * browser-session sockets (never a device token or an unknown client), and its
 * verification URL, code and email only to those that connected over a secure
 * transport.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import http from 'http';
import type { AddressInfo } from 'net';
import { WebSocket, WebSocketServer } from 'ws';
import {
  attachDashboardEventServer,
  broadcastDashboardEvent,
} from '../../../src/web-server/dashboard-events';
import {
  redactSignInJob,
  signInJobEvent,
} from '../../../src/web-server/services/account-lifecycle-runtime';
import type { RequestAuthKind } from '../../../src/web-server/middleware/request-auth';
import type { SignInJob } from '../../../src/web-server/services/signin-jobs';

const closers: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const close of closers.splice(0).reverse()) await close();
});

const JOB: SignInJob = {
  id: 'job_0123456789abcdef',
  provider: 'codex',
  kind: 'device-code',
  mode: 'add',
  accountId: null,
  profileName: 'codex-4',
  platform: 'ubuntu',
  state: 'waiting',
  verification: {
    url: 'https://auth.openai.com/codex/device',
    userCode: 'ABCD-12345',
    expiresAt: '2026-10-02T08:15:00.000Z',
  },
  result: null,
  error: null,
  startedAt: '2026-10-02T08:00:00.000Z',
  updatedAt: '2026-10-02T08:00:05.000Z',
  expiresAt: '2026-10-02T08:15:00.000Z',
};

describe('dashboard events', () => {
  it('builds a sign-in job per client and redacts it for plain transports', async () => {
    const server = http.createServer();
    const wss = new WebSocketServer({ server, path: '/ws' });
    const detach = attachDashboardEventServer(wss, {
      isSecure: (request) => request.headers['x-test-secure'] === '1',
      authKind: () => 'session',
    });
    closers.push(async () => {
      detach();
      for (const client of wss.clients) client.terminate();
      await new Promise<void>((resolve) => wss.close(() => resolve()));
      server.closeAllConnections?.();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', () => resolve()));
    const url = `ws://127.0.0.1:${(server.address() as AddressInfo).port}/ws`;
    const connect = async (secure: boolean) => {
      const socket = new WebSocket(url, { headers: secure ? { 'x-test-secure': '1' } : {} });
      await new Promise<void>((resolve, reject) => {
        socket.once('open', () => resolve());
        socket.once('error', reject);
      });
      closers.push(async () => socket.close());
      return socket;
    };
    const [secure, plain] = await Promise.all([connect(true), connect(false)]);
    const received = [secure, plain].map(
      (socket) =>
        new Promise<string>((resolve) => socket.once('message', (data) => resolve(String(data))))
    );
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(
      broadcastDashboardEvent((client) => ({
        type: 'signin-job',
        job: client.secure ? JOB : redactSignInJob(JOB),
      }))
    ).toBe(2);
    const [secureMessage, plainMessage] = await Promise.all(received);
    expect(JSON.parse(secureMessage).job.verification.userCode).toBe('ABCD-12345');
    expect(JSON.parse(plainMessage)).toEqual({
      type: 'signin-job',
      job: { ...JOB, verification: null },
    });
    expect(plainMessage).not.toContain('ABCD-12345');
    // A builder that skips everyone sends nothing; a plain event goes to all.
    expect(broadcastDashboardEvent(() => null)).toBe(0);
    expect(broadcastDashboardEvent({ type: 'accounts-changed' })).toBe(2);
  });

  it('sends sign-in jobs to browser sessions only, without the email on plain transports', async () => {
    const server = http.createServer();
    const wss = new WebSocketServer({ server, path: '/ws' });
    const detach = attachDashboardEventServer(wss, {
      isSecure: (request) => request.headers['x-test-secure'] === '1',
      authKind: (request) => {
        const kind = request.headers['x-test-auth'];
        if (kind === 'throw') throw new Error('no session store');
        return (kind === 'session' || kind === 'device' ? kind : null) as RequestAuthKind;
      },
    });
    closers.push(async () => {
      detach();
      for (const client of wss.clients) client.terminate();
      await new Promise<void>((resolve) => wss.close(() => resolve()));
      server.closeAllConnections?.();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', () => resolve()));
    const url = `ws://127.0.0.1:${(server.address() as AddressInfo).port}/ws`;
    const sockets: Record<string, { messages: string[] }> = {};
    const connect = async (name: string, auth: string, secure: boolean) => {
      const socket = new WebSocket(url, {
        headers: { 'x-test-auth': auth, ...(secure ? { 'x-test-secure': '1' } : {}) },
      });
      const messages: string[] = [];
      socket.on('message', (data) => messages.push(String(data)));
      await new Promise<void>((resolve, reject) => {
        socket.once('open', () => resolve());
        socket.once('error', reject);
      });
      closers.push(async () => socket.close());
      sockets[name] = { messages };
    };
    await Promise.all([
      connect('session-secure', 'session', true),
      connect('session-plain', 'session', false),
      connect('device-secure', 'device', true),
      connect('anonymous-secure', 'none', true),
      connect('broken', 'throw', true),
    ]);
    await new Promise((resolve) => setTimeout(resolve, 20));
    const done: SignInJob = {
      ...JOB,
      state: 'succeeded',
      verification: null,
      result: { accountId: 'codex:codex-4', email: 'private@example.com', plan: 'pro' },
    };
    expect(broadcastDashboardEvent((client) => signInJobEvent(JOB, client))).toBe(2);
    expect(broadcastDashboardEvent((client) => signInJobEvent(done, client))).toBe(2);
    expect(broadcastDashboardEvent({ type: 'accounts-changed' })).toBe(5);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(sockets['session-secure'].messages.map((text) => JSON.parse(text))).toEqual([
      { type: 'signin-job', job: JOB },
      { type: 'signin-job', job: done },
      { type: 'accounts-changed' },
    ]);
    expect(sockets['session-plain'].messages.map((text) => JSON.parse(text))).toEqual([
      { type: 'signin-job', job: { ...JOB, verification: null } },
      { type: 'signin-job', job: { ...done, result: { ...done.result, email: null } } },
      { type: 'accounts-changed' },
    ]);
    for (const name of ['device-secure', 'anonymous-secure', 'broken']) {
      expect([name, sockets[name].messages]).toEqual([name, ['{"type":"accounts-changed"}']]);
    }
    expect(redactSignInJob(done).result).toEqual({
      accountId: 'codex:codex-4',
      email: null,
      plan: 'pro',
    });
  });
});
