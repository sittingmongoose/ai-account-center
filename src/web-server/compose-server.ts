import { loadOrCreateUnifiedConfig } from '../config/unified-config-loader';
import { once } from 'node:events';
import { spawn, type ChildProcess } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import fs from 'node:fs/promises';
import type { AddressInfo } from 'node:net';
import { isIP } from 'node:net';
import { lookup } from 'node:dns';
import os from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { getCcsDir } from '../utils/config-manager';
import { attachDashboardEventFeed } from './dashboard-events';
import { startServer, type ServerInstance, type ServerOptions } from './index';
import { peekAccountDashboard } from './services/account-dashboard-service';
import type { AccountDashboard, ClaudeDashboardPlatform } from './services/account-dashboard-types';

function launched(child: ChildProcess): Promise<AddressInfo> {
  if (!child.stdout) return Promise.reject(new Error('Rust dashboard stdout was unavailable'));
  const lines = createInterface({ input: child.stdout });
  return new Promise((resolve, reject) => {
    const fail = () => reject(new Error('Composed Rust dashboard did not start'));
    const timeout = setTimeout(fail, 15_000);
    child.once('error', fail);
    child.once('exit', fail);
    const dispose = () => {
      clearTimeout(timeout);
      lines.close();
      child.stdout?.resume();
      child.off('error', fail);
      child.off('exit', fail);
    };
    lines.on('line', (line) => {
      const match = /^aac-http public listening on (\[[0-9a-f:]+\]|[0-9.]+):([0-9]+)$/i.exec(line);
      if (!match) return;
      const address = match[1].replace(/^\[|\]$/g, '');
      dispose();
      resolve({ address, port: Number(match[2]), family: isIP(address) === 6 ? 'IPv6' : 'IPv4' });
    });
    child.once('close', dispose);
  });
}

/** The H2 handoff while TS owns collection. Only the public usage-hub fields enter the file. */
function hubRows(dashboard: AccountDashboard) {
  return {
    accounts: dashboard.accounts
      .filter(({ provider }) => provider === 'codex' || provider === 'claude')
      .map(
        ({
          id,
          provider,
          label,
          email,
          plan,
          sampledAt,
          fetchedAt,
          status,
          windows,
          lifecycle,
        }) => ({
          id,
          provider,
          label,
          email,
          plan,
          sampledAt,
          fetchedAt,
          status,
          lifecycle: lifecycle ? { state: lifecycle.state } : undefined,
          windows: windows.map(
            ({ key, label, usedPercent, resetPassed, kind, resetAt, windowMinutes }) => ({
              key,
              label,
              usedPercent,
              resetPassed,
              kind,
              resetAt,
              windowMinutes,
            })
          ),
        })
      ),
  };
}

type Rendered = { at: number; updatedAt: string; rows: ReturnType<typeof hubRows> };
const PLATFORMS = ['mac', 'windows'] as const;

async function publishHubCache(
  target: string,
  native: ReadonlyMap<ClaudeDashboardPlatform, Rendered>
): Promise<void> {
  const dashboards = await Promise.allSettled(
    PLATFORMS.map((platform) => peekAccountDashboard(platform))
  );
  const cached = dashboards.flatMap((result, index) => {
    const rendered = native.get(PLATFORMS[index]);
    const update = result.status === 'fulfilled' ? result.value : undefined;
    if (
      rendered &&
      Date.now() - rendered.at < 5000 &&
      (!update || rendered.updatedAt >= update.updatedAt)
    )
      return [rendered.rows];
    return update ? [hubRows(update)] : [];
  });
  if (!cached.length) return;
  const bytes = JSON.stringify(cached);
  if (Buffer.byteLength(bytes) > 4 * 1024 * 1024) return;
  const staged = target + '.next';
  await fs.writeFile(staged, bytes, { mode: 0o600 });
  await fs.rename(staged, target);
}

/** Start the opt-in Rust listener, with the retained TS adapters on a private TCP port. */
export async function startComposedServer(
  options: ServerOptions,
  binary: string
): Promise<ServerInstance & { restartPublic: () => Promise<void> }> {
  // The retained adapters require the unified-config schema even on a fresh
  // root. Set it up before Rust can persist the first dashboard credentials.
  loadOrCreateUnifiedConfig();
  const host = options.host ?? 'localhost';
  const ip =
    host === 'localhost'
      ? '127.0.0.1'
      : await new Promise<string>((resolve, reject) => {
          lookup(host.replace(/^\[|\]$/g, ''), (error, address) =>
            error ? reject(error) : resolve(address)
          );
        });
  const proof = randomBytes(32).toString('hex');
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-compose-'));
  const cache = path.join(directory, 'hub.json');
  let backend: ServerInstance | undefined;
  let child: ChildProcess | undefined;
  let detach: (() => void) | undefined;
  let timer: ReturnType<typeof setInterval> | undefined;
  let writing: Promise<void> | undefined;
  const native = new Map<ClaudeDashboardPlatform, Rendered>();
  let closing = false;
  const close = () => {
    if (closing) return;
    closing = true;
    if (timer) clearInterval(timer);
    detach?.();
    child?.stdin?.end();
    if (backend?.server.listening) {
      backend.server.close();
      backend.server.closeAllConnections();
    }
    void Promise.resolve(writing)
      .finally(() => fs.rm(directory, { recursive: true, force: true }))
      .catch(() => {});
  };
  const refresh = () => {
    if (!writing)
      writing = publishHubCache(cache, native)
        .catch(() => {})
        .finally(() => {
          writing = undefined;
        });
  };
  try {
    backend = await startServer({
      ...options,
      host: '127.0.0.1',
      port: 0,
      rustServiceBinary: binary,
      compositionProof: proof,
      onRustDashboard(platform, dashboard) {
        native.set(platform, {
          at: Date.now(),
          updatedAt: dashboard.updatedAt,
          rows: hubRows(dashboard),
        });
        refresh();
      },
    });
    const address = backend.server.address() as AddressInfo;
    refresh();
    timer = setInterval(refresh, 2_500);
    timer.unref();
    const startPublic = async (port: number): Promise<AddressInfo> => {
      const listener = spawn(binary, ['--public'], {
        windowsHide: true,
        stdio: ['pipe', 'pipe', 'inherit'],
        env: {
          ...process.env,
          CCS_DIR: getCcsDir(),
          AAC_RUST_PUBLIC_LISTEN: (isIP(ip) === 6 ? '[' + ip + ']' : ip) + ':' + port,
          AAC_RUST_PUBLIC_EVENT_STDIN: '1',
          AAC_RUST_UI_DIR: options.staticDir ?? path.join(__dirname, '../ui'),
          AAC_RUST_HUB_CACHE: cache,
          AAC_RUST_COMPOSE_UPSTREAM: '127.0.0.1:' + address.port,
          AAC_RUST_COMPOSE_PROOF: proof,
        },
      });
      child = listener;
      if (listener.stdin) detach = attachDashboardEventFeed(listener.stdin);
      const started = await launched(listener);
      listener.on('error', close);
      listener.once('exit', close);
      return started;
    };
    const publicAddress = await startPublic(options.port);
    backend.server.once('close', close);
    const restartPublic = async () => {
      const listener = child;
      if (!listener || closing || listener.exitCode !== null)
        throw new Error('Rust dashboard is stopped');
      detach?.();
      listener.off('exit', close);
      const stopped = once(listener, 'exit');
      listener.stdin?.end();
      await stopped;
      await startPublic(publicAddress.port);
    };
    return { ...backend, publicAddress, restartPublic };
  } catch (error) {
    close();
    if (child?.exitCode === null) child.kill();
    throw error;
  }
}
