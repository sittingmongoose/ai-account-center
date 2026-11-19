import { mkdir, mkdtemp } from 'node:fs/promises';
import type { AddressInfo } from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { ConfigError } from '../errors/error-types';
import { getCcsDir } from '../utils/config-manager';
import { hubRows, publishHubCache, type RenderedHubRows } from './compose-server';
import { startServer } from './index';
import type { ClaudeDashboardPlatform } from './services/account-dashboard-types';
import { setupGracefulShutdown } from './shutdown';

async function main(): Promise<void> {
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  let closed = false;
  input.once('close', () => {
    closed = true;
  });
  const proof = await new Promise<string | undefined>((resolve) => {
    input.once('line', resolve);
    input.once('close', () => resolve(undefined));
  });
  if (!proof || !/^[0-9a-f]{64}$/.test(proof))
    throw new ConfigError('Invalid dashboard sidecar boot');
  const ccs = getCcsDir();
  await mkdir(ccs, { recursive: true, mode: 0o700 });
  const directory = await mkdtemp(path.join(os.tmpdir(), 'aac-sidecar-'));
  process.env.AAC_CUT_HUB_DIRECTORY = directory;
  const cache = path.join(directory, 'hub.json');
  const native = new Map<ClaudeDashboardPlatform, RenderedHubRows>();
  const refresh = () => {
    if (!writing)
      writing = publishHubCache(cache, native)
        .catch(() => {})
        .finally(() => {
          writing = undefined;
        });
  };
  let writing: Promise<void> | undefined;
  const server = await startServer({
    host: '127.0.0.1',
    port: 0,
    compositionProof: proof,
    sidecar: true,
    rustService: 'on',
    onRustDashboard(platform, dashboard) {
      native.set(platform, {
        at: Date.now(),
        updatedAt: dashboard.updatedAt,
        rows: hubRows(dashboard),
      });
      refresh();
    },
  });
  const address = server.server.address() as AddressInfo;
  process.stdout.write(JSON.stringify({ sidecar: 1, port: address.port, cache }) + '\n');
  const { attachDashboardEventFeed } = await import('./dashboard-events');
  const detach = attachDashboardEventFeed(process.stdout);
  refresh();
  const timer = setInterval(refresh, 2500);
  timer.unref();
  const shutdown = setupGracefulShutdown(server.server, server.wss, () => {
    clearInterval(timer);
    detach();
    server.cleanup();
  });
  if (closed) shutdown();
  else input.once('close', shutdown);
}

void main().catch(() => {
  console.error('[X] The private dashboard sidecar could not start.');
  process.exit(1);
});
