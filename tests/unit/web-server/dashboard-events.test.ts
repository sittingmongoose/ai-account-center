/**
 * /ws hints (CONTRACT-registry-lifecycle 6.6): a sign-in job goes to every
 * signed-in socket, but its verification URL and code only to sockets that
 * connected over a secure transport.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import http from 'http';
import type { AddressInfo } from 'net';
import { WebSocket, WebSocketServer } from 'ws';
import {
  attachDashboardEventServer,
  broadcastDashboardEvent,
} from '../../../src/web-server/dashboard-events';
import { redactSignInJob } from '../../../src/web-server/services/account-lifecycle-runtime';
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
});
