import { mkdir } from 'node:fs/promises';
import type { AddressInfo } from 'node:net';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { ConfigError } from '../errors/error-types';
import { loadNativeStore } from '../store/native-loader';
import { getCcsDir } from '../utils/config-manager';
import { hubRows, publishHubCache, type RenderedHubRows } from './compose-server';
import { startServer } from './index';
import { rustGroupOn } from './rust-service-groups';
import { registerRustStoreConnection, setRustStoreHosted } from './rust-service-transport';
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
  // Windows has one store engine in the Rust listener. The worker uses the same
  // bounded store protocol on the private per-boot loopback channel.
  const storeFeed = process.env.AAC_CUT_STORE_FEED;
  if (process.platform === 'win32' && storeFeed) {
    const port = /^http:\/\/127\.0\.0\.1:(\d+)$/.exec(storeFeed)?.[1];
    if (!port || Number(port) < 1 || Number(port) > 65535)
      throw new ConfigError('Invalid dashboard store feed');
    setRustStoreHosted(ccs, true);
    let build: { commit: string; storeSchema: number } | undefined;
    try {
      const identity = loadNativeStore().module?.buildIdentity?.();
      if (identity) build = JSON.parse(identity);
    } catch {
      // The hosted handle checks identity before it opens.
    }
    registerRustStoreConnection(
      ccs,
      { socketPath: `tcp:127.0.0.1:${port}`, secret: proof, bootId: proof.slice(0, 24), build },
      new Set(),
      {},
      true
    );
  }
  const directory = process.env.AAC_CUT_HUB_DIRECTORY;
  if (!directory || !path.isAbsolute(directory))
    throw new ConfigError('Invalid dashboard sidecar cache directory');
  process.env.AAC_CUT_OFFER_HOME =
    rustGroupOn('home') || process.env.AAC_RUST_HOME_VIEW === 'on' ? '1' : '0';
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
  process.stdout.write(JSON.stringify({ sidecar: 1, port: address.port }) + '\n');
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
