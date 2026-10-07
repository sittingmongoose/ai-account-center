/**
 * isSecureTransport (CONTRACT-auth-devices section 2a) and authKind.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import type { IncomingMessage } from 'http';
import type { Request } from 'express';
import {
  credentialAllowedOnRequest,
  credentialTransport,
  describeConnection,
  forwardedClientAddress,
  isDirectLoopbackRequest,
  isForwardedThroughTrustedProxy,
  isLanProxyPeer,
  isSecureTransport,
  isTrustedLocalNetworkPeer,
  isTrustedProxyHop,
  requestClientAddress,
  setLocalNetworkTrustResolver,
  setTrustedProxyAddressesResolver,
  setTrustedProxyResolver,
} from '../../../src/web-server/middleware/secure-transport';
import { authKind } from '../../../src/web-server/middleware/request-auth';
import {
  formatAddress,
  parseTrustedNetworks,
} from '../../../src/web-server/middleware/trusted-networks';

const PROXY = '192.168.1.20';
const CLIENT = '203.0.113.7';
const PUBLIC_HOST = 'aac.example.test';

function request(
  remoteAddress: string,
  headers: Record<string, string>,
  encrypted = false
): IncomingMessage {
  return { socket: { remoteAddress, encrypted }, headers } as unknown as IncomingMessage;
}

function viaProxy(
  remoteAddress: string,
  proto: string | undefined,
  forwardedFor: string | undefined
): IncomingMessage {
  return request(remoteAddress, {
    host: PUBLIC_HOST,
    ...(proto === undefined ? {} : { 'x-forwarded-proto': proto }),
    ...(forwardedFor === undefined ? {} : { 'x-forwarded-for': forwardedFor }),
  });
}

function lanProxyOn(addresses: readonly string[] = [PROXY]): void {
  setTrustedProxyResolver(() => 'lan-https-proxy');
  setTrustedProxyAddressesResolver(() => addresses);
}

afterEach(() => {
  setTrustedProxyResolver(() => null);
  setTrustedProxyAddressesResolver(null);
  setLocalNetworkTrustResolver(null);
});

describe('isSecureTransport', () => {
  it('accepts in-process TLS from any peer', () => {
    expect(isSecureTransport(request('192.168.1.20', { host: 'vm:3443' }, true))).toBe(true);
  });

  it('accepts loopback with a loopback Host (a browser on the VM or an ssh tunnel)', () => {
    for (const host of ['localhost:3000', '127.0.0.1:3000', '[::1]:3000', 'localhost']) {
      expect(isSecureTransport(request('127.0.0.1', { host }))).toBe(true);
    }
    expect(isSecureTransport(request('::1', { host: '[::1]:3000' }))).toBe(true);
    expect(isSecureTransport(request('::ffff:127.0.0.1', { host: '127.0.0.1:3000' }))).toBe(true);
  });

  it('refuses plain HTTP from the LAN, whatever the headers say', () => {
    expect(isSecureTransport(request('192.168.1.20', { host: '192.168.1.5:3000' }))).toBe(false);
    expect(isSecureTransport(request('192.168.1.20', { host: 'localhost:3000' }))).toBe(false);
    setTrustedProxyResolver(() => 'tailscale-serve');
    expect(
      isSecureTransport(
        request('192.168.1.20', { host: 'vm.ts.net', 'x-forwarded-proto': 'https' })
      )
    ).toBe(false);
  });

  it('refuses a loopback peer with a LAN Host, or one a local proxy forwarded', () => {
    expect(isSecureTransport(request('127.0.0.1', { host: '192.168.1.5:3000' }))).toBe(false);
    expect(
      isSecureTransport(
        request('127.0.0.1', { host: 'localhost:3000', 'x-forwarded-for': '192.168.1.20' })
      )
    ).toBe(false);
  });

  it('trusts a configured local TLS proxy only with X-Forwarded-Proto https', () => {
    const proxied = (proto?: string) =>
      request('127.0.0.1', {
        host: 'vm.tailnet.ts.net',
        ...(proto === undefined ? {} : { 'x-forwarded-proto': proto }),
      });
    expect(isSecureTransport(proxied('https'))).toBe(false);
    setTrustedProxyResolver(() => 'tailscale-serve');
    expect(isSecureTransport(proxied('https'))).toBe(true);
    expect(isSecureTransport(proxied('http'))).toBe(false);
    expect(isSecureTransport(proxied('HTTPS'))).toBe(false);
    expect(isSecureTransport(proxied())).toBe(false);
    expect(isSecureTransport(proxied('https'), { trustedProxy: null })).toBe(false);
    expect(isSecureTransport(proxied('https'), { trustedProxy: 'loopback-https-proxy' })).toBe(
      true
    );
  });
});

describe('lan-https-proxy', () => {
  const LAN = { trustedProxy: 'lan-https-proxy' as const, trustedProxyAddresses: [PROXY] };

  it('counts a peer as the proxy only with the kind and an exact address match', () => {
    expect(isLanProxyPeer(viaProxy(PROXY, 'https', CLIENT), LAN)).toBe(true);
    // An IPv4-mapped peer matches the plain IPv4 entry.
    expect(isLanProxyPeer(viaProxy(`::ffff:${PROXY}`, 'https', CLIENT), LAN)).toBe(true);
    // Another LAN address, even a neighbour, is not the proxy.
    expect(isLanProxyPeer(viaProxy('192.168.1.21', 'https', CLIENT), LAN)).toBe(false);
    // An empty list means the kind is off, whatever the peer.
    expect(
      isLanProxyPeer(viaProxy(PROXY, 'https', CLIENT), { ...LAN, trustedProxyAddresses: [] })
    ).toBe(false);
    // Without the kind, a listed address is just a LAN peer.
    for (const trustedProxy of [null, 'loopback-https-proxy', 'tailscale-serve'] as const) {
      expect(isLanProxyPeer(viaProxy(PROXY, 'https', CLIENT), { ...LAN, trustedProxy })).toBe(
        false
      );
    }
  });

  it('is secure only with X-Forwarded-Proto https and an IP address as the rightmost X-Forwarded-For entry', () => {
    expect(isSecureTransport(viaProxy(PROXY, 'https', CLIENT), LAN)).toBe(true);
    // The proxy appends the client it saw; entries further left are the client's own words.
    expect(isSecureTransport(viaProxy(PROXY, 'https', `<script>, unknown, ${CLIENT}`), LAN)).toBe(
      true
    );
    expect(isSecureTransport(viaProxy(PROXY, 'https', '2001:db8::7'), LAN)).toBe(true);
    expect(isSecureTransport(viaProxy(PROXY, 'https', ` 198.51.100.4 ,${CLIENT} `), LAN)).toBe(
      true
    );
    for (const [proto, forwardedFor] of [
      ['http', CLIENT],
      ['HTTPS', CLIENT],
      ['https, http', CLIENT],
      ['https,https', CLIENT],
      [undefined, CLIENT],
      ['https', undefined],
      ['https', ''],
      ['https', ' '],
      ['https', 'unknown'],
      ['https', 'not-an-address'],
      ['https', '<script>'],
      // Only the rightmost entry counts: a valid one further left is not enough.
      ['https', `${CLIENT}, <script>`],
      ['https', `${CLIENT},`],
      ['https', `${CLIENT}, unknown`],
      ['https', '[2001:db8::7]'],
      ['https', '203.0.113.7:4431'],
      ['https', 'fe80::1%eth0'],
      ['https', '203.0.113.256'],
      ['https', '0x7f.1'],
      [undefined, undefined],
    ] as Array<[string | undefined, string | undefined]>) {
      expect([
        proto,
        forwardedFor,
        isSecureTransport(viaProxy(PROXY, proto, forwardedFor), LAN),
      ]).toEqual([proto, forwardedFor, false]);
    }
    // The same headers from a peer the list does not name are ignored.
    expect(isSecureTransport(viaProxy('192.168.1.21', 'https', CLIENT), LAN)).toBe(false);
    // Without the kind configured, the same request is not secure.
    expect(isSecureTransport(viaProxy(PROXY, 'https', CLIENT))).toBe(false);
  });

  it('reads the client from the rightmost entry only, normalising IPv6 and mapped forms', () => {
    const client = (forwardedFor: string) => {
      const parsed = forwardedClientAddress(viaProxy(PROXY, 'https', forwardedFor));
      return parsed ? formatAddress(parsed) : null;
    };
    expect(client(CLIENT)).toBe(CLIENT);
    expect(client(`127.0.0.1, ${CLIENT}`)).toBe(CLIENT);
    expect(client(`::ffff:${CLIENT}`)).toBe(CLIENT);
    expect(client('2001:DB8:0:0::7')).toBe('2001:db8::7');
    expect(client(`${CLIENT}, 127.0.0.1`)).toBe('127.0.0.1');
    expect(client(`${CLIENT}, garbage`)).toBeNull();
  });

  it('trusts only hop 0 at a listed address for Express, and loopback hops for the local kinds', () => {
    expect(isTrustedProxyHop(PROXY, 0)).toBe(false);
    lanProxyOn();
    expect(isTrustedProxyHop(PROXY, 0)).toBe(true);
    expect(isTrustedProxyHop(`::ffff:${PROXY}`, 0)).toBe(true);
    // A forwarded entry that names the proxy is never a further trusted hop.
    expect(isTrustedProxyHop(PROXY, 1)).toBe(false);
    expect(isTrustedProxyHop('127.0.0.1', 0)).toBe(false);
    expect(isTrustedProxyHop('192.168.1.21', 0)).toBe(false);
    setTrustedProxyResolver(() => 'tailscale-serve');
    expect(isTrustedProxyHop('127.0.0.1', 0)).toBe(true);
    expect(isTrustedProxyHop(PROXY, 0)).toBe(false);
  });

  it('marks a request as forwarded only from a trusted hop that names a client', () => {
    lanProxyOn();
    expect(isForwardedThroughTrustedProxy(viaProxy(PROXY, 'https', CLIENT))).toBe(true);
    expect(isForwardedThroughTrustedProxy(viaProxy(PROXY, 'https', undefined))).toBe(false);
    // A direct LAN spoofer is keyed and logged as itself.
    const spoofer = viaProxy('192.168.1.21', 'https', '127.0.0.1');
    expect(isForwardedThroughTrustedProxy(spoofer)).toBe(false);
    expect(requestClientAddress(spoofer)).toBe('192.168.1.21');
    const proxied = Object.assign(viaProxy(PROXY, 'https', CLIENT), { ip: CLIENT });
    expect(requestClientAddress(proxied)).toBe(CLIENT);
    const garbage = Object.assign(viaProxy(PROXY, 'https', '<script>'), { ip: '<script>' });
    expect(requestClientAddress(garbage)).toBe('invalid');
  });

  it('is never LAN-trusted, with or without forwarded headers', () => {
    setLocalNetworkTrustResolver(() => ({
      enabled: true,
      networks: parseTrustedNetworks(undefined).networks,
    }));
    lanProxyOn();
    expect(isTrustedLocalNetworkPeer(viaProxy(PROXY, 'https', CLIENT))).toBe(false);
    expect(isTrustedLocalNetworkPeer(request(PROXY, { host: 'vm:3000' }))).toBe(false);
    expect(isSecureTransport(request(PROXY, { host: 'vm:3000' }))).toBe(false);
    // The same peer unlisted keeps the normal LAN trust without headers.
    setTrustedProxyAddressesResolver(() => ['192.168.1.22']);
    expect(isTrustedLocalNetworkPeer(request(PROXY, { host: 'vm:3000' }))).toBe(true);
  });

  it('describes the connection as the real client and the proxy hop', () => {
    lanProxyOn();
    expect(describeConnection(viaProxy(PROXY, 'https', `198.51.100.9, ${CLIENT}`))).toEqual({
      peer: CLIENT,
      trusted: false,
      proxied: true,
    });
    expect(describeConnection(viaProxy(PROXY, 'https', '<script>'))).toEqual({
      peer: 'unknown',
      trusted: false,
      proxied: true,
    });
    // Any other peer keeps the old shape exactly: no proxy hop is marked.
    expect(describeConnection(request('192.168.1.21', { host: 'vm:3000' }))).toEqual({
      peer: '192.168.1.21',
      trusted: false,
    });
  });

  it('never counts as the dashboard computer', () => {
    lanProxyOn(['127.0.0.1']);
    expect(isDirectLoopbackRequest(request('127.0.0.1', { host: 'localhost:3000' }))).toBe(false);
    setTrustedProxyAddressesResolver(null);
    expect(isDirectLoopbackRequest(request('127.0.0.1', { host: 'localhost:3000' }))).toBe(true);
  });
});

describe('credentialTransport and credentialAllowedOnRequest', () => {
  it('names how a credential crosses the network', () => {
    expect(credentialTransport(request('192.168.1.21', { host: 'vm:3443' }, true))).toBe(
      'encrypted'
    );
    expect(credentialTransport(request('127.0.0.1', { host: 'localhost:3000' }))).toBe('loopback');
    expect(credentialTransport(request('127.0.0.1', { host: '192.168.1.10:3000' }))).toBe('plain');
    // The trusted local network is plain HTTP on the wire.
    setLocalNetworkTrustResolver(() => ({
      enabled: true,
      networks: parseTrustedNetworks(undefined).networks,
    }));
    expect(credentialTransport(request('192.168.1.21', { host: 'vm:3000' }))).toBe('plain');
    lanProxyOn();
    expect(credentialTransport(viaProxy(PROXY, 'https', CLIENT))).toBe('encrypted');
    expect(credentialTransport(viaProxy(PROXY, 'http', CLIENT))).toBe('plain');
    expect(credentialTransport(viaProxy(PROXY, 'https', 'garbage'))).toBe('plain');
    setTrustedProxyResolver(() => 'tailscale-serve');
    expect(credentialTransport(viaProxy('127.0.0.1', 'https', CLIENT))).toBe('encrypted');
    expect(credentialTransport(viaProxy('127.0.0.1', 'http', CLIENT))).toBe('plain');
  });

  it('accepts only never-plain credentials through the LAN proxy, and every credential elsewhere', () => {
    lanProxyOn();
    const proxied = viaProxy(PROXY, 'https', CLIENT);
    expect(credentialAllowedOnRequest(proxied, 'encrypted')).toBe(true);
    expect(credentialAllowedOnRequest(proxied, 'loopback')).toBe(true);
    expect(credentialAllowedOnRequest(proxied, 'plain')).toBe(false);
    expect(credentialAllowedOnRequest(proxied, undefined)).toBe(false);
    // A plain request through the proxy carries no credential at all.
    expect(credentialAllowedOnRequest(viaProxy(PROXY, 'http', CLIENT), 'encrypted')).toBe(false);
    expect(credentialAllowedOnRequest(viaProxy(PROXY, 'https', 'bad'), 'encrypted')).toBe(false);
    const lan = request('192.168.1.21', { host: 'vm:3000' });
    for (const recorded of ['encrypted', 'loopback', 'plain', undefined]) {
      expect(credentialAllowedOnRequest(lan, recorded)).toBe(true);
    }
  });
});

describe('authKind', () => {
  it('names a session, a device token first, or nobody', () => {
    expect(authKind({ session: { authenticated: true } } as unknown as Request)).toBe('session');
    expect(authKind({ session: { authenticated: false } } as unknown as Request)).toBeNull();
    expect(authKind({} as Request)).toBeNull();
    expect(
      authKind({
        auth: { kind: 'device' },
        session: { authenticated: true },
      } as unknown as Request)
    ).toBe('device');
  });
});
