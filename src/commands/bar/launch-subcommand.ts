/**
 * `ai-account-center bar launch` — start the AI Account Center server detached, write bar.json, open the app.
 *
 * bar.json shape (v1):
 *   { baseUrl: string, port: number, authMode: "loopback" }
 *
 * Detached model (replaces the old in-process model):
 *   1. Probe candidate ports (bar.json port first, then 3000/3001/3002/8000/8080).
 *   2. If a live server is found → reuse it, write bar.json, open app, return.
 *      With an explicit --port that differs from the running server's port,
 *      stop that server first and fall through to a fresh start instead.
 *   3. Else → pick a port (--port exactly when given; otherwise bar.json's
 *      recorded port first, then the default candidates), refresh launch.json
 *      (including --port so the Swift app self-starts on the same port), spawn
 *      `ai-account-center bar serve --port N` detached with stdio → serve.log, poll
 *      /api/bar/auth until 200 (timeout ~10 s), write bar.json, open app,
 *      return. The CLI process exits; the server continues as a detached child.
 *
 * All side-effectful deps are injectable so tests can run without real
 * servers, ports, or file-system writes.
 */

import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { ChildProcess } from 'child_process';
import { getCcsDir } from '../../config/config-loader-facade';
import {
  BAR_AUTH_NONCE_HEADER,
  BAR_AUTH_TOKEN_HEADER,
  createBarAuthNonce,
  isMatchingBarAuthProof,
  getOrCreateBarAuthToken,
} from '../../utils/bar-auth-token';
import { getBarDir, getBarJsonPath, getLaunchJsonPath, getServeLogPath } from './bar-paths';
import type { LaunchJson } from './bar-paths';
import { createBarLaunchDescriptor } from './launch-descriptor';
import { parsePortFlag, validatePortArgs } from './port-arg';
import {
  defaultFindRunningServer as _defaultFindRunningServer,
  resolveBarPort as _resolveBarPort,
} from './bar-server-probe';
import type { DashboardInfo as _DashboardInfo } from './bar-server-probe';
import { stopDetachedBarServer } from './bar-process-control';
import { getMacAppPaths, resolveOwnedMacApp } from './native-app-paths';

const BAR_PROBE_TIMEOUT_MS = 1500;
const MAX_BAR_PROBE_RESPONSE_BYTES = 8192;

// ---------------------------------------------------------------------------
// Re-exports — backward compat for tests that import from this module.
// resolveBarPort + defaultFindRunningServer are canonical in bar-server-probe.ts;
// we re-export them so existing imports from launch-subcommand continue to work.
// ---------------------------------------------------------------------------

export { _defaultFindRunningServer as defaultFindRunningServer };
export { _resolveBarPort as resolveBarPort };

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface BarDiscoveryJson {
  baseUrl: string;
  port: number;
  authMode: 'loopback';
}

export type DashboardInfo = _DashboardInfo;

export interface LaunchDeps {
  /**
   * Probe candidate ports for a running AI Account Center server.
   * Returns { port, baseUrl } of the first live server found, or null if none.
   * Never throws — any error is treated as "not found".
   */
  findRunningServer: () => Promise<DashboardInfo | null>;
  /**
   * Find a free port from the candidate list.
   * Used to pre-select a port before spawning the detached server.
   */
  getPort: (opts: { port: number[]; host: string }) => Promise<number>;
  /**
   * Spawn the `ai-account-center bar serve --port N` process detached and return immediately.
   * The spawned process must be unref()ed so the launcher can exit.
   */
  spawnDetachedServer: (port: number, logPath: string) => ChildProcess | void;
  /**
   * Poll GET {baseUrl}/api/bar/auth until HTTP 200 or timeout.
   * Returns the live baseUrl on success, throws on timeout.
   */
  waitForServerLive: (baseUrl: string) => Promise<void>;
  /**
   * Build the launch.json descriptor (includes the chosen --port so the Swift
   * app self-starts the server on the same port).
   */
  createLaunchDescriptor: (opts?: { port?: number }) => LaunchJson;
  /**
   * Write launch.json so the Swift app can spawn the server independently.
   */
  writeLaunchDescriptor: (jsonPath: string, descriptor: LaunchJson) => void;
  /**
   * Stop the detached AI Account Center server recorded in server.pid and wait briefly
   * for the port to free. Used when an explicit --port differs from the port
   * the running server occupies.
   */
  stopDetachedServer: (ccsDir: string) => Promise<void>;
  /** Open the installed .app bundle. Throws if the app is not found. */
  openApp: (appPath: string) => Promise<void>;
  /** Returns path to ~/.ccs (respects CCS_HOME for test isolation). */
  getCcsDir: () => string;
  /** Full path where the .app should be installed, e.g. ~/Applications/AI Account Center.app */
  appInstallPath: string;
  /** Native app launch is supported on macOS; injectable for isolated fixtures. */
  getPlatform: () => NodeJS.Platform;
}

// ---------------------------------------------------------------------------
// Default production dependencies
// ---------------------------------------------------------------------------

async function defaultGetPort(opts: { port: number[]; host: string }): Promise<number> {
  const getPort = (await import('get-port')).default;
  return getPort(opts);
}

/**
 * Spawn `ai-account-center bar serve --port N` detached so it outlives this CLI process.
 *
 * stdio is redirected to serve.log so server output is preserved for
 * debugging without a terminal.  unref() lets the launcher exit immediately.
 */
function defaultSpawnDetachedServer(port: number, logPath: string): ChildProcess {
  const { spawn } = require('child_process') as typeof import('child_process');

  // Open (or create) the log file for appending.
  const logFd = fs.openSync(logPath, 'a');
  const child = spawn(process.execPath, [process.argv[1], 'bar', 'serve', '--port', String(port)], {
    detached: true,
    stdio: ['ignore', logFd, logFd],
    cwd: os.homedir(),
    env: process.env,
  });
  child.unref();
  // Close our copy of the fd — the child has its own reference.
  fs.closeSync(logFd);
  return child;
}

/**
 * Poll GET {baseUrl}/api/bar/auth every 250 ms until HTTP 200 or ~10 s.
 * Resolves when the server is live. Rejects on timeout.
 */
export class BarServerAuthRequiredError extends Error {
  constructor(baseUrl: string, statusCode: number) {
    super(
      `AI Account Center server at ${baseUrl} requires dashboard authentication (HTTP ${statusCode})`
    );
    this.name = 'BarServerAuthRequiredError';
  }
}

export class BarServerTimeoutError extends Error {
  constructor(baseUrl: string, timeoutSeconds: number) {
    super(`AI Account Center server did not become live at ${baseUrl} within ${timeoutSeconds}s`);
    this.name = 'BarServerTimeoutError';
  }
}

function isAuthRequiredStatus(statusCode: number): boolean {
  return statusCode === 401 || statusCode === 403;
}

export async function defaultWaitForServerLive(baseUrl: string, ccsDir?: string): Promise<void> {
  const net = await import('net');
  const token = getOrCreateBarAuthToken(ccsDir);
  const INTERVAL_MS = 250;
  const TIMEOUT_MS = 10_000;
  const deadline = Date.now() + TIMEOUT_MS;

  async function probe(): Promise<{ statusCode: number | null; tokenMatched: boolean }> {
    const url = new URL(`${baseUrl}/api/bar/auth`);
    const nonce = createBarAuthNonce();
    return new Promise((resolve) => {
      let rawResponse = '';
      let settled = false;
      const absoluteDeadline = setTimeout(() => finish(), BAR_PROBE_TIMEOUT_MS);
      absoluteDeadline.unref?.();
      const finish = (statusCode: number | null = null, headerSection = '') => {
        if (settled) return;
        settled = true;
        clearTimeout(absoluteDeadline);
        socket.destroy();
        const echoMatch = headerSection.match(
          new RegExp(`${BAR_AUTH_TOKEN_HEADER}:\\s*([^\\r\\n]+)`, 'i')
        );
        const proof = echoMatch ? echoMatch[1].trim() : '';
        resolve({ statusCode, tokenMatched: isMatchingBarAuthProof(token, nonce, proof) });
      };
      const socket = net.connect(
        { host: url.hostname.replace(/^\[|\]$/g, ''), port: Number(url.port) },
        () => {
          // Do NOT include the token in the request; only send a fresh nonce so
          // the server can prove it knows the token without disclosing it.
          socket.write(
            `GET ${url.pathname}${url.search} HTTP/1.1\r\nHost: ${url.host}\r\n${BAR_AUTH_NONCE_HEADER}: ${nonce}\r\nConnection: close\r\n\r\n`
          );
        }
      );
      socket.setTimeout(BAR_PROBE_TIMEOUT_MS, () => finish());
      socket.on('data', (chunk) => {
        rawResponse += chunk.toString('utf8');
        if (rawResponse.length > MAX_BAR_PROBE_RESPONSE_BYTES) {
          finish();
          return;
        }
        const statusMatch = rawResponse.match(/^HTTP\/\d(?:\.\d)?\s+(\d{3})/);
        if (statusMatch) {
          const code = Number(statusMatch[1]);
          if (code !== 200 && code !== 401 && code !== 403) {
            finish(code, rawResponse);
            return;
          }
          if (rawResponse.includes('\r\n\r\n')) {
            finish(code, rawResponse.split('\r\n\r\n')[0]);
          }
        }
      });
      socket.on('error', () => finish());
      socket.on('end', () => {
        const statusMatch = rawResponse.match(/^HTTP\/\d(?:\.\d)?\s+(\d{3})/);
        if (statusMatch) finish(Number(statusMatch[1]), rawResponse);
        else finish();
      });
    });
  }

  while (Date.now() < deadline) {
    const { statusCode, tokenMatched } = await probe();

    if (statusCode === 200 && tokenMatched) return;
    if (statusCode !== null && isAuthRequiredStatus(statusCode) && tokenMatched) {
      throw new BarServerAuthRequiredError(baseUrl, statusCode);
    }

    await new Promise<void>((resolve) => setTimeout(resolve, INTERVAL_MS));
  }

  throw new BarServerTimeoutError(baseUrl, TIMEOUT_MS / 1000);
}

function defaultWriteLaunchDescriptor(jsonPath: string, descriptor: LaunchJson): void {
  fs.mkdirSync(path.dirname(jsonPath), { recursive: true });
  fs.writeFileSync(jsonPath, JSON.stringify(descriptor, null, 2));
}

async function defaultOpenApp(appPath: string): Promise<void> {
  const { execFile } = await import('child_process');
  const { promisify } = await import('util');
  const execFileAsync = promisify(execFile);
  await execFileAsync('/usr/bin/codesign', ['--verify', '--deep', '--strict', appPath]);
  await execFileAsync('/usr/bin/open', ['-a', appPath]);
}

function defaultGetCcsDir(): string {
  return getCcsDir();
}

// Fix #5: use os.homedir() to match install-subcommand.ts and uninstall-subcommand.ts.
function defaultAppInstallPath(): string {
  const paths = getMacAppPaths();
  return resolveOwnedMacApp(paths.appsDir)?.path ?? paths.canonical;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

export async function handleBarLaunch(
  _args: string[],
  deps: Partial<LaunchDeps> = {}
): Promise<void> {
  const argError = validatePortArgs(_args);
  if (argError !== null) {
    console.error(`[X] ${argError}`);
    process.exitCode = 1;
    return;
  }
  if ((deps.getPlatform ?? (() => process.platform))() !== 'darwin' && !deps.openApp) {
    console.error('[X] Native bar launch from the CLI is supported on macOS.');
    console.error('[i] On Windows, use the installed AI Account Center tray app.');
    process.exitCode = 1;
    return;
  }
  let appInstallPath: string;
  try {
    appInstallPath = deps.appInstallPath ?? defaultAppInstallPath();
  } catch (error) {
    console.error(`[X] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
    return;
  }
  const ccsDir = (deps.getCcsDir ?? defaultGetCcsDir)();
  const openApp = deps.openApp ?? defaultOpenApp;
  const getPortFn = deps.getPort ?? defaultGetPort;
  const spawnDetachedServer = deps.spawnDetachedServer ?? defaultSpawnDetachedServer;
  const waitForServerLive =
    deps.waitForServerLive ?? ((baseUrl) => defaultWaitForServerLive(baseUrl, ccsDir));
  const createLaunchDescriptor = deps.createLaunchDescriptor ?? createBarLaunchDescriptor;
  const writeLaunchDescriptor = deps.writeLaunchDescriptor ?? defaultWriteLaunchDescriptor;
  const stopDetachedServer = deps.stopDetachedServer ?? stopDetachedBarServer;

  // Wire findRunningServer after ccsDir is resolved.
  const findRunningServer = deps.findRunningServer ?? (() => _defaultFindRunningServer(ccsDir));

  const barJsonPath = getBarJsonPath(ccsDir);
  const launchJsonPath = getLaunchJsonPath(ccsDir);

  // 0. Parse --port. A present-but-invalid value is a hard error (silently
  //    launching on a different port than the user asked for is worse).
  const portFlag = parsePortFlag(_args);
  if (portFlag.present && portFlag.port === null) {
    console.error('[X] Invalid --port value. Use a number between 1 and 65535.');
    process.exitCode = 1;
    return;
  }
  const requestedPort = portFlag.port;

  // 1. Probe for an already-running server.
  let running: DashboardInfo | null = null;
  try {
    running = await findRunningServer();
  } catch {
    /* any probe error counts as null */
  }

  let movingFrom: DashboardInfo | null = null;
  let port: number | null = null;

  if (running !== null) {
    if (running.authRequired) {
      console.error(
        `[X] AI Account Center cannot launch while dashboard authentication protects ${running.baseUrl}.`
      );
      console.error(
        '[i] Disable dashboard authentication for AI Account Center or start the dashboard manually.'
      );
      return;
    }

    if (requestedPort === null || running.port === requestedPort) {
      // Reuse the live server — write bar.json and open the app.
      const barJson: BarDiscoveryJson = {
        baseUrl: running.baseUrl,
        port: running.port,
        authMode: 'loopback',
      };
      try {
        fs.mkdirSync(ccsDir, { recursive: true });
        fs.writeFileSync(barJsonPath, JSON.stringify(barJson, null, 2));
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        console.error(`[X] Failed to write bar.json: ${msg}`);
        return;
      }
      console.log(`[OK] Reusing running AI Account Center dashboard server at ${running.baseUrl}`);
      console.log(`[i]  Discovery file written: ${barJsonPath}`);
      await _openAppWithFallback(appInstallPath, openApp);
      return;
    }

    // Explicit --port that differs from the running server: preflight the
    // destination before disrupting the healthy current service.
    console.log(
      `[i] AI Account Center server is running on port ${running.port}; moving to port ${requestedPort}...`
    );
    if (requestedPort === null) return;
    try {
      const availablePort = await getPortFn({ port: [requestedPort], host: '127.0.0.1' });
      if (availablePort !== requestedPort) {
        console.error(`[X] Port ${requestedPort} is already in use by another process.`);
        console.error('[i] The existing AI Account Center server was left running.');
        process.exitCode = 1;
        return;
      }
      port = requestedPort;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error(`[X] Could not preflight port ${requestedPort}: ${msg}`);
      console.error('[i] The existing AI Account Center server was left running.');
      process.exitCode = 1;
      return;
    }
    try {
      await stopDetachedServer(ccsDir);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error(`[X] Could not safely stop the running server: ${msg}`);
      console.error('[i] Recovery state was preserved; resolve the stop error, then retry.');
      process.exitCode = 1;
      return;
    }
    movingFrom = running;
    // Fall through to the fresh-start path below.
  }

  // 2. No live server (or moving ports) — pick a port, write/refresh
  //    launch.json, spawn detached.

  // 2a. Pick a free port. An explicit --port must be honored exactly; without
  //     it, the port recorded in bar.json is preferred so the server keeps
  //     coming back on the port the user last chose (sticky port).
  try {
    if (port !== null) {
      // Destination was already preflighted before stopping the prior server.
    } else if (requestedPort !== null) {
      const got = await getPortFn({ port: [requestedPort], host: '127.0.0.1' });
      if (got !== requestedPort) {
        console.error(`[X] Port ${requestedPort} is already in use by another process.`);
        console.error('[i] Choose a different port or free it, then retry.');
        process.exitCode = 1;
        return;
      }
      port = requestedPort;
    } else {
      const stickyPort = _resolveBarPort(ccsDir);
      const base = [3000, 3001, 3002, 8000, 8080];
      const candidates =
        stickyPort !== null ? [stickyPort, ...base.filter((p) => p !== stickyPort)] : base;
      port = await getPortFn({ port: candidates, host: '127.0.0.1' });
    }
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.error(`[X] Could not find a free port: ${msg}`);
    return;
  }

  if (port === null) {
    console.error('[X] Could not resolve a valid AI Account Center port.');
    process.exitCode = 1;
    return;
  }
  const selectedPort = port;

  const rollbackPriorServer = async (): Promise<void> => {
    if (movingFrom === null) return;
    try {
      spawnDetachedServer(movingFrom.port, serveLogPath);
      await waitForServerLive(movingFrom.baseUrl);
      console.log(`[OK] Restored AI Account Center server at ${movingFrom.baseUrl}.`);
    } catch (rollbackErr) {
      const message = rollbackErr instanceof Error ? rollbackErr.message : String(rollbackErr);
      console.error(`[X] Failed to restore AI Account Center at ${movingFrom.baseUrl}: ${message}`);
      console.error('[i] Existing discovery and launch state was preserved for manual recovery.');
    }
  };

  // 2b. Spawn the detached server. launch.json is not replaced until the new
  // server is proven live, preserving the prior recovery path during a move.
  const serveLogPath = getServeLogPath(ccsDir);
  const baseUrl = `http://127.0.0.1:${selectedPort}`;
  let spawnedChild: ChildProcess | void;
  try {
    fs.mkdirSync(getBarDir(ccsDir), { recursive: true });
    spawnedChild = spawnDetachedServer(selectedPort, serveLogPath);
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.error(`[X] Could not start AI Account Center dashboard server: ${msg}`);
    await rollbackPriorServer();
    if (movingFrom === null)
      console.error('[i] Run `ai-account-center dashboard` to start the dashboard manually.');
    process.exitCode = 1;
    return;
  }

  console.log('[i] Starting AI Account Center server...');

  // 2d. Poll until live or timeout.
  try {
    await waitForServerLive(baseUrl);
  } catch (err) {
    if (err instanceof BarServerAuthRequiredError) {
      spawnedChild?.kill();
      console.error(
        `[X] AI Account Center cannot launch while dashboard authentication protects ${baseUrl}.`
      );
      console.error(
        '[i] Disable dashboard authentication for AI Account Center or start the dashboard manually.'
      );
    } else {
      const msg = err instanceof Error ? err.message : String(err);
      console.error(`[X] Could not connect to AI Account Center dashboard server: ${msg}`);
      console.error(`[i] Check logs at ${serveLogPath}`);
    }
    spawnedChild?.kill();
    await rollbackPriorServer();
    process.exitCode = 1;
    return;
  }

  // 2d. Persist the proven-good recovery descriptor.
  try {
    const launchDescriptor = createLaunchDescriptor({ port: selectedPort });
    writeLaunchDescriptor(launchJsonPath, launchDescriptor);
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.error(`[!] Could not write launch.json: ${msg}`);
  }

  // 2e. Write bar.json.
  const barJson: BarDiscoveryJson = {
    baseUrl,
    port: selectedPort,
    authMode: 'loopback',
  };
  try {
    fs.mkdirSync(ccsDir, { recursive: true });
    fs.writeFileSync(barJsonPath, JSON.stringify(barJson, null, 2));
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.error(`[X] Failed to write bar.json: ${msg}`);
    return;
  }

  console.log(`[OK] AI Account Center dashboard server running at ${baseUrl}`);
  console.log(`[i]  Discovery file written: ${barJsonPath}`);

  // 3. Open the app — then return (process exits; server continues detached).
  await _openAppWithFallback(appInstallPath, openApp);
}

// ---------------------------------------------------------------------------
// Internal helper — open app with graceful degraded path
// ---------------------------------------------------------------------------

async function _openAppWithFallback(
  appInstallPath: string,
  openApp: (p: string) => Promise<void>
): Promise<void> {
  try {
    await openApp(appInstallPath);
    console.log('[OK] AI Account Center launched.');
  } catch {
    if (!fs.existsSync(appInstallPath)) {
      console.log('[!] AI Account Center app is not installed.');
      console.log('[i] Run `ai-account-center bar install` to install it.');
    } else {
      console.log(
        '[!] Could not open AI Account Center. Check its signature and the macOS launch error.'
      );
    }
  }
}
