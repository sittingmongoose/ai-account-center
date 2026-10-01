import { afterEach, describe, expect, it, mock } from 'bun:test';
import type { AddressInfo } from 'net';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

import { startServer } from '../../../src/web-server';

const instances: Array<Awaited<ReturnType<typeof startServer>>> = [];
const staticDirs: string[] = [];

class MockUpgradeSocket {
  data = '';
  destroyed = false;

  write(chunk: string | Buffer): boolean {
    this.data += chunk.toString();
    return true;
  }

  destroy(): void {
    this.destroyed = true;
  }
}

function dispatchUpgrade(instance: Awaited<ReturnType<typeof startServer>>, url: string) {
  const listener = instance.server.listeners('upgrade')[0] as (
    request: unknown,
    socket: MockUpgradeSocket,
    head: Buffer
  ) => void;
  const socket = new MockUpgradeSocket();

  listener({ url, headers: {}, socket: { remoteAddress: '127.0.0.1' } }, socket, Buffer.alloc(0));

  return socket;
}

afterEach(async () => {
  while (instances.length > 0) {
    const instance = instances.pop();
    if (!instance) {
      continue;
    }

    instance.cleanup();
    await new Promise<void>((resolve) => instance.server.close(() => resolve()));
  }

  mock.restore();
  for (const dir of staticDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

describe('startServer host binding', () => {
  it('binds to localhost by default when no host is provided', async () => {
    const instance = await startServer({ port: 0 });
    instances.push(instance);

    const address = instance.server.address() as AddressInfo;
    expect(address.port).toBeGreaterThan(0);
    expect(['127.0.0.1', '::1']).toContain(address.address);
  });

  it('binds to an explicit loopback host', async () => {
    const instance = await startServer({ port: 0, host: '127.0.0.1' });
    instances.push(instance);

    const address = instance.server.address() as AddressInfo;
    expect(address.address).toBe('127.0.0.1');
  });

  it('binds to wildcard host when requested', async () => {
    const instance = await startServer({ port: 0, host: '0.0.0.0' });
    instances.push(instance);

    const address = instance.server.address() as AddressInfo;
    expect(['0.0.0.0', '::']).toContain(address.address);
  });

  it('serves the same Slint assets in dev mode with streaming WebAssembly MIME', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-slint-static-'));
    staticDirs.push(dir);
    fs.writeFileSync(path.join(dir, 'index.html'), '<canvas id="slint-dashboard"></canvas>');
    const wasm = Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]);
    fs.writeFileSync(path.join(dir, 'dashboard.wasm'), wasm);
    const instance = await startServer({ port: 0, host: '127.0.0.1', dev: true, staticDir: dir });
    instances.push(instance);
    const address = instance.server.address() as AddressInfo;
    const root = `http://127.0.0.1:${address.port}`;
    expect(await (await fetch(root)).text()).toContain('slint-dashboard');
    expect(await (await fetch(`${root}/login`)).text()).toContain('slint-dashboard');
    const previousDashboard = await fetch(`${root}/codex/accounts`, { redirect: 'manual' });
    expect(previousDashboard.status).toBe(302);
    expect(previousDashboard.headers.get('location')).toBe('/');
    const response = await fetch(`${root}/dashboard.wasm`);
    expect(response.headers.get('content-type')).toBe('application/wasm');
    expect(Buffer.from(await response.arrayBuffer())).toEqual(wasm);
    expect(dispatchUpgrade(instance, '/vite-hmr').destroyed).toBe(true);
  });

  it('rejects unsupported production websocket upgrade paths', async () => {
    const instance = await startServer({ port: 0 });
    instances.push(instance);

    const socket = dispatchUpgrade(instance, '/vite-hmr');

    expect(socket.data.startsWith('HTTP/1.1 404')).toBe(true);
    expect(socket.destroyed).toBe(true);
  });

  it('rejects malformed websocket upgrade targets', async () => {
    const instance = await startServer({ port: 0 });
    instances.push(instance);

    const socket = dispatchUpgrade(instance, 'http://[bad');

    expect(socket.data.startsWith('HTTP/1.1 400')).toBe(true);
    expect(socket.destroyed).toBe(true);
  });
});
