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
import { NetworkError } from '../errors/error-types';
import { getCcsDir } from '../utils/config-manager';
import { attachDashboardEventFeed } from './dashboard-events';
import { startServer, type ServerInstance, type ServerOptions } from './index';
import { peekAccountDashboard } from './services/account-dashboard-service';
import type { AccountDashboard, ClaudeDashboardPlatform } from './services/account-dashboard-types';

type PublicListeners = { publicAddress: AddressInfo; httpsAddress: AddressInfo | null };

function launched(child: ChildProcess, interactive: boolean): Promise<PublicListeners> {
  if (!child.stdout) return Promise.reject(new Error('Rust dashboard stdout was unavailable'));
  const lines = createInterface({ input: child.stdout });
  return new Promise((resolve, reject) => {
    const fail = () => {
      dispose();
      reject(new Error('Composed Rust dashboard did not start'));
    };
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
    let httpsAddress: AddressInfo | null = null;
    lines.on('line', (line) => {
      const match = /^aac-http public (TLS )?listening on (\[[0-9a-f:]+\]|[0-9.]+):([0-9]+)$/i.exec(
        line
      );
      if (!match) {
        if (
          line === 'Valid for 60 minutes. Saved in auth/setup-code in the CCS folder (0600).' ||
          (interactive &&
            /^Dashboard first-run setup code: [2-9A-HJ-NP-Z]{4}-[2-9A-HJ-NP-Z]{4}$/.test(line))
        ) {
          process.stdout.write(line + '\n');
        }
        return;
      }
      const address = match[2].replace(/^\[|\]$/g, '');
      const bound = {
        address,
        port: Number(match[3]),
        family: isIP(address) === 6 ? 'IPv6' : 'IPv4',
      };
      if (match[1]) {
        httpsAddress = bound;
        return;
      }
      dispose();
      resolve({ publicAddress: bound, httpsAddress });
    });
    child.once('close', dispose);
  });
}

/** The H2 handoff while TS owns collection. Only the public usage-hub fields enter the file. */
export function hubRows(dashboard: AccountDashboard) {
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

export type RenderedHubRows = { at: number; updatedAt: string; rows: ReturnType<typeof hubRows> };
const PLATFORMS = ['mac', 'windows'] as const;

export async function publishHubCache(
  target: string,
  native: ReadonlyMap<ClaudeDashboardPlatform, RenderedHubRows>
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
): Promise<ServerInstance & PublicListeners & { restartPublic: () => Promise<void> }> {
  const root = getCcsDir();
  await fs.mkdir(root, { recursive: true, mode: 0o700 });
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
  const native = new Map<ClaudeDashboardPlatform, RenderedHubRows>();
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
    const startPublic = async (port: number): Promise<PublicListeners> => {
      const interactive = process.stdout.isTTY === true;
      const listener = spawn(binary, ['--public'], {
        windowsHide: true,
        stdio: ['pipe', 'pipe', 'inherit'],
        env: {
          ...process.env,
          CCS_DIR: root,
          AAC_RUST_PUBLIC_LISTEN: (isIP(ip) === 6 ? '[' + ip + ']' : ip) + ':' + port,
          AAC_RUST_PUBLIC_EVENT_STDIN: '1',
          AAC_RUST_UI_DIR: options.staticDir ?? path.join(__dirname, '../ui'),
          AAC_RUST_HUB_CACHE: cache,
          AAC_RUST_COMPOSE_UPSTREAM: '127.0.0.1:' + address.port,
          AAC_RUST_COMPOSE_PROOF: proof,
          AAC_RUST_COMPOSE_STDOUT_TTY: interactive ? '1' : '0',
        },
      });
      child = listener;
      if (listener.stdin) detach = attachDashboardEventFeed(listener.stdin);
      const started = await launched(listener, interactive);
      listener.on('error', close);
      listener.once('exit', close);
      return started;
    };
    const listeners = await startPublic(options.port);
    backend.server.once('close', close);
    const restartPublic = async () => {
      const listener = child;
      if (!listener || closing || listener.exitCode !== null)
        throw new NetworkError('Rust dashboard is stopped');
      detach?.();
      listener.off('exit', close);
      const stopped = once(listener, 'exit');
      listener.stdin?.end();
      await stopped;
      Object.assign(listeners, await startPublic(listeners.publicAddress.port));
    };
    return {
      ...backend,
      get publicAddress() {
        return listeners.publicAddress;
      },
      get httpsAddress() {
        return listeners.httpsAddress;
      },
      restartPublic,
    };
  } catch (error) {
    close();
    if (child?.exitCode === null) child.kill();
    throw error;
  }
}
