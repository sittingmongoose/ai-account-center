import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import express from 'express';
import http, { type Server } from 'http';
import type { AddressInfo } from 'net';
import fs from 'fs';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { StatusAnswers } from '../../../../src/web-server/status/answers';
import { StatusService } from '../../../../src/web-server/status/status-service';
import { PmMailRegistryReader } from '../../../../src/web-server/status/status-collectors';
import { openCardFile, parseByteRange } from '../../../../src/web-server/status/status-files';
import { createStatusRouter } from '../../../../src/web-server/routes/status-routes';
import { cardDoc, NOW, tempDirs, writeCard } from './status-test-helpers';

const dirs = tempDirs();
let server: Server | undefined;
// The 401 path exists only with dashboard sign-in on; the sign-in-off answer has its own test.
const authEnv = process.env.CCS_DASHBOARD_AUTH_ENABLED;
beforeEach(() => {
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
});
afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server?.close(() => resolve()));
  server = undefined;
  dirs.cleanup();
  if (authEnv === undefined) delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  else process.env.CCS_DASHBOARD_AUTH_ENABLED = authEnv;
});

async function start(options: { ringOk?: boolean } = {}) {
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
      return options.ringOk === false
        ? { ok: false, detail: 'pm-mail refused.' }
        : { ok: true, detail: 'Rang.' };
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
  app.use(
    '/api/status',
    createStatusRouter(() => service)
  );
  server = await new Promise<Server>((resolve) => {
    const s = app.listen(0, '127.0.0.1', () => resolve(s));
  });
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  return { root, store, base, service, rings };
}

const session = { 'x-test-session': 'true' };

describe('status routes', () => {
  for (const code of ['EIO', 'EACCES'])
    it(`isolates ${code} history failures and recovers both GET routes`, async () => {
      const { root, store, base, service } = await start();
      const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
      await service.scanReports();
      await service.answers.submitSet(
        [
          {
            cardId: 'vm.lead--Q1',
            cardSeq: 1,
            cardSha256: sha,
            choice: ['A'],
            note: '',
            via: 'deck',
          },
        ],
        'Linux'
      );
      // A restarted reader has no cache to mask the failed private source.
      (service.answers as unknown as { entries: unknown }).entries = null;
      const read = fs.readFileSync;
      const fault = spyOn(fs, 'readFileSync').mockImplementation(((file: any, ...args: any[]) => {
        if (String(file) === path.join(store, 'answers.jsonl'))
          throw Object.assign(Error('private error'), { code });
        return (read as any)(file, ...args);
      }) as typeof fs.readFileSync);
      try {
        const res = await fetch(`${base}/api/status`, { headers: session });
        expect(res.status).toBe(200);
        expect(res.headers.get('cache-control')).toBe('private, no-cache');
        const view = await res.json();
        expect(view.cards.answerHistoryProblem).toBe("The answer history can't be read right now.");
        expect(view.cards.answered).toBeNull();
        expect(view.cards.open).toHaveLength(1);
        expect(view.cards.open[0].answerable).toBe(false);
        expect(view.cards.complete).toBe(false);
        expect(view.sources.some((s: any) => s.id === 'machines')).toBe(true);
        const history = await fetch(`${base}/api/status/answers`, { headers: session });
        expect(history.status).toBe(503);
        expect(history.headers.get('cache-control')).toBe('no-store');
        expect(await history.json()).toEqual({
          code: 'answers_unavailable',
          error: "The answer history can't be read right now.",
        });
      } finally {
        fault.mockRestore();
      }
      const recovered = await fetch(`${base}/api/status`, { headers: session });
      expect(recovered.status).toBe(200);
      const view = await recovered.json();
      expect(view.cards.answerHistoryProblem).toBeNull();
      expect(view.cards.answered).toHaveLength(1);
      expect(view.cards.open).toHaveLength(0);
      const history = await fetch(`${base}/api/status/answers`, { headers: session });
      expect(history.status).toBe(200);
      expect((await history.json()).answers).toHaveLength(1);
    });

  it('shows a send summary that cannot be written, with its attempts, beside the ring', async () => {
    const { root, base, service, rings } = await start();
    const q1 = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const q2 = writeCard(root, cardDoc('vm.lead', 'Q2'));
    await service.scanReports();
    // Real permissions: answer files are written and rung, the batches folder refuses every summary.
    const batches = path.join(root, 'v1', 'answers', 'batches');
    fs.mkdirSync(batches, { recursive: true });
    fs.chmodSync(batches, 0o555);
    try {
      const base_ = { choice: [], via: 'deck' as const };
      await service.answers.submitSet(
        [
          { ...base_, cardId: 'vm.lead--Q1', cardSeq: 1, cardSha256: q1, note: 'A note.' },
          { ...base_, cardId: 'vm.lead--Q2', cardSeq: 1, cardSha256: q2, choice: ['A'], note: '' },
        ],
        'Linux'
      );
      const deliveries = async () => {
        const view = await (await fetch(`${base}/api/status`, { headers: session })).json();
        const history = await (
          await fetch(`${base}/api/status/answers`, { headers: session })
        ).json();
        return [
          view.cards.open[0].notes[0].delivery,
          view.cards.answered[0].answer.delivery,
          ...history.answers.map((a: any) => a.answer.delivery),
        ];
      };
      let seen = await deliveries();
      expect(seen).toHaveLength(4);
      for (const delivery of seen) {
        expect(delivery.state).toBe('rang');
        expect(delivery.summary).toMatchObject({ state: 'failed', attempts: 1 });
        expect(delivery.summaryWarning).toBe(
          'The signed send summary could not be written to the share yet (attempt 1 of 20); retrying.'
        );
      }
      for (let i = 0; i < 25; i += 1) await service.answers.retryPending();
      seen = await deliveries();
      for (const delivery of seen) {
        expect(delivery.summary).toMatchObject({ state: 'failed', attempts: 20 });
        expect(delivery.summaryWarning).toBe(
          'The signed send summary could not be written to the share after 20 attempts; no more retries.'
        );
      }
      expect(rings).toHaveLength(1);
    } finally {
      fs.chmodSync(batches, 0o755);
    }
    await service.answers.retryPending();
    // Retries have stopped: it stays shown, never hidden.
    const after = await (await fetch(`${base}/api/status`, { headers: session })).json();
    expect(after.cards.answered[0].answer.delivery.summary.state).toBe('failed');
  });

  // A note or objection never reaches `cards.answered`, so its own failed share write or ring must be on its
  // `notes` entry even while its send summary is written.
  async function noteAndObjection(root: string, service: StatusService) {
    const q1 = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const none = { options: [], recommendation: null };
    const o1 = writeCard(root, cardDoc('vm.lead', 'O1', { kind: 'objection', ...none }));
    await service.scanReports();
    const results = await service.answers.submitSet(
      [
        {
          cardId: 'vm.lead--Q1',
          cardSeq: 1,
          cardSha256: q1,
          choice: [],
          note: 'A note.',
          via: 'deck',
        },
        {
          cardId: 'vm.lead--O1',
          cardSeq: 1,
          cardSha256: o1,
          choice: [],
          kind: 'object',
          note: 'Keep the old layout.',
          via: 'deck',
        },
      ],
      'Linux'
    );
    expect(results.map((r) => r.outcome)).toEqual(['recorded', 'recorded']);
  }
  async function noteDeliveries(base: string) {
    const view = await (await fetch(`${base}/api/status`, { headers: session })).json();
    expect(view.cards.answered).toEqual([]);
    const notes = ['vm.lead--Q1', 'vm.lead--O1'].map(
      (id) => view.cards.open.find((c: any) => c.id === id).notes
    );
    expect(notes.map((n: any[]) => n.map((x) => x.kind))).toEqual([['note'], ['object']]);
    const history = await (await fetch(`${base}/api/status/answers`, { headers: session })).json();
    expect(history.answers).toHaveLength(2);
    return [
      ...notes.map((n: any[]) => n[0].delivery),
      ...history.answers.map((a: any) => a.answer.delivery),
    ];
  }

  it("shows a note's and an objection's failed share write on the open card while their summary is written", async () => {
    const { root, base, service, rings } = await start();
    // Real permissions: the batches folder takes the summary, its parent refuses the signed note files.
    const answersDir = path.join(root, 'v1', 'answers');
    fs.mkdirSync(path.join(answersDir, 'batches'), { recursive: true });
    fs.chmodSync(answersDir, 0o555);
    try {
      await noteAndObjection(root, service);
      let seen = await noteDeliveries(base);
      for (const delivery of seen) {
        expect(delivery.state).toBe('failed');
        expect(delivery.share).toMatchObject({ state: 'failed', attempts: 1 });
        expect(delivery.detail).toBe(
          'The signed copy could not be written to the share yet (attempt 1 of 20); retrying.'
        );
        expect(delivery.summary).toMatchObject({ state: 'done' });
        expect(delivery.summaryWarning).toBeNull();
      }
      for (let i = 0; i < 25; i += 1) await service.answers.retryPending();
      seen = await noteDeliveries(base);
      for (const delivery of seen) {
        expect(delivery.state).toBe('failed');
        expect(delivery.share).toMatchObject({ state: 'failed', attempts: 20 });
        expect(delivery.detail).toBe(
          'The signed copy could not be written to the share after 20 attempts; no more retries.'
        );
        expect(delivery.summaryWarning).toBeNull();
      }
      expect(rings).toHaveLength(0);
    } finally {
      fs.chmodSync(answersDir, 0o755);
    }
  });

  it("shows a note's and an objection's failed ring on the open card while their summary is written", async () => {
    const { root, base, service, rings } = await start({ ringOk: false });
    await noteAndObjection(root, service);
    let seen = await noteDeliveries(base);
    for (const delivery of seen) {
      expect(delivery.state).toBe('failed');
      expect(delivery.share).toMatchObject({ state: 'done' });
      expect(delivery.doorbell).toMatchObject({ state: 'failed', attempts: 1 });
      expect(delivery.detail).toBe(
        'The asker could not be rung yet (attempt 1 of 20); retrying. The signed answer is on the share.'
      );
      expect(delivery.summary).toMatchObject({ state: 'done' });
      expect(delivery.summaryWarning).toBeNull();
    }
    for (let i = 0; i < 25; i += 1) await service.answers.retryPending();
    seen = await noteDeliveries(base);
    for (const delivery of seen) {
      expect(delivery.doorbell).toMatchObject({ state: 'failed', attempts: 20 });
      expect(delivery.detail).toBe(
        'The asker could not be rung after 20 attempts; no more retries. The signed answer is on the share.'
      );
      expect(delivery.summaryWarning).toBeNull();
    }
    expect(rings.length).toBeGreaterThanOrEqual(20);
  });

  it('says nothing about a send summary once it is written', async () => {
    const { root, base, service } = await start();
    const q1 = writeCard(root, cardDoc('vm.lead', 'Q1'));
    await service.scanReports();
    await service.answers.submitSet(
      [{ cardId: 'vm.lead--Q1', cardSeq: 1, cardSha256: q1, choice: ['A'], note: '', via: 'deck' }],
      'Linux'
    );
    const view = await (await fetch(`${base}/api/status`, { headers: session })).json();
    const delivery = view.cards.answered[0].answer.delivery;
    expect(delivery.summary).toMatchObject({ state: 'done', attempts: 1 });
    expect(delivery.summaryWarning).toBeNull();
    const history = await (await fetch(`${base}/api/status/answers`, { headers: session })).json();
    expect(history.answers[0].answer.delivery.summaryWarning).toBeNull();
  });

  it('serves the view to a browser session only, never cached', async () => {
    const { base } = await start();
    const anonymous = await fetch(`${base}/api/status`);
    expect(anonymous.status).toBe(401);
    expect(await anonymous.json()).toEqual({
      error: 'Authentication required',
      code: 'auth_required',
    });
    const device = await fetch(`${base}/api/status`, {
      headers: { 'x-test-device': 'true', ...session },
    });
    expect(device.status).toBe(403);
    expect((await device.json()).code).toBe('device_scope');
    const ok = await fetch(`${base}/api/status`, { headers: session });
    expect(ok.status).toBe(200);
    expect(ok.headers.get('cache-control')).toBe('private, no-cache');
    const etag = ok.headers.get('etag');
    expect(etag).toBeTruthy();
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

  it('with dashboard sign-in off, says these tabs need it, not a bare 401', async () => {
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
    const { base } = await start();
    const anonymous = await fetch(`${base}/api/status`);
    expect(anonymous.status).toBe(403);
    expect(await anonymous.json()).toEqual({
      error: 'These tabs need dashboard sign-in turned on.',
      code: 'signin_required',
    });
    // The message is the same on the routes below the guard, and a kept session still works.
    const answers = await fetch(`${base}/api/status/answers`);
    expect(answers.status).toBe(403);
    expect((await answers.json()).code).toBe('signin_required');
    expect((await fetch(`${base}/api/status`, { headers: session })).status).toBe(200);
  });

  it('takes an answer set only with the dashboard origin, JSON, no query and a strict body', async () => {
    const { root, base, service, rings } = await start();
    await service.scanReports();
    const sha = writeCard(root, cardDoc('vm.lead', 'Q1'));
    const sha2 = writeCard(root, cardDoc('vm.lead', 'Q2'));
    const one = {
      cardId: 'vm.lead--Q1',
      cardSeq: 1,
      cardSha256: sha,
      choice: ['A'],
      note: 'Yes',
      via: 'deck',
    };
    const body = { answers: [one] };
    const post = (url: string, payload: unknown, headers: Record<string, string> = {}) =>
      fetch(`${base}${url}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', origin: base, ...session, ...headers },
        body: typeof payload === 'string' ? payload : JSON.stringify(payload),
      });
    const code = async (response: Response) => `${response.status} ${(await response.json()).code}`;
    expect(await code(await post('/api/status/answers', body, { 'x-test-session': 'false' }))).toBe(
      '401 auth_required'
    );
    expect(await code(await post('/api/status/answers', body, { 'x-test-device': 'true' }))).toBe(
      '403 device_scope'
    );
    expect(
      await code(await post('/api/status/answers', body, { origin: 'http://evil.test' }))
    ).toBe('403 origin_required');
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
      await code(
        await post('/api/status/answers', JSON.stringify(body), { 'content-type': 'text/plain' })
      )
    ).toBe('415 json_required');
    const big = { answers: [{ ...one, note: 'x'.repeat(65 * 1024) }] };
    expect(await code(await post('/api/status/answers', big))).toBe('413 body_too_large');
    // 40 KB is inside the 64 KB limit: the entry itself is refused for its note length only.
    const roomy = await post('/api/status/answers', {
      answers: [{ ...one, note: 'x'.repeat(40 * 1024) }],
    });
    expect(roomy.status).toBe(200);
    expect((await roomy.json()).results[0].code).toBe('invalid_note');
    expect(await code(await post('/api/status/answers', { ...body, extra: 1 }))).toBe(
      '400 invalid_body'
    );
    expect(await code(await post('/api/status/answers', { answers: [] }))).toBe('400 invalid_body');
    expect(await code(await post('/api/status/answers', { answers: Array(51).fill(one) }))).toBe(
      '400 invalid_body'
    );
    expect(await code(await post('/api/status/answers', [one]))).toBe('400 invalid_body');
    expect(await code(await post('/api/status/answers', one))).toBe('400 invalid_body');
    const ok = await post('/api/status/answers', {
      answers: [
        one,
        { ...one, cardSeq: 2 },
        {
          cardId: 'vm.lead--Q2',
          cardSeq: 1,
          cardSha256: sha2,
          choice: [],
          note: 'Why?',
          via: 'deck',
        },
        { ...one, cardId: 'vm.lead--<script>' },
      ],
    });
    expect(ok.status).toBe(200);
    const result = await ok.json();
    expect(Object.keys(result)).toEqual(['results']);
    expect(
      result.results.map((r: { cardId: string | null; outcome: string }) => [r.cardId, r.outcome])
    ).toEqual([
      ['vm.lead--Q1', 'recorded'],
      ['vm.lead--Q1', 'card_changed'],
      ['vm.lead--Q2', 'recorded'],
      [null, 'refused'],
    ]);
    expect(Object.keys(result.results[0]).sort()).toEqual(['cardId', 'code', 'error', 'outcome']);
    expect(JSON.stringify(result)).not.toContain('script');
    await new Promise((resolve) => setTimeout(resolve, 20));
    // One ring per asker per send.
    expect(rings).toEqual(['lead']);
    const history = await fetch(`${base}/api/status/answers?limit=1`, { headers: session });
    expect(history.headers.get('cache-control')).toBe('no-store');
    const answers = (await history.json()).answers;
    expect(answers).toHaveLength(1);
    expect(answers[0].answer.kind).toBe('note');
    expect((await fetch(`${base}/api/status/answers?limit=0`, { headers: session })).status).toBe(
      400
    );
    expect((await fetch(`${base}/api/status/answers?limit=501`, { headers: session })).status).toBe(
      400
    );
    expect((await fetch(`${base}/api/status/answers?other=1`, { headers: session })).status).toBe(
      400
    );
    expect((await post('/api/status/answers/batch', body)).status).toBe(404);
  });

  it('serves option images from the writer folder only, by content, at most 2 MB', async () => {
    const { root, base } = await start();
    const writer = path.join(root, 'v1', 'writers', 'vm.lead');
    fs.mkdirSync(path.join(writer, 'images'), { recursive: true });
    const png = Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
      Buffer.alloc(16),
    ]);
    fs.writeFileSync(path.join(writer, 'images', 'a.png'), png);
    fs.writeFileSync(path.join(writer, 'images', 'fake.png'), '<svg onload=alert(1)>');
    fs.writeFileSync(
      path.join(writer, 'images', 'photo.dat'),
      Buffer.from([0xff, 0xd8, 0xff, 0xe0, 1, 2])
    );
    fs.writeFileSync(
      path.join(writer, 'images', 'big.png'),
      Buffer.concat([png, Buffer.alloc(2 * 1024 * 1024)])
    );
    const outside = path.join(root, 'secret.png');
    fs.writeFileSync(outside, png);
    fs.symlinkSync(outside, path.join(writer, 'images', 'link.png'));
    fs.mkdirSync(path.join(root, 'v1', 'writers', 'vm.other'), { recursive: true });
    fs.writeFileSync(path.join(root, 'v1', 'writers', 'vm.other', 'b.png'), png);
    const get = (url: string, headers: Record<string, string> = session) =>
      fetch(`${base}${url}`, { headers });
    const ok = await get('/api/status/files/vm.lead/images/a.png');
    expect(ok.status).toBe(200);
    expect(ok.headers.get('content-type')).toBe('image/png');
    expect(ok.headers.get('cache-control')).toBe('private, max-age=300');
    expect(ok.headers.get('x-content-type-options')).toBe('nosniff');
    expect(Buffer.from(await ok.arrayBuffer()).equals(png)).toBe(true);
    const jpeg = await get('/api/status/files/vm.lead/images/photo.dat');
    expect(jpeg.headers.get('content-type')).toBe('image/jpeg');
    expect((await get('/api/status/files/vm.lead/images/a.png', {})).status).toBe(401);
    expect((await get('/api/status/files/vm.lead/images/fake.png')).status).toBe(415);
    expect((await get('/api/status/files/vm.lead/images/big.png')).status).toBe(413);
    expect((await get('/api/status/files/vm.lead/images/link.png')).status).toBe(404);
    expect((await get('/api/status/files/vm.lead/images/missing.png')).status).toBe(404);
    // Raw request paths with encoded slashes (dot segments, literal or %2e, are resolved by the
    // URL rules before routing and can only name another writer's own folder).
    const raw = (rawPath: string) =>
      new Promise<number>((resolve, reject) => {
        const { hostname, port } = new URL(base);
        const request = http.get(
          { hostname, port, path: rawPath, headers: session },
          (response) => {
            response.resume();
            resolve(response.statusCode ?? 0);
          }
        );
        request.on('error', reject);
      });
    for (const attempt of [
      '/api/status/files/vm.lead/images%2f..%2f..%2fvm.other%2fb.png',
      '/api/status/files/vm.lead/%2Fetc%2Fpasswd',
      '/api/status/files/..%2f..%2fsecret.png/x',
    ])
      expect([attempt, await raw(attempt)]).toEqual([attempt, 404]);
    // A literal "/../" may be resolved by the HTTP stack before routing; the reader refuses it too.
    for (const file of [
      '../vm.other/b.png',
      'images/../../../secret.png',
      '/etc/passwd',
      'images\\a.png',
    ])
      expect(await openCardFile(root, 'vm.lead', file)).toEqual({
        ok: false,
        reason: 'not_found',
      });
    expect(await openCardFile(root, '..', 'secret.png')).toEqual({
      ok: false,
      reason: 'not_found',
    });
    expect(await raw('/api/status/files/vm.other/b.png')).toBe(200);
    expect((await get('/api/status/files/vm.lead/images/a.png?x=1')).status).toBe(400);
  });

  it('serves MP4 and WebM by content, up to 50 MB, with byte ranges', async () => {
    const { root, base } = await start();
    const writer = path.join(root, 'v1', 'writers', 'vm.lead', 'cards', 'vm.lead--Q1.assets');
    fs.mkdirSync(writer, { recursive: true });
    const mp4 = Buffer.concat([
      Buffer.from([0, 0, 0, 0x18]),
      Buffer.from('ftypisom', 'latin1'),
      Buffer.alloc(1000, 7),
    ]);
    for (let i = 0; i < mp4.length; i += 1) if (i >= 12) mp4[i] = i % 251;
    fs.writeFileSync(path.join(writer, 'clip.bin'), mp4);
    const webm = Buffer.concat([
      Buffer.from([0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x82, 0x84]),
      Buffer.from('webm', 'latin1'),
      Buffer.alloc(64),
    ]);
    fs.writeFileSync(path.join(writer, 'clip.webm'), webm);
    const heic = Buffer.concat([
      Buffer.from([0, 0, 0, 0x18]),
      Buffer.from('ftypheic', 'latin1'),
      Buffer.alloc(16),
    ]);
    fs.writeFileSync(path.join(writer, 'photo.mp4'), heic);
    const big = path.join(writer, 'big.mp4');
    fs.writeFileSync(big, mp4.subarray(0, 16));
    fs.truncateSync(big, 50 * 1024 * 1024 + 1);
    const url = `${base}/api/status/files/vm.lead/cards/vm.lead--Q1.assets`;
    const whole = await fetch(`${url}/clip.bin`, { headers: session });
    expect(whole.status).toBe(200);
    expect(whole.headers.get('content-type')).toBe('video/mp4');
    expect(whole.headers.get('accept-ranges')).toBe('bytes');
    expect(whole.headers.get('content-length')).toBe(String(mp4.length));
    expect(Buffer.from(await whole.arrayBuffer()).equals(mp4)).toBe(true);
    const part = await fetch(`${url}/clip.bin`, {
      headers: { ...session, range: 'bytes=100-199' },
    });
    expect(part.status).toBe(206);
    expect(part.headers.get('content-range')).toBe(`bytes 100-199/${mp4.length}`);
    expect(part.headers.get('content-length')).toBe('100');
    expect(Buffer.from(await part.arrayBuffer()).equals(mp4.subarray(100, 200))).toBe(true);
    // iOS opens with bytes=0-1.
    const probe = await fetch(`${url}/clip.bin`, { headers: { ...session, range: 'bytes=0-1' } });
    expect(probe.status).toBe(206);
    expect(Buffer.from(await probe.arrayBuffer()).equals(mp4.subarray(0, 2))).toBe(true);
    const tail = await fetch(`${url}/clip.bin`, { headers: { ...session, range: 'bytes=-10' } });
    expect(Buffer.from(await tail.arrayBuffer()).equals(mp4.subarray(mp4.length - 10))).toBe(true);
    const past = await fetch(`${url}/clip.bin`, { headers: { ...session, range: 'bytes=5000-' } });
    expect(past.status).toBe(416);
    expect(past.headers.get('content-range')).toBe(`bytes */${mp4.length}`);
    const w = await fetch(`${url}/clip.webm`, { headers: session });
    expect(w.headers.get('content-type')).toBe('video/webm');
    expect((await fetch(`${url}/photo.mp4`, { headers: session })).status).toBe(415);
    expect((await fetch(`${url}/big.mp4`, { headers: session })).status).toBe(413);
    const head = await fetch(`${url}/clip.bin`, { method: 'HEAD', headers: session });
    expect(head.status).toBe(200);
    expect(head.headers.get('content-length')).toBe(String(mp4.length));
    expect(parseByteRange('bytes=0-1,4-5', 10)).toBeNull();
    expect(parseByteRange('bytes=3-100', 10)).toEqual({ start: 3, end: 9 });
    expect(parseByteRange('bytes=-0', 10)).toBe('unsatisfiable');
    expect(parseByteRange(undefined, 10)).toBeNull();
  });
});
