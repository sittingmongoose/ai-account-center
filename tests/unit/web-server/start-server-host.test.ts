import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import { createConnection, type AddressInfo } from 'node:net';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { spawn, execFileSync } from 'child_process';

import { startServer } from '../../../src/web-server';
import { CodexProfileRenewalService } from '../../../src/web-server/services/codex-profile-renewal-service';
import * as analyticsSampling from '../../../src/web-server/services/account-analytics-service';

const instances: Array<Awaited<ReturnType<typeof startServer>>> = [];
const staticDirs: string[] = [];
const fixtureEnvKeys = ['CCS_HOME', 'CCS_DIR', 'CLAUDE_CONFIG_DIR', 'CODEX_HOME'] as const;
let fixtureHome = '';
let originalEnv: Partial<Record<(typeof fixtureEnvKeys)[number], string | undefined>> = {};
let renewalStart: ReturnType<typeof spyOn<CodexProfileRenewalService, 'start'>>;
let renewalStop: ReturnType<typeof spyOn<CodexProfileRenewalService, 'stop'>>;
let analyticsStart: ReturnType<
  typeof spyOn<typeof analyticsSampling, 'startAccountAnalyticsSampling'>
>;

beforeEach(() => {
  fixtureHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-server-fixture-'));
  originalEnv = Object.fromEntries(fixtureEnvKeys.map((key) => [key, process.env[key]]));
  process.env.CCS_HOME = fixtureHome;
  process.env.CCS_DIR = path.join(fixtureHome, '.ccs');
  process.env.CLAUDE_CONFIG_DIR = path.join(fixtureHome, '.claude');
  process.env.CODEX_HOME = path.join(fixtureHome, '.codex');

  // Exercise the real HTTP/session/upgrade stack while avoiding background
  // account activation and usage helpers unrelated to these server fixtures.
  renewalStart = spyOn(CodexProfileRenewalService.prototype, 'start').mockImplementation(() => {});
  renewalStop = spyOn(CodexProfileRenewalService.prototype, 'stop');
  analyticsStart = spyOn(analyticsSampling, 'startAccountAnalyticsSampling').mockImplementation(
    () => {}
  );
  const realFetch = globalThis.fetch;
  spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
    const target = new URL(
      typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
    );
    const ownsTarget = instances.some((instance) => {
      const address = instance.server.address();
      return (
        address &&
        typeof address !== 'string' &&
        target.origin === `http://127.0.0.1:${address.port}`
      );
    });
    if (!ownsTarget)
      return Promise.reject(new Error('Only owned server fixtures may receive requests.'));
    return realFetch(input, init);
  });
});

function dispatchUpgrade(instance: Awaited<ReturnType<typeof startServer>>, url: string) {
  const { address, port } = instance.server.address() as AddressInfo;
  return new Promise<{ data: string; destroyed: boolean }>((resolve, reject) => {
    const socket = createConnection({ host: address, port });
    let data = '';
    socket.setEncoding('latin1');
    socket.setTimeout(3000, () =>
      socket.destroy(new Error('Server left the rejected upgrade open'))
    );
    socket.once('error', reject);
    socket.once('connect', () => {
      socket.write(
        [
          `GET ${url} HTTP/1.1`,
          `Host: ${address.includes(':') ? `[${address}]` : address}:${port}`,
          'Connection: Upgrade',
          'Upgrade: websocket',
          'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==',
          'Sec-WebSocket-Version: 13',
          '',
          '',
        ].join('\r\n')
      );
    });
    socket.on('data', (chunk) => {
      data += chunk;
    });
    socket.once('close', () => resolve({ data, destroyed: socket.destroyed }));
  });
}

afterEach(async () => {
  // Saved-login renewal now belongs to the native service, including its stop path.
  expect(renewalStart).not.toHaveBeenCalled();
  expect(analyticsStart).toHaveBeenCalledTimes(1);
  while (instances.length > 0) {
    const instance = instances.pop();
    if (!instance) {
      continue;
    }

    instance.cleanup();
    await new Promise<void>((resolve) => instance.server.close(() => resolve()));
  }
  expect(renewalStop).not.toHaveBeenCalled();

  mock.restore();
  for (const key of fixtureEnvKeys) {
    if (originalEnv[key] === undefined) delete process.env[key];
    else process.env[key] = originalEnv[key];
  }
  fs.rmSync(fixtureHome, { recursive: true, force: true });
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

  it('serves the same dashboard assets in dev mode with streaming WebAssembly MIME', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-ui-static-'));
    staticDirs.push(dir);
    fs.mkdirSync(path.join(dir, 'app'));
    fs.writeFileSync(path.join(dir, 'app', 'index.html'), '<main id="aac-frame-fixture"></main>');
    const wasm = Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]);
    fs.writeFileSync(path.join(dir, 'dashboard.wasm'), wasm);
    const instance = await startServer({ port: 0, host: '127.0.0.1', dev: true, staticDir: dir });
    instances.push(instance);
    const address = instance.server.address() as AddressInfo;
    const root = `http://127.0.0.1:${address.port}`;
    expect(await (await fetch(root)).text()).toContain('aac-frame-fixture');
    expect(await (await fetch(`${root}/login`)).text()).toContain('aac-frame-fixture');
    const previousDashboard = await fetch(`${root}/codex/accounts`, { redirect: 'manual' });
    expect(previousDashboard.status).toBe(302);
    expect(previousDashboard.headers.get('location')).toBe('/');
    const response = await fetch(`${root}/dashboard.wasm`);
    expect(response.headers.get('content-type')).toBe('application/wasm');
    expect(Buffer.from(await response.arrayBuffer())).toEqual(wasm);
    expect((await dispatchUpgrade(instance, '/vite-hmr')).destroyed).toBe(true);
  });

  it('rejects unsupported production websocket upgrade paths', async () => {
    const instance = await startServer({ port: 0 });
    instances.push(instance);

    const socket = await dispatchUpgrade(instance, '/vite-hmr');

    expect(socket.data.startsWith('HTTP/1.1 404')).toBe(true);
    expect(socket.destroyed).toBe(true);
  });

  it('rejects malformed websocket upgrade targets', async () => {
    const instance = await startServer({ port: 0 });
    instances.push(instance);

    const socket = await dispatchUpgrade(instance, 'http://[bad');

    expect(socket.data.startsWith('HTTP/1.1 400')).toBe(true);
    expect(socket.destroyed).toBe(true);
  });
});

// Max review M1: real public-server startup, not a mocked supervisor.
describe('optional Rust startup', () => {
  for (const kind of ['silent', 'fifo'] as const) {
    it(`serves TypeScript while Rust startup is ${kind}, without delaying JS timers`, async () => {
      const binary = path.join(fixtureHome, 'aac-http');
      let unblock: ReturnType<typeof spawn> | undefined;
      if (kind === 'fifo') {
        execFileSync('mkfifo', [binary]);
        // Pre-fix synchronous read would hang the test process. An owned writer releases it.
        unblock = spawn('python3', [
          '-c',
          'import time,sys; time.sleep(2); open(sys.argv[1], "w").close()',
          binary,
        ]);
      } else {
        const fake = fs.readFileSync(
          path.join(import.meta.dir, 'fixtures/fake-aac-http.mjs'),
          'utf8'
        );
        fs.writeFileSync(
          binary,
          fake.replace(
            "if (req.url === '/api/health') {",
            "if (req.url === '/api/health') { return;"
          ),
          { mode: 0o755 }
        );
      }
      let timerMs = Infinity;
      const began = performance.now();
      const timer = setTimeout(() => {
        timerMs = performance.now() - began;
      }, 50);
      try {
        const instance = await startServer({
          port: 0,
          host: '127.0.0.1',
          rustService: 'shadow',
          rustServiceBinary: binary,
        });
        instances.push(instance);
        const port = (instance.server.address() as AddressInfo).port;
        const reply = await fetch(`http://127.0.0.1:${port}/api/health`);
        expect(reply.status).toBe(200);
        expect(performance.now() - began).toBeLessThan(1500);
        await new Promise((r) => setTimeout(r, 75));
        expect(timerMs).toBeLessThan(500);
      } finally {
        clearTimeout(timer);
        unblock?.kill();
      }
    }, 120_000);
  }
});
