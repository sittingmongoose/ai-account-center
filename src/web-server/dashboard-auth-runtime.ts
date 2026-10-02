import type { Express, Request, Response } from 'express';
import fs from 'fs';
import http from 'http';
import https from 'https';
import os from 'os';
import path from 'path';
import { createLogger } from '../services/logging';
import { isLoopbackRemoteAddress } from './middleware/auth-middleware';
import {
  setLocalNetworkTrustResolver,
  setTrustedProxyResolver,
} from './middleware/secure-transport';
import { dashboardAuthState, sendAuthError } from './routes/auth-route-helpers';
import { authFile } from './services/dashboard-auth-files';
import { createSetupCode } from './services/dashboard-setup-code';
import {
  getDashboardTlsSettings,
  type DashboardHttpsListenerSettings,
} from './services/dashboard-tls-config';
import { getDashboardNetworkSettings } from './services/dashboard-network-config';

/**
 * Server-side wiring for dashboard sign-in (CONTRACT-auth-devices 2a and 4):
 * the trusted local TLS proxy, the optional in-process HTTPS listener and the
 * first-run setup code. Everything is off by default; `dashboard_tls` in
 * config.yaml turns each part on, and nothing here changes that file.
 */
const logger = createLogger('dashboard-auth');

/**
 * Rule 3 of isSecureTransport, plus Express `trust proxy` for loopback peers
 * only, and only while `dashboard_tls.trusted_proxy` is set. X-Forwarded-*
 * from any other peer is never trusted. Rule 4 reads `dashboard_network`
 * (the owner's trusted local network, off by default).
 */
export function configureDashboardTransport(app: Express): void {
  setTrustedProxyResolver(() => getDashboardTlsSettings().trustedProxy);
  setLocalNetworkTrustResolver(() => {
    const settings = getDashboardNetworkSettings();
    return { enabled: settings.trustLocalNetwork, networks: settings.networks };
  });
  app.set(
    'trust proxy',
    (address: string) =>
      getDashboardTlsSettings().trustedProxy !== null && isLoopbackRemoteAddress(address)
  );
}

/**
 * Under /api/auth (any letter case) a body or routing error answers like the
 * auth routes do: `no-store` and a stable code (section 2). Returns false for
 * other paths, which keep their existing answers.
 */
export function sendAuthPathError(
  req: Request,
  res: Response,
  status: number,
  code: string,
  error: string
): boolean {
  if (!/^\/api\/auth(?:\/|$)/i.test(req.path)) return false;
  sendAuthError(res, status, code, error);
  return true;
}

/** Where the code file is, for the message: home-relative, never a full path outside the home folder. */
function setupCodeLocation(): string {
  const file = authFile('setup-code');
  const home = os.homedir();
  return home && file.startsWith(`${home}${path.sep}`)
    ? `~${file.slice(home.length)}`
    : 'auth/setup-code in the CCS folder';
}

/**
 * Printed once to the terminal that started the server, and never to a log
 * file: when stdout is not a terminal (`bar launch` sends it to
 * ~/.ccs/bar/serve.log, a service manager to its journal), only the file's
 * location is printed, not the code.
 */
export function printSetupCode(
  code: string,
  interactive: boolean = process.stdout.isTTY === true
): void {
  process.stdout.write(
    interactive
      ? `\n  Dashboard first-run setup code: ${code}\n` +
          `  Valid for 60 minutes. Also saved in ${setupCodeLocation()} (0600).\n\n`
      : `\n  Dashboard first-run setup code saved in ${setupCodeLocation()} (0600).\n` +
          '  Valid for 60 minutes. Read it there on this computer.\n\n'
  );
}

/** While sign-in is on but has no password yet, make this run's one-time setup code. */
export async function prepareFirstRunSetupCode(
  print: (code: string) => void = (code) => printSetupCode(code)
): Promise<string | null> {
  const state = dashboardAuthState();
  if (!state.enabled || state.configured || state.managedBy === 'env') return null;
  try {
    return await createSetupCode(print);
  } catch {
    logger.warn('auth.setup.code_failed', 'The first-run setup code could not be written');
    return null;
  }
}

function readTlsFiles(settings: DashboardHttpsListenerSettings): {
  cert: Buffer;
  key: Buffer;
} | null {
  try {
    const keyStat = fs.lstatSync(settings.keyPath);
    if (!keyStat.isFile()) return null;
    if (process.platform !== 'win32' && (keyStat.mode & 0o077) !== 0) {
      logger.warn(
        'auth.tls.key_refused',
        'The dashboard TLS key is readable by others; set it to 0600'
      );
      return null;
    }
    return { cert: fs.readFileSync(settings.certPath), key: fs.readFileSync(settings.keyPath) };
  } catch {
    return null;
  }
}

/**
 * Start the optional in-process HTTPS listener on the same app. A missing or
 * unsafe certificate leaves it off (with one warning) and never stops the
 * plain listener.
 */
export async function startDashboardHttpsListener(
  app: Express,
  host: string,
  onUpgrade: (request: http.IncomingMessage, socket: import('stream').Duplex, head: Buffer) => void,
  settings = getDashboardTlsSettings().httpsListener
): Promise<https.Server | null> {
  if (!settings) return null;
  const files = readTlsFiles(settings);
  if (!files) {
    logger.warn('auth.tls.listener_off', 'The dashboard HTTPS listener stays off');
    return null;
  }
  let server: https.Server;
  try {
    server = https.createServer({ cert: files.cert, key: files.key }, app);
  } catch {
    logger.warn('auth.tls.listener_off', 'The dashboard HTTPS listener stays off');
    return null;
  }
  server.on('upgrade', onUpgrade);
  return new Promise((resolve) => {
    const onError = () => {
      logger.warn('auth.tls.listener_off', 'The dashboard HTTPS listener could not bind');
      resolve(null);
    };
    server.once('error', onError);
    server.listen(settings.port, host, () => {
      server.off('error', onError);
      logger.info('auth.tls.listening', 'Dashboard HTTPS listener started', {
        port: settings.port,
      });
      resolve(server);
    });
  });
}
