/**
 * The /api router factory: case-sensitive routes, and handler failures that end
 * in their own response instead of reaching the process-wide handlers that exit.
 */

import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import type { AddressInfo } from 'net';

import { apiErrorHandler, createApiRouter } from '../../../src/web-server/routes/api-router';

let server: Server | undefined;
let baseUrl = '';
let processFailures: unknown[] = [];
const recordProcessFailure = (reason: unknown) => processFailures.push(reason);

beforeEach(async () => {
  processFailures = [];
  process.on('unhandledRejection', recordProcessFailure);
  process.on('uncaughtException', recordProcessFailure);

  const nested = createApiRouter();
  nested.get('/rejects', async () => {
    throw new Error('PRIVATE_NESTED_DETAIL');
  });

  const router = createApiRouter();
  router.get('/ok', async (_req, res) => {
    res.json({ ok: true });
  });
  router.get('/rejects', async () => {
    throw new Error('PRIVATE_HANDLER_DETAIL');
  });
  router.get('/rejects-with-a-value', () => Promise.reject('PRIVATE_VALUE'));
  router.get('/throws', () => {
    throw new Error('PRIVATE_SYNC_DETAIL');
  });
  router.post(
    '/middleware',
    async () => {
      throw new Error('PRIVATE_MIDDLEWARE_DETAIL');
    },
    (_req, res) => res.json({ reached: true })
  );
  router.use('/nested', nested);
  router.use(apiErrorHandler);

  const app = express();
  app.use('/api', router);
  server = await new Promise<Server>((resolve) => {
    const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
  });
  baseUrl = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
});

afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server?.close(() => resolve()));
  server = undefined;
  process.off('unhandledRejection', recordProcessFailure);
  process.off('uncaughtException', recordProcessFailure);
});

describe('createApiRouter', () => {
  it('serves canonical paths and refuses other letter cases', async () => {
    expect((await fetch(`${baseUrl}/api/ok`)).status).toBe(200);
    expect((await fetch(`${baseUrl}/api/OK`)).status).toBe(404);
    expect((await fetch(`${baseUrl}/api/Ok`)).status).toBe(404);
  });

  it.each([
    { method: 'GET', route: '/rejects' },
    { method: 'GET', route: '/rejects-with-a-value' },
    { method: 'GET', route: '/throws' },
    { method: 'POST', route: '/middleware' },
    { method: 'GET', route: '/nested/rejects' },
  ])('keeps a failing $method $route inside its response', async ({ method, route }) => {
    const response = await fetch(`${baseUrl}/api${route}`, { method });
    expect(response.status).toBe(500);
    const text = await response.text();
    expect(JSON.parse(text)).toEqual({ error: 'The request could not be completed safely.' });
    expect(text).not.toContain('PRIVATE_');
    expect((await fetch(`${baseUrl}/api/ok`)).status).toBe(200);
    expect(processFailures).toEqual([]);
  });
});
