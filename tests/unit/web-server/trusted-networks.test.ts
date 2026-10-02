/**
 * Rule 4 of isSecureTransport (CONTRACT-auth-devices section 2a, amended
 * 2026-10-02): the trusted local network. The ranges, IPv4-mapped
 * normalisation, the refusals and the default off, without a server.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import type { IncomingMessage } from 'http';

import {
  describeConnection,
  isSecureTransport,
  isTrustedLocalNetworkPeer,
  localNetworkTrust,
  setLocalNetworkTrustResolver,
  type LocalNetworkTrust,
} from '../../../src/web-server/middleware/secure-transport';
import {
  DEFAULT_TRUSTED_NETWORKS,
  normalizePeerAddress,
  parseTrustedNetwork,
  parseTrustedNetworks,
} from '../../../src/web-server/middleware/trusted-networks';
import { parseDashboardNetworkSettings } from '../../../src/web-server/services/dashboard-network-config';

function request(
  address: string | undefined,
  headers: Record<string, string> = {},
  host = '192.168.50.10:3000'
): IncomingMessage {
  return {
    socket: { remoteAddress: address, encrypted: false },
    headers: { host, ...headers },
  } as unknown as IncomingMessage;
}

function trust(networks: unknown = undefined, enabled = true): LocalNetworkTrust {
  return { enabled, networks: parseTrustedNetworks(networks).networks };
}

afterEach(() => {
  setLocalNetworkTrustResolver(null);
});

describe('trusted ranges', () => {
  it('trusts each default range, inclusive at both ends, and nothing next to them', () => {
    const on = trust();
    const cases: Array<[string, boolean]> = [
      ['10.0.0.0', true],
      ['10.6.0.2', true],
      ['10.255.255.255', true],
      ['9.255.255.255', false],
      ['11.0.0.0', false],
      ['172.16.0.0', true],
      ['172.20.5.4', true],
      ['172.31.255.255', true],
      ['172.15.255.255', false],
      ['172.32.0.0', false],
      ['192.168.0.0', true],
      ['192.168.50.1', true],
      ['192.168.255.255', true],
      ['192.167.255.255', false],
      ['192.169.0.0', false],
      ['fc00::1', true],
      ['fd12:3456:789a::42', true],
      ['fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff', true],
      ['fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff', false],
      ['fe00::1', false],
    ];
    for (const [address, expected] of cases) {
      expect([address, isTrustedLocalNetworkPeer(request(address), on)]).toEqual([
        address,
        expected,
      ]);
    }
  });

  it('refuses public, link-local, CGNAT, documentation and unknown peers', () => {
    const on = trust();
    for (const address of [
      '8.8.8.8',
      '1.1.1.1',
      '203.0.113.9',
      '192.0.2.21',
      '169.254.10.20',
      'fe80::1',
      'fe80::1%eth0',
      '100.64.0.1',
      '100.127.255.254',
      '2001:4860:4860::8888',
      '::',
      '0.0.0.0',
      '::192.168.1.1',
      'not-an-address',
      '',
      undefined,
    ]) {
      expect([address, isTrustedLocalNetworkPeer(request(address), on)]).toEqual([address, false]);
    }
  });

  it('normalises IPv4-mapped IPv6 peers in either spelling before matching', () => {
    const on = trust();
    expect(isTrustedLocalNetworkPeer(request('::ffff:192.168.50.20'), on)).toBe(true);
    expect(isTrustedLocalNetworkPeer(request('::FFFF:c0a8:3214'), on)).toBe(true);
    expect(isTrustedLocalNetworkPeer(request('[::ffff:10.6.0.2]'), on)).toBe(true);
    expect(isTrustedLocalNetworkPeer(request('::ffff:8.8.8.8'), on)).toBe(false);
    expect(isTrustedLocalNetworkPeer(request('::ffff:100.64.0.1'), on)).toBe(false);
    expect(normalizePeerAddress('::ffff:192.168.50.20')).toBe('192.168.50.20');
    expect(normalizePeerAddress('::ffff:c0a8:3214')).toBe('192.168.50.20');
    expect(normalizePeerAddress('FD12:0:0:0:0:0:0:42')).toBe('fd12::42');
    expect(normalizePeerAddress('fe80::1%eth0')).toBe('fe80::1');
    expect(normalizePeerAddress('nonsense')).toBeNull();
  });

  it('trusts a CGNAT or other VPN subnet only when the list names it', () => {
    const vpn = trust(['192.168.50.0/24', '100.64.0.0/10', '10.6.0.0/24']);
    expect(isTrustedLocalNetworkPeer(request('100.100.1.2'), vpn)).toBe(true);
    expect(isTrustedLocalNetworkPeer(request('10.6.0.9'), vpn)).toBe(true);
    expect(isTrustedLocalNetworkPeer(request('192.168.50.7'), vpn)).toBe(true);
    // The list replaces the defaults: other private ranges are no longer trusted.
    expect(isTrustedLocalNetworkPeer(request('10.7.0.9'), vpn)).toBe(false);
    expect(isTrustedLocalNetworkPeer(request('192.168.1.7'), vpn)).toBe(false);
    expect(isTrustedLocalNetworkPeer(request('127.0.0.1'), vpn)).toBe(false);
  });

  it('parses ranges strictly: host bits cleared, nothing wider than /8 or /48, no zones', () => {
    expect(parseTrustedNetwork('192.168.50.1/24')?.cidr).toBe('192.168.50.0/24');
    expect(parseTrustedNetwork('10.6.0.5')?.cidr).toBe('10.6.0.5/32');
    expect(parseTrustedNetwork('fd00::1/8')?.cidr).toBe('fd00::/8');
    expect(parseTrustedNetwork('::ffff:10.0.0.0/104')?.cidr).toBe('10.0.0.0/8');
    // IPv6: one site's /48 at most, except inside the unique-local block fc00::/7.
    expect(parseTrustedNetwork('2001:db8:1234::/48')?.cidr).toBe('2001:db8:1234::/48');
    expect(parseTrustedNetwork('2001:db8:1234:5600::/56')?.cidr).toBe('2001:db8:1234:5600::/56');
    expect(parseTrustedNetwork('fc00::/7')?.cidr).toBe('fc00::/7');
    expect(parseTrustedNetwork('fd12:3456::/32')?.cidr).toBe('fd12:3456::/32');
    for (const bad of [
      '0.0.0.0/0',
      '10.0.0.0/7',
      '::/0',
      'fc00::/6',
      '2000::/7',
      '2400::/7',
      '2001:db8::/32',
      '2001:db8:1200::/47',
      'fe80::/10',
      'fe00::/7',
      '::/47',
      '10.0.0.0/33',
      'fe80::/10%eth0',
      ' 10.0.0.0/8',
      '10.0.0.0/8 ',
      '10.0.0/8',
      '10.0.0.0/+8',
      '10.0.0.0/',
      '[fd00::]/8',
      'example.com/24',
      42,
      null,
    ]) {
      expect([bad, parseTrustedNetwork(bad)]).toEqual([bad, null]);
    }
  });

  it('refuses loopback-only ranges, since rule 4 never trusts loopback', () => {
    for (const loopback of [
      '127.0.0.0/8',
      '127.0.0.1',
      '127.1.0.0/16',
      '::1',
      '::1/128',
      '::ffff:127.0.0.0/104',
      '::ffff:127.0.0.1',
    ]) {
      expect([loopback, parseTrustedNetwork(loopback)]).toEqual([loopback, null]);
    }
    const listed = parseTrustedNetworks(['192.168.50.0/24', '127.0.0.0/8', '::1/128']);
    expect(listed.networks.map((network) => network.cidr)).toEqual(['192.168.50.0/24']);
    expect(listed.rejected).toBe(2);
  });

  it('uses the defaults when the list is absent, and trusts less, never more, when it is bad', () => {
    expect(parseTrustedNetworks(undefined).networks.map((network) => network.cidr)).toEqual([
      ...DEFAULT_TRUSTED_NETWORKS,
    ]);
    expect(parseTrustedNetworks(null).networks).toHaveLength(DEFAULT_TRUSTED_NETWORKS.length);
    const mixed = parseTrustedNetworks(['10.6.0.0/24', 'nonsense', '0.0.0.0/0', '10.6.0.0/24']);
    expect(mixed.networks.map((network) => network.cidr)).toEqual(['10.6.0.0/24']);
    expect(mixed.rejected).toBe(2);
    expect(parseTrustedNetworks('10.6.0.0/24').networks.map((network) => network.cidr)).toEqual([
      '10.6.0.0/24',
    ]);
    expect(parseTrustedNetworks({ cidr: '10.0.0.0/8' })).toEqual({ networks: [], rejected: 1 });
    expect(parseTrustedNetworks([]).networks).toEqual([]);
  });
});

describe('isSecureTransport rule 4', () => {
  it('is off by default: a private LAN peer over plain HTTP is not secure', () => {
    expect(localNetworkTrust().enabled).toBe(false);
    expect(isSecureTransport(request('192.168.50.20'))).toBe(false);
    expect(describeConnection(request('::ffff:192.168.50.20'))).toEqual({
      peer: '192.168.50.20',
      trusted: false,
    });
    expect(parseDashboardNetworkSettings(undefined)).toMatchObject({
      trustLocalNetwork: false,
      trustedNetworks: [...DEFAULT_TRUSTED_NETWORKS],
    });
    // Only the boolean true turns it on.
    for (const value of ['true', 1, 'yes', null]) {
      expect(parseDashboardNetworkSettings({ trust_local_network: value }).trustLocalNetwork).toBe(
        false
      );
    }
  });

  it('makes a private peer secure once the owner turns it on, and only a private one', () => {
    setLocalNetworkTrustResolver(() => trust());
    expect(isSecureTransport(request('192.168.50.20'))).toBe(true);
    expect(isSecureTransport(request('::ffff:10.6.0.2'))).toBe(true);
    expect(isSecureTransport(request('fd00::20'))).toBe(true);
    expect(isSecureTransport(request('203.0.113.9'))).toBe(false);
    expect(isSecureTransport(request('100.64.0.1'))).toBe(false);
    expect(isSecureTransport(request('169.254.1.1'))).toBe(false);
    expect(describeConnection(request('192.168.50.20'))).toEqual({
      peer: '192.168.50.20',
      trusted: true,
    });
    expect(describeConnection(request(undefined))).toEqual({ peer: 'unknown', trusted: false });
    setLocalNetworkTrustResolver(() => trust(undefined, false));
    expect(isSecureTransport(request('192.168.50.20'))).toBe(false);
  });

  it('decides by the socket address only: forwarded headers never name the peer', () => {
    setLocalNetworkTrustResolver(() => trust());
    // A public socket cannot claim a private address.
    expect(isSecureTransport(request('203.0.113.9', { 'x-forwarded-for': '192.168.50.20' }))).toBe(
      false
    );
    // A local proxy on loopback forwards someone unknown: rule 4 does not apply to it.
    expect(
      isSecureTransport(request('127.0.0.1', { 'x-forwarded-for': '203.0.113.9' }, 'localhost'))
    ).toBe(false);
  });

  it('never trusts a request a proxy forwarded, whatever address the proxy has', () => {
    setLocalNetworkTrustResolver(() => trust());
    // A reverse proxy on a NAS or router would pass its trust to everyone behind it.
    expect(isSecureTransport(request('192.168.50.2'))).toBe(true);
    for (const header of [
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
    ]) {
      const proxied = request('192.168.50.2', { [header]: '203.0.113.9' });
      expect([header, isSecureTransport(proxied), describeConnection(proxied).trusted]).toEqual([
        header,
        false,
        false,
      ]);
      const mapped = request('::ffff:10.6.0.2', { [header]: '10.6.0.3' });
      expect([header, isTrustedLocalNetworkPeer(mapped, trust())]).toEqual([header, false]);
    }
  });

  it('never applies to a loopback peer: rule 2 and its Host test decide loopback', () => {
    setLocalNetworkTrustResolver(() => trust());
    // A raw TCP forward onto 127.0.0.1 (ssh -R, socat, frp) adds no headers and keeps the
    // remote Host; a DNS-rebinding page names its own host. Neither is secure.
    for (const peer of ['127.0.0.1', '127.8.9.10', '::1', '::ffff:127.0.0.1', '::ffff:7f00:1']) {
      for (const host of ['192.168.50.10:3000', 'attacker.example', 'attacker.example:3000']) {
        expect([peer, host, isSecureTransport(request(peer, {}, host))]).toEqual([
          peer,
          host,
          false,
        ]);
      }
      expect([peer, describeConnection(request(peer, {}, 'localhost:3000')).trusted]).toEqual([
        peer,
        false,
      ]);
    }
    // Even a list that names loopback, built without the parser, changes nothing.
    const loopbackListed: LocalNetworkTrust = {
      enabled: true,
      networks: [
        { family: 4, bytes: Uint8Array.from([127, 0, 0, 0]), prefix: 8, cidr: '127.0.0.0/8' },
        {
          family: 6,
          bytes: Uint8Array.from([...Array<number>(15).fill(0), 1]),
          prefix: 128,
          cidr: '::1/128',
        },
      ],
    };
    expect(isTrustedLocalNetworkPeer(request('127.0.0.1'), loopbackListed)).toBe(false);
    expect(isTrustedLocalNetworkPeer(request('::1'), loopbackListed)).toBe(false);
    // Real loopback stays secure by rule 2, switch on or off.
    expect(isSecureTransport(request('127.0.0.1', {}, 'localhost:3000'))).toBe(true);
    expect(isSecureTransport(request('::1', {}, '[::1]:3000'))).toBe(true);
    setLocalNetworkTrustResolver(null);
    expect(isSecureTransport(request('127.0.0.1', {}, '127.0.0.1:3000'))).toBe(true);
  });

  it('fails closed when the resolver throws', () => {
    setLocalNetworkTrustResolver(() => {
      throw new Error('config unreadable');
    });
    expect(localNetworkTrust().enabled).toBe(false);
    expect(isSecureTransport(request('192.168.50.20'))).toBe(false);
  });
});
