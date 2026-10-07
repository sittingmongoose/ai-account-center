import type { Request } from 'express';
import { ipKeyGenerator } from 'express-rate-limit';
import * as net from 'net';
import { isForwardedThroughTrustedProxy } from './secure-transport';
import { formatAddress, isLoopbackAddress, parseAddress } from './trusted-networks';

/**
 * The client key every per-client limit uses (CONTRACT-auth-devices section
 * 10): the sign-in and pairing budget, the password-change and sign-out
 * limits, and the usage hub's limits.
 *
 * - A request whose socket peer is a trusted proxy hop and that names a
 *   forwarded client gets `proxy:<client>`. Anything on the proxy's computer
 *   can open the dashboard port directly and pick any `X-Forwarded-For`, so a
 *   forwarded address is never the same key as a direct one: a forwarded
 *   `127.0.0.1` or a forwarded LAN address spends only its own `proxy:` budget,
 *   never that of the browser on the dashboard computer or of a LAN computer.
 *   A forwarded value that is not an IP address shares one `proxy:invalid` key.
 * - Every other request is keyed by its socket peer: X-Forwarded-For from a
 *   peer that is not a trusted hop changes nothing.
 *
 * Addresses are normalised first, so `::ffff:192.0.2.5` and `192.0.2.5` are
 * one key and `::1` keeps its own key; other IPv6 addresses use
 * express-rate-limit's /56 subnet key.
 */
/** A forwarded client counts only as exactly an IP address (no brackets, zone or port). */
function strictAddress(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  const text = value.trim();
  return text.includes('%') || net.isIP(text) === 0 ? null : text;
}

export function rateLimitClientKey(req: Request): string {
  const forwarded = isForwardedThroughTrustedProxy(req);
  const address = parseAddress(forwarded ? strictAddress(req.ip) : req.socket?.remoteAddress);
  let key = forwarded ? 'invalid' : 'unknown';
  if (address) {
    const text = formatAddress(address);
    key = address.family === 4 || isLoopbackAddress(address) ? text : ipKeyGenerator(text);
  }
  return forwarded ? `proxy:${key}` : key;
}
