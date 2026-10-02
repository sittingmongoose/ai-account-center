/**
 * isSecureTransport (CONTRACT-auth-devices section 2a) and authKind.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import type { IncomingMessage } from 'http';
import type { Request } from 'express';
import {
  isSecureTransport,
  setTrustedProxyResolver,
} from '../../../src/web-server/middleware/secure-transport';
import { authKind } from '../../../src/web-server/middleware/request-auth';

function request(
  remoteAddress: string,
  headers: Record<string, string>,
  encrypted = false
): IncomingMessage {
  return { socket: { remoteAddress, encrypted }, headers } as unknown as IncomingMessage;
}

afterEach(() => setTrustedProxyResolver(() => null));

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
