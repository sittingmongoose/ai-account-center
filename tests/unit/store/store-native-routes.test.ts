import { afterEach, describe, expect, it } from 'bun:test';
import { mkdtemp, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { haveNativeStoreServer, startNativeStoreServer } from '../../support/native-store-server';

const paths = ['/api/store/status', '/api/usage/history', '/api/usage/at', '/api/usage/burn'];
let root: string | undefined;
let server: Awaited<ReturnType<typeof startNativeStoreServer>> | undefined;

async function start(store?: string) {
  root = await mkdtemp(path.join(os.tmpdir(), 'aac-store-http-'));
  server = await startNativeStoreServer(root, store);
  return server;
}

afterEach(async () => {
  await server?.stop();
  if (root) await rm(root, { recursive: true, force: true });
  server = undefined;
  root = undefined;
});

describe.skipIf(!haveNativeStoreServer)('native history store HTTP', () => {
  it('guards each read and admits both browser and device HEAD', async () => {
    const fixture = await start();
    const { send, cookie } = fixture;
    const authorization = await fixture.device();
    for (const route of paths) {
      const signedOut = await send(route);
      expect(signedOut.status).toBe(401);
      expect(signedOut.headers.get('cache-control')).toBe('no-store');
      expect(
        (await send(route, { headers: { cookie, origin: 'https://elsewhere.example' } })).status
      ).toBe(403);
      expect(
        (await send(route, { headers: { cookie, 'sec-fetch-site': 'cross-site' } })).status
      ).toBe(403);
      expect((await send(route, { headers: { authorization } })).status).toBe(200);
      for (const headers of [{ cookie }, { authorization }]) {
        const head = await send(route + '/', { method: 'HEAD', headers });
        expect(head.status).toBe(200);
        expect(await head.text()).toBe('');
        expect(head.headers.get('content-type')).toBe('application/json; charset=utf-8');
        expect(Number(head.headers.get('content-length'))).toBeGreaterThan(0);
      }
      expect((await send(route, { method: 'POST', headers: { authorization } })).status).toBe(403);
    }
    expect((await send('/api/views/home', { headers: { authorization } })).status).toBe(403);
  });

  it('keeps validation, options and conditional reads on the native listener', async () => {
    const { send, cookie } = await start();
    for (const route of [
      '/api/store/status?ignored=true',
      '/api/usage/history?resolution=minute',
      '/api/usage/history?from=yesterday',
      '/api/usage/history?account=%3Cscript%3E',
      '/api/usage/history?bogus=1',
      '/api/usage/history?from=2026-11-06T00:00:00Z&to=2026-11-01T00:00:00Z',
      '/api/usage/at?t=2026-11-06T00:00:00Z&t=2026-11-06T00:00:00Z',
      '/api/usage/burn?span=2y',
    ]) {
      const response = await send(route, { headers: { cookie } });
      expect(response.status).toBe(400);
      expect(await response.json()).toEqual({
        error: 'The query is not valid for this route.',
        code: 'invalid_query',
      });
    }
    const options = await send('/api/store/status', { method: 'OPTIONS', headers: { cookie } });
    expect(options.headers.get('allow')).toBe('GET, HEAD');
    const query = '/api/usage/at?t=2026-11-06T00:00:00Z';
    const etag = (await send(query, { headers: { cookie } })).headers.get('etag')!;
    expect(etag).toMatch(/^W\//);
    const fresh = await send(query, { headers: { cookie, 'if-none-match': etag } });
    expect(fresh.status).toBe(304);
    expect(await fresh.text()).toBe('');
  });

  it('returns the explicit unavailability when the store is disabled', async () => {
    const { send, cookie } = await start('off');
    const status = await send(paths[0], { headers: { cookie } });
    expect(await status.json()).toEqual({ state: 'unavailable', reason: 'disabled' });
    for (const route of paths.slice(1)) {
      const response = await send(route, { headers: { cookie } });
      expect(response.status).toBe(503);
      expect((await response.json()).code).toBe('store_unavailable');
    }
  });
});
