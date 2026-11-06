import { afterEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import type { AddressInfo } from 'net';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { StatusAnswers } from '../../../../src/web-server/status/answers';
import { StatusService } from '../../../../src/web-server/status/status-service';
import { PmMailRegistryReader } from '../../../../src/web-server/status/status-collectors';
import { createStatusRouter } from '../../../../src/web-server/routes/status-routes';
import { cardDoc, NOW, tempDirs, writeCard } from './status-test-helpers';

const dirs = tempDirs();
let server: Server | undefined;
afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server?.close(() => resolve()));
  server = undefined;
  dirs.cleanup();
});

async function start() {
  const root = dirs.make('ccs-status-reports-');
  const store = path.join(dirs.make('ccs-status-store-'), 'status');
  const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
  const rings: string[] = [];
  const answers = new StatusAnswers({
    readCard: (id) => reader.readCardNow(id),
    storeDir: () => store,
    reportsRoot: () => root,
    doorbell: async (to) => {
      rings.push(to);
      return { ok: true, detail: 'Rang.' };
    },
  });
  const service = new StatusService({
    reader,
    answers,
    registryReader: new PmMailRegistryReader(() => path.join(root, 'none')),
    run: async () => ({ code: null, stdout: '', timedOut: false, missing: true }),
    readLocalMachine: async () => {
      throw new Error('not in tests');
    },
    readRemoteMachine: async () => {
      throw new Error('not in tests');
    },
    timeZone: () => 'UTC',
    notify: () => undefined,
  });
  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    if (req.headers['x-test-session'] === 'true')
      Object.assign(req, { session: { authenticated: true } });
    if (req.headers['x-test-device'] === 'true') Object.assign(req, { auth: { kind: 'device' } });
    next();
  });
  app.use('/api/status', createStatusRouter(() => service));
  server = await new Promise<Server>((resolve) => {
    const s = app.listen(0, '127.0.0.1', () => resolve(s));
  });
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  return { root, base, service, rings };
}

const session = { 'x-test-session': 'true' };

describe('status routes', () => {
  it('serves the view to a browser session only, never cached', async () => {
    const { base } = await start();
    const anonymous = await fetch(`${base}/api/status`);
    expect(anonymous.status).toBe(401);
    expect(await anonymous.json()).toEqual({ error: 'Authentication required', code: 'auth_required' });
    const device = await fetch(`${base}/api/status`, { headers: { 'x-test-device': 'true', ...session } });
    expect(device.status).toBe(403);
    expect((await device.json()).code).toBe('device_scope');
    const ok = await fetch(`${base}/api/status`, { headers: session });
    expect(ok.status).toBe(200);
    expect(ok.headers.get('cache-control')).toBe('no-store');
    const view = await ok.json();
    expect(view.version).toBe(1);
    expect(view.sources.map((s: { id: string }) => s.id)).toEqual([
      'agent-reports',
      'pm-mail',
      'machines',
      'browsers',
      'simlock',
      'usage',
      'quota-watch',
    ]);
    expect((await fetch(`${base}/api/status?x=1`, { headers: session })).status).toBe(400);
  });

  it('takes an answer only with the dashboard origin, JSON, no query and a strict small body', async () => {
    const { root, base, service, rings } = await start();
    await service.scanReports();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const body = { cardId: 'vm.lead--Q1', cardSeq: 1, cardSha256: sha, choice: ['A'], note: 'Yes' };
    const post = (
      url: string,
      payload: unknown,
      headers: Record<string, string> = {}
    ) =>
      fetch(`${base}${url}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', origin: base, ...session, ...headers },
        body: typeof payload === 'string' ? payload : JSON.stringify(payload),
      });
    const code = async (response: Response) => `${response.status} ${(await response.json()).code}`;
    expect(await code(await post('/api/status/answers', body, { 'x-test-session': 'false' }))).toBe(
      '401 auth_required'
    );
    expect(await code(await post('/api/status/answers', body, { origin: 'http://evil.test' }))).toBe(
      '403 origin_required'
    );
    expect(
      await code(
        await fetch(`${base}/api/status/answers`, {
          method: 'POST',
          headers: { 'content-type': 'application/json', ...session },
          body: JSON.stringify(body),
        })
      )
    ).toBe('403 origin_required');
    expect(await code(await post('/api/status/answers?x=1', body))).toBe('400 unexpected_query');
    expect(
      await code(await post('/api/status/answers', JSON.stringify(body), { 'content-type': 'text/plain' }))
    ).toBe('415 json_required');
    expect(await code(await post('/api/status/answers', { ...body, note: 'x'.repeat(17 * 1024) }))).toBe(
      '413 body_too_large'
    );
    expect(await code(await post('/api/status/answers', { ...body, extra: 1 }))).toBe('400 invalid_body');
    expect(await code(await post('/api/status/answers', { ...body, cardSeq: '1' }))).toBe('400 invalid_body');
    expect(await code(await post('/api/status/answers', [body]))).toBe('400 invalid_body');
    expect(await code(await post('/api/status/answers', { ...body, choice: ['Z'] }))).toBe(
      '400 invalid_choice'
    );
    expect(await code(await post('/api/status/answers', { ...body, cardSeq: 2 }))).toBe('409 card_changed');
    const ok = await post('/api/status/answers', body);
    expect(ok.status).toBe(200);
    const answer = (await ok.json()).answer;
    expect(answer.answer.note).toBe('Yes');
    expect(answer.card.id).toBe('vm.lead--Q1');
    expect(await code(await post('/api/status/answers', body))).toBe('409 card_changed');
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(rings).toEqual(['lead']);
    // The error bodies never echo input.
    const refused = await post('/api/status/answers', { ...body, cardId: 'vm.lead--<script>' });
    expect(JSON.stringify(await refused.json())).not.toContain('script');
  });

  it('runs a batch and serves the history with a bounded limit', async () => {
    const { root, base } = await start();
    const q1 = writeCard(root, cardDoc('vm.lead', 'Q1', { batch: 'b' }));
    const q2 = writeCard(root, cardDoc('vm.lead', 'Q2', { batch: 'b' }));
    const response = await fetch(`${base}/api/status/answers/batch`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', origin: base, ...session },
      body: JSON.stringify({
        batch: 'b',
        cards: [
          { id: 'vm.lead--Q1', seq: 1, cardSha256: q1 },
          { id: 'vm.lead--Q2', seq: 1, cardSha256: q2 },
        ],
        except: ['vm.lead--Q2'],
      }),
    });
    expect(response.status).toBe(200);
    const result = await response.json();
    expect(result.results.map((r: { outcome: string }) => r.outcome)).toEqual(['answered', 'excepted']);
    const history = await fetch(`${base}/api/status/answers?limit=1`, { headers: session });
    expect(history.headers.get('cache-control')).toBe('no-store');
    const answers = (await history.json()).answers;
    expect(answers).toHaveLength(1);
    expect(answers[0].answer.via).toBe('approve-all-except');
    expect((await fetch(`${base}/api/status/answers?limit=0`, { headers: session })).status).toBe(400);
    expect((await fetch(`${base}/api/status/answers?limit=501`, { headers: session })).status).toBe(400);
    expect((await fetch(`${base}/api/status/answers?other=1`, { headers: session })).status).toBe(400);
    const badBatch = await fetch(`${base}/api/status/answers/batch`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', origin: base, ...session },
      body: JSON.stringify({ batch: 'b', cards: [], except: [] }),
    });
    expect(badBatch.status).toBe(400);
  });
});
