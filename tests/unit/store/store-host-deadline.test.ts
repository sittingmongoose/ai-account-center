/** Max pair 2 M6: real sockets, stalled responses, and a separate live health endpoint. */
import { expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import {
  registerRustStoreConnection,
  requestRustStore,
  rustStoreConnection,
  setRustStoreHosted,
} from '../../../src/web-server/rust-service-transport';
import { SocketStoreHandle } from '../../../src/store/store-host-client';

for (const mode of ['paused', 'live-long-job', 'live-stalled-read'] as const)
  it(`bounds hosted read and close while ${mode}`, async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-store-deadline-'));
    const active = new Set<http.ClientRequest>();
    let hung = false;
    let probes = 0;
    let unresponsive = 0;
    const server = http.createServer((req, res) => {
      res.setHeader('x-aac-service-boot', 'fixture');
      if (req.url === '/api/health') {
        expect(JSON.parse(String(req.headers['x-aac-principal']))).toEqual({
          kind: 'session',
          id: 'public-health',
          role: 'public',
        });
        probes++;
        if (mode !== 'paused') res.end('{"status":"ok"}');
        return;
      }
      if (hung) return;
      if (req.url?.endsWith('/hostIdentity'))
        res.end(
          '{"commit":"ffffffffffffffffffffffffffffffffffffffff","storeSchema":2,"bindingVersion":2}'
        );
      else res.end('{}');
    });
    const socketPath = path.join(root, 'http.sock');
    await new Promise<void>((resolve) => server.listen(socketPath, resolve));
    const disconnect = registerRustStoreConnection(
      root,
      {
        socketPath,
        secret: 'fixture',
        bootId: 'fixture',
        build: { commit: 'f'.repeat(40), storeSchema: 2 },
      },
      active,
      { unresponsive: () => unresponsive++ },
      true
    );
    const handle = new SocketStoreHandle(root, {
      load: () => ({
        module: {
          buildIdentity: () =>
            '{"commit":"ffffffffffffffffffffffffffffffffffffffff","storeSchema":2,"bindingVersion":2}',
        } as never,
        file: 'fixture.node',
      }),
    });
    let pending: Promise<unknown> | undefined;
    try {
      await handle.call('open', '{}');
      hung = true;
      if (mode === 'live-long-job') {
        let settled = false;
        pending = requestRustStore(rustStoreConnection(root)!, 'backup', '{}')
          .catch(() => undefined)
          .finally(() => (settled = true));
        await new Promise((resolve) => setTimeout(resolve, 2800));
        expect(probes).toBeGreaterThan(0);
        expect(settled).toBe(false);
        const began = performance.now();
        await expect(handle.call('close', '{}')).rejects.toThrow('service_down');
        expect(performance.now() - began).toBeLessThan(3500);
      } else {
        pending = handle.call('status', '{}');
        const read = pending.then(
          () => 'success',
          (error) => String(error)
        );
        const close = handle.call('close', '{}').catch((error) => String(error));
        const began = performance.now();
        const result = await Promise.race([
          Promise.all([read, close]),
          new Promise<null>((resolve) => setTimeout(() => resolve(null), 4000)),
        ]);
        expect(result).not.toBeNull();
        expect(result?.[0]).toContain('service_down');
        expect(performance.now() - began).toBeLessThan(3500);
        if (mode === 'paused') expect(unresponsive).toBeGreaterThan(0);
      }
    } finally {
      for (const req of active) req.destroy();
      await pending?.catch(() => undefined);
      disconnect();
      setRustStoreHosted(root, false);
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
      fs.rmSync(root, { recursive: true, force: true });
    }
  }, 10000);
