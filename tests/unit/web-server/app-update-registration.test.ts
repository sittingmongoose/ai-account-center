import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import { apiRoutes } from '../../../src/web-server/routes';

describe('installed updater API registration', () => {
  let server: Server;
  let base: string;

  beforeAll(async () => {
    const app = express();
    app.use(express.json());
    app.use('/api', apiRoutes);
    await new Promise<void>((resolve, reject) => {
      server = app.listen(0, '127.0.0.1');
      server.once('error', reject);
      server.once('listening', resolve);
    });
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('Test listener unavailable');
    base = `http://127.0.0.1:${address.port}`;
  });

  afterAll(async () => {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });

  it('mounts the real status and start routes with authentication required', async () => {
    const status = await fetch(`${base}/api/app-updates/status`);
    expect(status.status).toBe(401);
    expect(await status.json()).toEqual({ error: 'Authentication required' });
    const start = await fetch(`${base}/api/app-updates/start`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Origin: base },
      body: '{}',
    });
    expect(start.status).toBe(401);
    expect(await start.json()).toEqual({ error: 'Authentication required' });
    const cancel = await fetch(`${base}/api/app-updates/cancel`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Origin: base },
      body: '{}',
    });
    expect(cancel.status).toBe(401);
    expect(await cancel.json()).toEqual({ error: 'Authentication required' });
  });
});
