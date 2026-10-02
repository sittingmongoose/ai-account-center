import type { IncomingMessage } from 'http';
import type { TLSSocket } from 'tls';
import * as net from 'net';
import { isLoopbackRemoteAddress } from './auth-middleware';

/**
 * isSecureTransport(req) (CONTRACT-auth-devices section 2a). Passwords, API
 * keys, authorization codes and device codes are accepted or returned only
 * over a transport that is encrypted end to end:
 *
 * 1. in-process TLS (`req.socket.encrypted === true`);
 * 2. loopback: the peer is loopback and the Host header names loopback, which
 *    covers a browser on the VM and an ssh tunnel to 127.0.0.1;
 * 3. a trusted local TLS proxy: the peer is loopback, `dashboard_tls.trusted_proxy`
 *    is `tailscale-serve` or `loopback-https-proxy`, and `X-Forwarded-Proto` is
 *    exactly `https`. Forwarded headers from any other peer are ignored.
 *
 * The trusted-proxy setting has no home in the typed config yet (the
 * `dashboard_tls` block arrives with the auth-devices work), so the default
 * resolver reports none and rule 3 stays off until it is wired.
 */
export type TrustedProxyKind = 'tailscale-serve' | 'loopback-https-proxy';

let trustedProxyResolver: () => TrustedProxyKind | null = () => null;

/** Wire the configured trusted proxy (CONTRACT-auth-devices 2a, `dashboard_tls`). */
export function setTrustedProxyResolver(resolver: () => TrustedProxyKind | null): void {
  trustedProxyResolver = resolver;
}

function singleHeader(value: string | string[] | undefined): string | undefined {
  return Array.isArray(value) ? (value.length === 1 ? value[0] : undefined) : value;
}

function hostName(value: string | undefined): string | null {
  if (!value) return null;
  try {
    return new URL(`http://${value}`).hostname.toLowerCase();
  } catch {
    return null;
  }
}

/** `localhost`, `127.0.0.1` (any 127/8), `[::1]`: the Host test of requireLocalAccessWhenAuthDisabled. */
function isLoopbackHostName(value: string | null): boolean {
  if (!value) return false;
  const bare = value.replace(/^\[|\]$/g, '');
  return (
    bare === 'localhost' ||
    bare.endsWith('.localhost') ||
    (net.isIP(bare) !== 0 && isLoopbackRemoteAddress(bare))
  );
}

const FORWARDED_HEADERS = ['forwarded', 'x-forwarded-for', 'x-forwarded-proto', 'x-real-ip'];

export interface SecureTransportOptions {
  trustedProxy?: TrustedProxyKind | null;
}

export function isSecureTransport(
  req: IncomingMessage,
  options: SecureTransportOptions = {}
): boolean {
  const socket = req.socket as (TLSSocket & { encrypted?: boolean }) | undefined;
  if (socket?.encrypted === true) return true;
  if (!isLoopbackRemoteAddress(socket?.remoteAddress)) return false;
  // A request that a local proxy forwarded came from somewhere else; without a
  // configured trusted proxy its outer transport is unknown, so rule 2 does not apply.
  const proxied = FORWARDED_HEADERS.some((name) => req.headers[name] !== undefined);
  if (!proxied && isLoopbackHostName(hostName(singleHeader(req.headers.host)))) return true;
  const trustedProxy =
    options.trustedProxy === undefined ? trustedProxyResolver() : options.trustedProxy;
  if (trustedProxy !== 'tailscale-serve' && trustedProxy !== 'loopback-https-proxy') return false;
  return singleHeader(req.headers['x-forwarded-proto']) === 'https';
}
