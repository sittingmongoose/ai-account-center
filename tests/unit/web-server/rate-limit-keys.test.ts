/**
 * The per-client limiter key (CONTRACT-auth-devices section 10): the socket
 * peer, or `proxy:<client>` behind a trusted proxy hop, normalised.
 * Placeholder addresses only.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import type { Request } from 'express';
import { rateLimitClientKey } from '../../../src/web-server/middleware/rate-limit-keys';
import {
  setTrustedProxyAddressesResolver,
  setTrustedProxyResolver,
} from '../../../src/web-server/middleware/secure-transport';

const PROXY = '192.168.1.20';

function request(peer: string, headers: Record<string, string> = {}, ip = peer): Request {
  return { socket: { remoteAddress: peer }, headers, ip } as unknown as Request;
}

afterEach(() => {
  setTrustedProxyResolver(() => null);
  setTrustedProxyAddressesResolver(null);
});

describe('rateLimitClientKey', () => {
  it('keys a direct request by its normalised socket peer, whatever it forwards', () => {
    expect(rateLimitClientKey(request('127.0.0.1'))).toBe('127.0.0.1');
    expect(rateLimitClientKey(request('::ffff:127.0.0.1'))).toBe('127.0.0.1');
    expect(rateLimitClientKey(request('::1'))).toBe('::1');
    expect(rateLimitClientKey(request('192.168.1.21'))).toBe('192.168.1.21');
    expect(rateLimitClientKey(request('::ffff:192.168.1.21'))).toBe('192.168.1.21');
    expect(rateLimitClientKey(request('2001:db8:aa:bb::7'))).toBe('2001:db8:aa::/56');
    expect(
      rateLimitClientKey(request('192.168.1.21', { 'x-forwarded-for': '127.0.0.1' }, '127.0.0.1'))
    ).toBe('192.168.1.21');
    expect(rateLimitClientKey(request('not-an-address'))).toBe('unknown');
  });

  it('keys a request through a trusted hop as proxy:<client>, apart from every direct key', () => {
    setTrustedProxyResolver(() => 'lan-https-proxy');
    setTrustedProxyAddressesResolver(() => [PROXY]);
    const via = (client: string) =>
      rateLimitClientKey(request(PROXY, { 'x-forwarded-for': client }, client));
    expect(via('203.0.113.7')).toBe('proxy:203.0.113.7');
    expect(via('127.0.0.1')).toBe('proxy:127.0.0.1');
    expect(via('::ffff:127.0.0.1')).toBe('proxy:127.0.0.1');
    expect(via('::1')).toBe('proxy:::1');
    expect(via('2001:db8:aa:bb::7')).toBe('proxy:2001:db8:aa::/56');
    for (const garbage of ['<script>', 'unknown', '', '[::1]', 'fe80::1%eth0', '203.0.113.7:80']) {
      expect([garbage, via(garbage)]).toEqual([garbage, 'proxy:invalid']);
    }
    // The proxy without forwarded headers is keyed by its own address.
    expect(rateLimitClientKey(request(PROXY))).toBe(PROXY);
    // A loopback peer is not a hop for the LAN kind.
    expect(
      rateLimitClientKey(request('127.0.0.1', { 'x-forwarded-for': '203.0.113.7' }, '203.0.113.7'))
    ).toBe('127.0.0.1');
  });

  it('keys a local proxy kind the same way', () => {
    setTrustedProxyResolver(() => 'tailscale-serve');
    expect(
      rateLimitClientKey(request('127.0.0.1', { 'x-forwarded-for': '127.0.0.1' }, '127.0.0.1'))
    ).toBe('proxy:127.0.0.1');
    expect(rateLimitClientKey(request('127.0.0.1'))).toBe('127.0.0.1');
  });
});
