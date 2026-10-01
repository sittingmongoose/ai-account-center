/**
 * AI Account Center — authenticated server liveness probes.
 *
 * Shared by launch-subcommand.ts and serve-subcommand.ts so neither imports
 * from the other (which would create a cross-module dependency that breaks
 * Bun's test module isolation when cache-busting URLs are used).
 */

import * as fs from 'fs';
import * as path from 'path';
import {
  BAR_AUTH_NONCE_HEADER,
  BAR_AUTH_TOKEN_HEADER,
  createBarAuthNonce,
  isMatchingBarAuthProof,
  getOrCreateBarAuthToken,
  getBarAuthTokenPath,
} from '../../utils/bar-auth-token';

const PROBE_TIMEOUT_MS = 1500;
const MAX_PROBE_RESPONSE_BYTES = 8192;
export const BAR_AUTH_PATH = '/api/bar/auth';

export interface DashboardInfo {
  port: number;
  baseUrl: string;
  authRequired?: boolean;
}

export interface BarProbeOptions {
  /** Isolated callers may supply only endpoints they own; default discovery is unchanged. */
  targets?: readonly Pick<DashboardInfo, 'port' | 'baseUrl'>[];
}

function isValidPort(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 1 && (value as number) <= 65535;
}

/**
 * Read the port recorded in an existing bar.json, falling back to the --port
 * in launch.json's args. `ccs bar stop` deletes bar.json but leaves
 * launch.json, so the fallback is what keeps the sticky port (and the probe's
 * ability to find a server on a non-default port) across a stop/start cycle.
 * Returns null when neither file records a port.
 */

export function resolveBarPort(ccsDir: string): number | null {
  const barJsonPath = path.join(ccsDir, 'bar.json');
  try {
    const raw = fs.readFileSync(barJsonPath, 'utf8');
    const parsed = JSON.parse(raw) as Partial<{ port: number }>;
    if (isValidPort(parsed.port)) return parsed.port;
  } catch {
    /* fall through to launch.json */
  }

  const launchJsonPath = path.join(ccsDir, 'bar', 'launch.json');
  try {
    const raw = fs.readFileSync(launchJsonPath, 'utf8');
    const parsed = JSON.parse(raw) as Partial<{ args: unknown[] }>;
    const args = Array.isArray(parsed.args) ? parsed.args : [];
    const idx = args.indexOf('--port');
    if (idx !== -1 && idx + 1 < args.length) {
      const rawPort = args[idx + 1];
      if (typeof rawPort === 'string' && /^[1-9]\d{0,4}$/.test(rawPort)) {
        const n = Number(rawPort);
        if (isValidPort(n)) return n;
      }
    }
  } catch {
    /* absent or malformed -> null */
  }
  return null;
}

/**
 * Probe candidate ports for a running CCS server.
 *
 * Both IPv4 (127.0.0.1) and IPv6 (::1) loopback addresses are probed for each
 * port. All probes are fired concurrently so worst-case latency is ~1.5 s
 * (one timeout) rather than N × 1.5 s sequentially. Results are awaited in
 * priority order so a lower-priority slow or streaming response cannot block
 * returning an already-known higher-priority hit.
 *
 * Each probe speaks raw HTTP/1.1 over a socket and resolves on the status line,
 * which lets discovery distinguish a live-but-auth-protected server (401/403)
 * from a healthy one (200) without depending on a higher-level HTTP client.
 *
 * Token authentication: the probe does NOT send the token in the request.
 * Instead, it sends a fresh nonce. The real CCS Bar server reads the token from
 * the 0600 file and returns HMAC(token, nonce) in the x-ccs-bar-token response
 * header. The probe verifies the nonce-bound proof, so a captured proof cannot
 * be replayed for a future probe.
 */
async function probe(url: string, token: string): Promise<{ ok: boolean; authRequired: boolean }> {
  const net = await import('net');
  const parsed = new URL(url);
  const port = Number(parsed.port);
  const host = parsed.hostname.replace(/^\[|\]$/g, '');
  const nonce = createBarAuthNonce();

  return new Promise((resolve) => {
    let rawResponse = '';
    let settled = false;
    const absoluteDeadline = setTimeout(() => finish(), PROBE_TIMEOUT_MS);
    absoluteDeadline.unref?.();
    const finish = (statusCode = 0, headerSection = '') => {
      if (settled) return;
      settled = true;
      clearTimeout(absoluteDeadline);
      // Tear down the socket the moment we have enough to decide. The auth
      // endpoint needs only the status and nonce proof for liveness, so a non-CCS
      // loopback service that streams forever cannot block discovery from
      // returning a higher-priority hit.
      socket.destroy();
      const proofMatch = headerSection.match(
        new RegExp(`${BAR_AUTH_TOKEN_HEADER}:\\s*([^\\r\\n]+)`, 'i')
      );
      const proof = proofMatch ? proofMatch[1].trim() : '';
      const tokenMatched = isMatchingBarAuthProof(token, nonce, proof);
      const authRequired = (statusCode === 401 || statusCode === 403) && tokenMatched;
      if (authRequired) {
        resolve({ ok: true, authRequired: true });
        return;
      }
      if (statusCode === 200) {
        resolve({ ok: tokenMatched, authRequired: false });
        return;
      }
      resolve({ ok: false, authRequired: false });
    };
    const socket = net.connect({ host, port }, () => {
      // Do NOT include the token in the request; only send a fresh nonce so
      // the server can prove it knows the token without disclosing it.
      socket.write(
        `GET ${parsed.pathname}${parsed.search} HTTP/1.1\r\nHost: ${parsed.host}\r\n${BAR_AUTH_NONCE_HEADER}: ${nonce}\r\nConnection: close\r\n\r\n`
      );
    });
    socket.setTimeout(PROBE_TIMEOUT_MS, () => finish());
    socket.on('data', (chunk) => {
      rawResponse += chunk.toString('utf8');
      if (rawResponse.length > MAX_PROBE_RESPONSE_BYTES) {
        finish();
        return;
      }
      const statusMatch = rawResponse.match(/^HTTP\/\d(?:\.\d)?\s+(\d{3})/);
      if (statusMatch) {
        const code = Number(statusMatch[1]);
        // CCS-authenticated 401/403 responses also carry the nonce proof, so
        // wait for their complete headers before deciding identity.
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

/**
 * Verify one recorded loopback server without creating or changing a token.
 * A plain HTTP 200 cannot identify the installed server. The existing private
 * token must validate the response's fresh nonce-bound HMAC proof.
 */
export async function probeRecordedBarServer(ccsDir: string, baseUrl: string): Promise<boolean> {
  try {
    const parsed = new URL(baseUrl);
    if (
      parsed.protocol !== 'http:' ||
      !['127.0.0.1', '[::1]'].includes(parsed.hostname) ||
      parsed.username ||
      parsed.password ||
      parsed.pathname !== '/' ||
      parsed.search ||
      parsed.hash ||
      !isValidPort(Number(parsed.port || 80))
    ) {
      return false;
    }
    const tokenPath = getBarAuthTokenPath(ccsDir);
    if (!fs.lstatSync(path.dirname(tokenPath)).isDirectory() || !fs.lstatSync(tokenPath).isFile()) {
      return false;
    }
    const tokenFd = fs.openSync(
      tokenPath,
      fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK
    );
    let token: string;
    try {
      const metadata = fs.fstatSync(tokenFd);
      if (!metadata.isFile() || metadata.size > 128) return false;
      token = fs.readFileSync(tokenFd, 'utf8').trim();
    } finally {
      fs.closeSync(tokenFd);
    }
    if (!/^[a-f0-9]{64}$/i.test(token)) return false;
    const result = await probe(new URL(BAR_AUTH_PATH, parsed).href, token);
    return result.ok;
  } catch {
    return false;
  }
}

export async function defaultFindRunningServer(
  ccsDir: string,
  options: BarProbeOptions = {}
): Promise<DashboardInfo | null> {
  const token = getOrCreateBarAuthToken(ccsDir);

  const barJsonPort = resolveBarPort(ccsDir);
  const base = [3000, 3001, 3002, 8000, 8080];
  const candidates: number[] =
    barJsonPort !== null ? [barJsonPort, ...base.filter((p) => p !== barJsonPort)] : base;

  const defaultTargets = candidates.flatMap((port) => [
    { port, baseUrl: `http://127.0.0.1:${port}`, url: `http://127.0.0.1:${port}${BAR_AUTH_PATH}` },
    { port, baseUrl: `http://[::1]:${port}`, url: `http://[::1]:${port}${BAR_AUTH_PATH}` },
  ]);
  const probeTargets = options.targets
    ? options.targets.map(({ port, baseUrl }) => ({
        port,
        baseUrl,
        url: `${baseUrl}${BAR_AUTH_PATH}`,
      }))
    : defaultTargets;
  // Overrides are an injectable boundary, never an unrestricted URL proxy.
  for (const target of probeTargets) {
    try {
      const parsed = new URL(target.baseUrl);
      if (
        !isValidPort(target.port) ||
        parsed.protocol !== 'http:' ||
        !['127.0.0.1', '[::1]'].includes(parsed.hostname) ||
        Number(parsed.port || 80) !== target.port ||
        parsed.username ||
        parsed.password ||
        parsed.pathname !== '/' ||
        parsed.search ||
        parsed.hash
      ) {
        return null;
      }
      target.url = new URL(BAR_AUTH_PATH, parsed).href;
    } catch {
      return null;
    }
  }

  const probes = probeTargets.map((t) => probe(t.url, token));

  for (let i = 0; i < probeTargets.length; i++) {
    const result = await probes[i];
    if (result.ok) {
      const { port, baseUrl } = probeTargets[i];
      return { port, baseUrl, authRequired: result.authRequired };
    }
  }
  return null;
}
