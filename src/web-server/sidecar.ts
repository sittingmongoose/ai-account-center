import { mkdir, rename, writeFile } from 'node:fs/promises';
import type { AddressInfo } from 'node:net';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { ConfigError } from '../errors/error-types';
import { getCcsDir } from '../utils/config-manager';
import { hubRows, publishHubCache, type RenderedHubRows } from './compose-server';
import { startServer } from './index';
import { rustGroupOn } from './rust-service-groups';
import type { ClaudeDashboardPlatform } from './services/account-dashboard-types';
import { getSignInJobRunner, lifecycleProviderFacts } from './services/account-lifecycle-runtime';
import { setupGracefulShutdown } from './shutdown';

async function publishLifecycleFacts(directory: string): Promise<void> {
  const facts = lifecycleProviderFacts({ secureTransport: true });
  const disabled = (values: Record<string, boolean | undefined> = {}) =>
    Object.keys(values).filter((provider) => values[provider] === false);
  const text = JSON.stringify({
    mounted: facts.lifecycleRoutes,
    blocked: facts.flows ?? {},
    removeDisabled: disabled(facts.remove),
    recheckDisabled: disabled(facts.recheck),
    jobs: Object.fromEntries(getSignInJobRunner().accountStates()),
  });
  if (Buffer.byteLength(text) > 65536) return;
  const target = path.join(directory, 'lifecycle.json');
  await writeFile(target + '.next', text, { mode: 0o600 });
  await rename(target + '.next', target);
}

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
  const directory = process.env.AAC_CUT_HUB_DIRECTORY;
  if (!directory || !path.isAbsolute(directory))
    throw new ConfigError('Invalid dashboard sidecar cache directory');
  process.env.AAC_CUT_OFFER_HOME =
    rustGroupOn('home') || process.env.AAC_RUST_HOME_VIEW === 'on' ? '1' : '0';
  const cache = path.join(directory, 'hub.json');
  const collection = process.platform !== 'win32' && rustGroupOn('collection');
  const saveLifecycle = () => (collection ? publishLifecycleFacts(directory) : Promise.resolve());
  await saveLifecycle().catch(() => {});
  const native = new Map<ClaudeDashboardPlatform, RenderedHubRows>();
  const refresh = () => {
    if (!writing)
      writing = Promise.allSettled([publishHubCache(cache, native), saveLifecycle()])
        .then(() => {})
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
