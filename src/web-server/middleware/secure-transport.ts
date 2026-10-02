import type { IncomingMessage } from 'http';
import type { TLSSocket } from 'tls';
import * as net from 'net';
import { isLoopbackRemoteAddress } from './auth-middleware';
import {
  isAddressInNetworks,
  isLoopbackAddress,
  normalizePeerAddress,
  parseAddress,
  type TrustedNetwork,
} from './trusted-networks';

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
 *    exactly `https`. Forwarded headers from any other peer are ignored;
 * 4. a trusted local network (amended 2026-10-02, owner's choice): the owner
 *    turned on `dashboard_network.trust_local_network`, the peer address is in
 *    `dashboard_network.trusted_networks` (default the private ranges), the
 *    peer is not loopback (loopback is rule 2's alone, Host test included), and
 *    the request carries no forwarded headers (a proxy's clients are unknown,
 *    whatever address the proxy itself has). Plain HTTP then counts as secure
 *    for that peer. Public, link-local, CGNAT and unknown peers stay refused
 *    unless the list names them.
 *
 * Rules 3 and 4 read config.yaml through resolvers that the server wires at
 * startup (`configureDashboardTransport`); until then both are off.
 */
export type TrustedProxyKind = 'tailscale-serve' | 'loopback-https-proxy';

let trustedProxyResolver: () => TrustedProxyKind | null = () => null;

/** Wire the configured trusted proxy (CONTRACT-auth-devices 2a, `dashboard_tls`). */
export function setTrustedProxyResolver(resolver: () => TrustedProxyKind | null): void {
  trustedProxyResolver = resolver;
}

/** Rule 4's inputs: the owner's switch and the trusted ranges. */
export interface LocalNetworkTrust {
  enabled: boolean;
  networks: readonly TrustedNetwork[];
}

const LOCAL_NETWORK_TRUST_OFF: LocalNetworkTrust = Object.freeze({ enabled: false, networks: [] });

let localNetworkTrustResolver: () => LocalNetworkTrust = () => LOCAL_NETWORK_TRUST_OFF;

/** Wire rule 4 (CONTRACT-auth-devices 2a, `dashboard_network`); null turns it off again. */
export function setLocalNetworkTrustResolver(resolver: (() => LocalNetworkTrust) | null): void {
  localNetworkTrustResolver = resolver ?? (() => LOCAL_NETWORK_TRUST_OFF);
}

/** The current switch and ranges; off when the resolver fails. */
export function localNetworkTrust(): LocalNetworkTrust {
  try {
    return localNetworkTrustResolver();
  } catch {
    return LOCAL_NETWORK_TRUST_OFF;
  }
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

/**
 * Headers that an HTTP proxy, load balancer or CDN adds for the client behind
 * it. Any one of them means the socket peer is a proxy, not the client.
 */
const FORWARDED_HEADERS = [
  'forwarded',
  'via',
  'x-forwarded-for',
  'x-forwarded-host',
  'x-forwarded-proto',
  'x-forwarded-port',
  'x-forwarded-server',
  'x-original-forwarded-for',
  'x-real-ip',
  'x-client-ip',
  'x-cluster-client-ip',
  'cf-connecting-ip',
  'true-client-ip',
  'fastly-client-ip',
];

export interface SecureTransportOptions {
  trustedProxy?: TrustedProxyKind | null;
  localNetworkTrust?: LocalNetworkTrust;
}

function carriesForwardedHeaders(req: IncomingMessage): boolean {
  return FORWARDED_HEADERS.some((name) => req.headers[name] !== undefined);
}

/**
 * Rule 4. Only the socket's own address counts: X-Forwarded-* never names the
 * peer. It never applies to:
 * - a loopback peer, even when the list names loopback: rule 2 already covers
 *   real loopback and also requires a loopback Host, so a raw TCP forward onto
 *   127.0.0.1 (ssh -R, socat, frp) or a DNS-rebinding page gets nothing here;
 * - a request that carries forwarded headers, from any peer: a reverse proxy
 *   on the LAN (a NAS or router) would otherwise pass its trust to every client
 *   behind it, internet clients included.
 */
export function isTrustedLocalNetworkPeer(
  req: IncomingMessage,
  trust: LocalNetworkTrust = localNetworkTrust()
): boolean {
  if (!trust.enabled) return false;
  const address = parseAddress(req.socket?.remoteAddress);
  if (!address || isLoopbackAddress(address)) return false;
  if (carriesForwardedHeaders(req)) return false;
  return isAddressInNetworks(address, trust.networks);
}

/**
 * What Settings shows as "This connection": the normalised peer and whether
 * rule 4 holds. `trusted` is false for a loopback peer: the dashboard computer
 * itself is secure by rule 2, not by the local network trust.
 */
export function describeConnection(req: IncomingMessage): { peer: string; trusted: boolean } {
  return {
    peer: normalizePeerAddress(req.socket?.remoteAddress) ?? 'unknown',
    trusted: isTrustedLocalNetworkPeer(req),
  };
}

/**
 * Loopback as `requireLocalAccessWhenAuthDisabled` tests it: the peer and the
 * Host header are loopback, and no local proxy forwarded the request.
 */
export function isDirectLoopbackRequest(req: IncomingMessage): boolean {
  if (!isLoopbackRemoteAddress(req.socket?.remoteAddress)) return false;
  if (carriesForwardedHeaders(req)) return false;
  return isLoopbackHostName(hostName(singleHeader(req.headers.host)));
}

export function isSecureTransport(
  req: IncomingMessage,
  options: SecureTransportOptions = {}
): boolean {
  const socket = req.socket as (TLSSocket & { encrypted?: boolean }) | undefined;
  if (socket?.encrypted === true) return true;
  if (isTrustedLocalNetworkPeer(req, options.localNetworkTrust ?? localNetworkTrust())) return true;
  if (!isLoopbackRemoteAddress(socket?.remoteAddress)) return false;
  // A request that a local proxy forwarded came from somewhere else; without a
  // configured trusted proxy its outer transport is unknown, so rule 2 does not apply.
  const proxied = carriesForwardedHeaders(req);
  if (!proxied && isLoopbackHostName(hostName(singleHeader(req.headers.host)))) return true;
  const trustedProxy =
    options.trustedProxy === undefined ? trustedProxyResolver() : options.trustedProxy;
  if (trustedProxy !== 'tailscale-serve' && trustedProxy !== 'loopback-https-proxy') return false;
  return singleHeader(req.headers['x-forwarded-proto']) === 'https';
}
