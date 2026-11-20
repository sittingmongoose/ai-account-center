import { mkdir } from 'node:fs/promises';
import type { AddressInfo } from 'node:net';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { ConfigError } from '../errors/error-types';
import { getCcsDir } from '../utils/config-manager';
import { startServer } from './index';
import { rustGroupOn } from './rust-service-groups';
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
  const directory = process.env.AAC_CUT_HUB_DIRECTORY;
  if (!directory || !path.isAbsolute(directory))
    throw new ConfigError('Invalid dashboard sidecar cache directory');
  process.env.AAC_CUT_OFFER_HOME =
    rustGroupOn('home') || process.env.AAC_RUST_HOME_VIEW === 'on' ? '1' : '0';
  const server = await startServer({
    host: '127.0.0.1',
    port: 0,
    compositionProof: proof,
    sidecar: true,
    rustService: 'on',
  });
  const address = server.server.address() as AddressInfo;
  process.stdout.write(JSON.stringify({ sidecar: 1, port: address.port }) + '\n');
  const { attachDashboardEventFeed } = await import('./dashboard-events');
  const detach = attachDashboardEventFeed(process.stdout);
  const shutdown = setupGracefulShutdown(server.server, server.wss, () => {
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
