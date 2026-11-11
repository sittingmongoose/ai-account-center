import { expect, it } from 'bun:test';
import express from 'express';
import type { AddressInfo } from 'net';
import { sendRevalidatedView } from '../../../src/web-server/routes/revalidated-view';

it('revalidates clock-only changes but sends changed state, data and timestamps', async () => {
  const app = express();
  let view = { generatedAt: 'first', source: { asOf: 'measured', ageSeconds: 3, state: 'ok' }, count: 1 };
  app.get('/', (_req, res) => sendRevalidatedView(res, view));
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  try {
    const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}/`;
    const first = await fetch(url);
    const tag = first.headers.get('etag')!;
    expect(first.headers.get('cache-control')).toBe('private, no-cache');
    view = { ...view, generatedAt: 'later', source: { ...view.source, ageSeconds: 8 } };
    expect((await fetch(url, { headers: { 'If-None-Match': tag, 'Cache-Control': 'no-cache' } })).status).toBe(304);
    view = { ...view, source: { ...view.source, state: 'stale' } };
    expect((await fetch(url, { headers: { 'If-None-Match': tag, 'Cache-Control': 'no-cache' } })).status).toBe(200);
    view = { ...view, source: { ...view.source, state: 'ok' }, count: 2 };
    expect((await fetch(url, { headers: { 'If-None-Match': tag, 'Cache-Control': 'no-cache' } })).status).toBe(200);
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
