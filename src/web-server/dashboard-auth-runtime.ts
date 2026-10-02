import type { Express } from 'express';
import fs from 'fs';
import http from 'http';
import https from 'https';
import { createLogger } from '../services/logging';
import { isLoopbackRemoteAddress } from './middleware/auth-middleware';
import { setTrustedProxyResolver } from './middleware/secure-transport';
import { dashboardAuthState } from './routes/auth-route-helpers';
import { createSetupCode } from './services/dashboard-setup-code';
import {
  getDashboardTlsSettings,
  type DashboardHttpsListenerSettings,
} from './services/dashboard-tls-config';

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
 * from any other peer is never trusted.
 */
export function configureDashboardTransport(app: Express): void {
  setTrustedProxyResolver(() => getDashboardTlsSettings().trustedProxy);
  app.set(
    'trust proxy',
    (address: string) =>
      getDashboardTlsSettings().trustedProxy !== null && isLoopbackRemoteAddress(address)
  );
}

function printSetupCode(code: string): void {
  // Printed once to the terminal that started the server; never to the log files.
  process.stdout.write(
    `\n  Dashboard first-run setup code: ${code}\n` +
      '  Valid for 60 minutes. Also saved in ~/.ccs/auth/setup-code (0600).\n\n'
  );
}

/** While sign-in is on but has no password yet, make this run's one-time setup code. */
export async function prepareFirstRunSetupCode(
  print: (code: string) => void = printSetupCode
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
