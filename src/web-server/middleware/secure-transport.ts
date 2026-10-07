import type { IncomingMessage } from 'http';
import type { TLSSocket } from 'tls';
import * as net from 'net';
import { isLoopbackRemoteAddress } from './auth-middleware';
import {
  formatAddress,
  isAddressInNetworks,
  isLoopbackAddress,
  normalizePeerAddress,
  parseAddress,
  type ParsedAddress,
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
 * 3. a trusted TLS proxy (`dashboard_tls.trusted_proxy`) with `X-Forwarded-Proto`
 *    exactly `https`. The two local kinds (`tailscale-serve`,
 *    `loopback-https-proxy`) need a loopback peer. The `lan-https-proxy` kind
 *    (an HTTPS reverse proxy on another LAN computer) needs the socket peer to
 *    be one of `dashboard_tls.trusted_proxy_addresses`, and the rightmost
 *    `X-Forwarded-For` entry (the client the proxy itself saw) to be a valid IP
 *    address. Forwarded headers from any other peer are ignored;
 * 4. a trusted local network (amended 2026-10-02, owner's choice): the owner
 *    turned on `dashboard_network.trust_local_network`, the peer address is in
 *    `dashboard_network.trusted_networks` (default the private ranges), the
 *    peer is not loopback (loopback is rule 2's alone, Host test included),
 *    the peer is not a configured `lan-https-proxy` address, and the request
 *    carries no forwarded headers (a proxy's clients are unknown, whatever
 *    address the proxy itself has). Plain HTTP then counts as secure for that
 *    peer. Public, link-local, CGNAT and unknown peers stay refused unless the
 *    list names them.
 *
 * Rules 3 and 4 read config.yaml through resolvers that the server wires at
 * startup (`configureDashboardTransport`); until then both are off.
 */
export type TrustedProxyKind = 'tailscale-serve' | 'loopback-https-proxy' | 'lan-https-proxy';

let trustedProxyResolver: () => TrustedProxyKind | null = () => null;

/** Wire the configured trusted proxy (CONTRACT-auth-devices 2a, `dashboard_tls`). */
export function setTrustedProxyResolver(resolver: () => TrustedProxyKind | null): void {
  trustedProxyResolver = resolver;
}

function configuredTrustedProxy(options: SecureTransportOptions = {}): TrustedProxyKind | null {
  if (options.trustedProxy !== undefined) return options.trustedProxy;
  try {
    return trustedProxyResolver();
  } catch {
    return null;
  }
}

let trustedProxyAddressesResolver: () => readonly string[] = () => [];

/**
 * Wire `dashboard_tls.trusted_proxy_addresses` (only `lan-https-proxy` reads
 * it; the settings parser already refused anything but exact private LAN
 * addresses); null turns it off again.
 */
export function setTrustedProxyAddressesResolver(resolver: (() => readonly string[]) | null): void {
  trustedProxyAddressesResolver = resolver ?? (() => []);
}

/** The configured proxy addresses; empty when the resolver fails. */
export function trustedProxyAddresses(): readonly string[] {
  try {
    const addresses = trustedProxyAddressesResolver();
    return Array.isArray(addresses) ? addresses : [];
  } catch {
    return [];
  }
}

/**
 * Whether an address is one of `addresses`, compared as bytes after
 * normalisation (`::ffff:192.0.2.5` matches `192.0.2.5`).
 */
export function isTrustedProxyAddress(
  value: string | undefined | null,
  addresses: readonly string[]
): boolean {
  const peer = parseAddress(value);
  if (!peer) return false;
  return addresses.some((entry) => {
    const candidate = typeof entry === 'string' ? parseAddress(entry) : null;
    if (!candidate || candidate.family !== peer.family) return false;
    return candidate.bytes.every((byte, index) => byte === peer.bytes[index]);
  });
}

/**
 * The socket peer is the LAN HTTPS proxy: `trusted_proxy` is
 * `lan-https-proxy` AND the socket address exactly matches one entry of
 * `trusted_proxy_addresses`. An empty list matches nothing, so the kind is off.
 * Forwarded headers never decide this; only the socket does.
 */
export function isLanProxyPeer(
  req: IncomingMessage,
  options: SecureTransportOptions = {}
): boolean {
  if (configuredTrustedProxy(options) !== 'lan-https-proxy') return false;
  const addresses = options.trustedProxyAddresses ?? trustedProxyAddresses();
  return isTrustedProxyAddress(req.socket?.remoteAddress, addresses);
}

/**
 * Express `trust proxy` (CONTRACT-auth-devices 2a, rule 3): which hop's
 * `X-Forwarded-For` Express may read. The local kinds trust loopback hops; the
 * `lan-https-proxy` kind trusts only the socket peer (hop 0) and only when it is
 * a configured proxy address, so `req.ip` is exactly the rightmost
 * `X-Forwarded-For` entry, the client the proxy itself saw. Nothing else is
 * ever trusted.
 */
export function isTrustedProxyHop(address: string | undefined, hop: number): boolean {
  const kind = configuredTrustedProxy();
  if (kind === 'lan-https-proxy') {
    return hop === 0 && isTrustedProxyAddress(address, trustedProxyAddresses());
  }
  return kind !== null && isLoopbackRemoteAddress(address);
}

/**
 * Express took `req.ip` from `X-Forwarded-For`: the request names a forwarded
 * client and its socket peer is a trusted proxy hop. The rate limits key such
 * a request apart from every direct one (rate-limit-keys.ts).
 */
export function isForwardedThroughTrustedProxy(req: IncomingMessage): boolean {
  if (req.headers['x-forwarded-for'] === undefined) return false;
  return isTrustedProxyHop(req.socket?.remoteAddress, 0);
}

/**
 * The rightmost `X-Forwarded-For` entry, the client the proxy itself saw, when
 * it is exactly an IP address (no brackets, zone, port or text); otherwise
 * null. Entries further left were sent by the client and are never used.
 * Several headers are read as one list, as Node joins them.
 */
export function forwardedClientAddress(req: IncomingMessage): ParsedAddress | null {
  const raw = req.headers['x-forwarded-for'];
  const text = Array.isArray(raw) ? raw.join(',') : raw;
  if (typeof text !== 'string') return null;
  const entries = text.split(',');
  const rightmost = entries[entries.length - 1].trim();
  if (rightmost.includes('%') || net.isIP(rightmost) === 0) return null;
  return parseAddress(rightmost);
}

/**
 * The client address for the request log, audit lines and a device's "last
 * seen": behind a trusted proxy hop the forwarded client Express read
 * (normalised, or `invalid` when it is not an IP address), else the socket peer
 * exactly as before.
 */
export function requestClientAddress(req: IncomingMessage & { ip?: unknown }): string | null {
  if (isForwardedThroughTrustedProxy(req)) {
    const client = typeof req.ip === 'string' ? req.ip.trim() : '';
    if (client.includes('%') || net.isIP(client) === 0) return 'invalid';
    return normalizePeerAddress(client) ?? 'invalid';
  }
  return req.socket?.remoteAddress ?? null;
}

/** Rule 3 for the LAN proxy: the outer transport was exactly https, for a client it names. */
function lanProxyRequestIsSecure(req: IncomingMessage): boolean {
  return (
    singleHeader(req.headers['x-forwarded-proto']) === 'https' &&
    forwardedClientAddress(req) !== null
  );
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
  trustedProxyAddresses?: readonly string[];
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
 * - a configured `lan-https-proxy` address, with or without forwarded
 *   headers: the proxy's clients are unknown, and a proxy that sent no headers
 *   would otherwise pass its trust to every client behind it;
 * - a request that carries forwarded headers, from any other peer: a reverse
 *   proxy on the LAN (a NAS or router) would otherwise pass its trust to every
 *   client behind it, internet clients included.
 */
export function isTrustedLocalNetworkPeer(
  req: IncomingMessage,
  trust: LocalNetworkTrust = localNetworkTrust()
): boolean {
  if (!trust.enabled) return false;
  if (isLanProxyPeer(req)) return false;
  const address = parseAddress(req.socket?.remoteAddress);
  if (!address || isLoopbackAddress(address)) return false;
  if (carriesForwardedHeaders(req)) return false;
  return isAddressInNetworks(address, trust.networks);
}

/** What Settings shows as "This connection" (`GET /api/auth/check`, `/setup`, `/network`). */
export interface ConnectionDescription {
  peer: string;
  trusted: boolean;
  /**
   * Present and true only when the request came through the LAN HTTPS proxy;
   * `peer` is then the client the proxy saw (the rightmost `X-Forwarded-For`
   * entry), or `unknown` when that is not an IP address.
   */
  proxied?: true;
}

/**
 * What Settings shows as "This connection": the normalised peer and whether
 * rule 4 holds. `trusted` is false for a loopback peer: the dashboard computer
 * itself is secure by rule 2, not by the local network trust. Through the LAN
 * HTTPS proxy the socket peer is the proxy, so `peer` names the client instead,
 * `trusted` is false and `proxied` marks the hop.
 */
export function describeConnection(req: IncomingMessage): ConnectionDescription {
  if (isLanProxyPeer(req)) {
    const client = forwardedClientAddress(req);
    return { peer: client ? formatAddress(client) : 'unknown', trusted: false, proxied: true };
  }
  return {
    peer: normalizePeerAddress(req.socket?.remoteAddress) ?? 'unknown',
    trusted: isTrustedLocalNetworkPeer(req),
  };
}

/**
 * Loopback as `requireLocalAccessWhenAuthDisabled` tests it: the peer and the
 * Host header are loopback, no local proxy forwarded the request, and the peer
 * is not the LAN HTTPS proxy (a request through it is never the dashboard
 * computer; the settings parser refuses loopback proxy addresses anyway).
 */
export function isDirectLoopbackRequest(req: IncomingMessage): boolean {
  if (isLanProxyPeer(req)) return false;
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
  // The LAN HTTPS proxy: secure only when it says the outer transport was
  // exactly https, for a client it names as an IP address. Anything else from
  // that peer is not secure, and rule 4 never trusts the peer either.
  if (isLanProxyPeer(req, options)) return lanProxyRequestIsSecure(req);
  if (isTrustedLocalNetworkPeer(req, options.localNetworkTrust ?? localNetworkTrust())) return true;
  if (!isLoopbackRemoteAddress(socket?.remoteAddress)) return false;
  // A request that a local proxy forwarded came from somewhere else; without a
  // configured trusted proxy its outer transport is unknown, so rule 2 does not apply.
  const proxied = carriesForwardedHeaders(req);
  if (!proxied && isLoopbackHostName(hostName(singleHeader(req.headers.host)))) return true;
  const trustedProxy = configuredTrustedProxy(options);
  if (trustedProxy !== 'tailscale-serve' && trustedProxy !== 'loopback-https-proxy') return false;
  return singleHeader(req.headers['x-forwarded-proto']) === 'https';
}

/**
 * How a credential handed out or presented on this request crosses the
 * network: `encrypted` (in-process TLS, or a trusted HTTPS proxy whose outer
 * transport was https), `loopback` (rule 2: it never leaves the dashboard
 * computer), or `plain` (everything else, including rule 4's trusted local
 * network, which is plain HTTP on the wire). Sessions and device tokens record
 * it, so the LAN HTTPS proxy can refuse one that crossed the LAN in plain text.
 */
export type CredentialTransport = 'encrypted' | 'loopback' | 'plain';

export function credentialTransport(
  req: IncomingMessage,
  options: SecureTransportOptions = {}
): CredentialTransport {
  const socket = req.socket as (TLSSocket & { encrypted?: boolean }) | undefined;
  if (socket?.encrypted === true) return 'encrypted';
  if (isLanProxyPeer(req, options)) return lanProxyRequestIsSecure(req) ? 'encrypted' : 'plain';
  if (!isLoopbackRemoteAddress(socket?.remoteAddress)) return 'plain';
  if (!carriesForwardedHeaders(req)) {
    return isLoopbackHostName(hostName(singleHeader(req.headers.host))) ? 'loopback' : 'plain';
  }
  const trustedProxy = configuredTrustedProxy(options);
  if (trustedProxy !== 'tailscale-serve' && trustedProxy !== 'loopback-https-proxy') return 'plain';
  return singleHeader(req.headers['x-forwarded-proto']) === 'https' ? 'encrypted' : 'plain';
}

/**
 * Whether a session or device token whose recorded transport is `recorded`
 * may be used on this request. Through the LAN HTTPS proxy only a secure
 * request (rule 3) with one that never crossed the network in plain text is
 * accepted, so a cookie or tray key sniffed on the home network does not work
 * from the internet. Anywhere else every credential works exactly as before.
 * A missing record (a session or device from before this rule) counts as
 * plain.
 */
export function credentialAllowedOnRequest(req: IncomingMessage, recorded: unknown): boolean {
  if (!isLanProxyPeer(req)) return true;
  return lanProxyRequestIsSecure(req) && (recorded === 'encrypted' || recorded === 'loopback');
}
