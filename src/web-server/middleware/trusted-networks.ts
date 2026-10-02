import * as net from 'net';

/**
 * Address ranges for the trusted local network (CONTRACT-auth-devices section 2a,
 * rule 4 of isSecureTransport). Addresses are compared as bytes after
 * normalisation: brackets and an IPv6 zone are dropped, and an IPv4-mapped IPv6
 * address (`::ffff:192.168.1.5`, either spelling) becomes its IPv4 address, so a
 * dual-stack listener cannot make a LAN peer look like a different family.
 */
export type AddressFamily = 4 | 6;

export interface ParsedAddress {
  family: AddressFamily;
  /** 4 bytes for IPv4, 16 for IPv6. */
  bytes: Uint8Array;
}

export interface TrustedNetwork {
  family: AddressFamily;
  /** The network address, host bits cleared. */
  bytes: Uint8Array;
  prefix: number;
  /** Canonical CIDR text, for display. */
  cidr: string;
}

/**
 * The private ranges: what `dashboard_network.trusted_networks` means when
 * absent. Loopback is not one of them: rule 4 never applies to a loopback peer
 * (rule 2 covers real loopback, with its Host test), so a raw TCP forward onto
 * 127.0.0.1 or a DNS-rebinding page cannot borrow it.
 */
export const DEFAULT_TRUSTED_NETWORKS: readonly string[] = Object.freeze([
  '10.0.0.0/8',
  '172.16.0.0/12',
  '192.168.0.0/16',
  'fc00::/7',
]);

/**
 * The broadest range a list may name: nothing wider than a /8 (IPv4) or a /48
 * (IPv6, one site's prefix). The IPv6 unique-local block fc00::/7 is the only
 * wider IPv6 range allowed, and only inside it (fc00::/7, fd00::/8 and so on).
 */
const MIN_PREFIX: Record<AddressFamily, number> = { 4: 8, 6: 48 };
const IPV6_UNIQUE_LOCAL_PREFIX = 7;
const MAX_ENTRIES = 64;

function isInsideUniqueLocal(bytes: Uint8Array, prefix: number): boolean {
  return prefix >= IPV6_UNIQUE_LOCAL_PREFIX && (bytes[0] & 0xfe) === 0xfc;
}

/** A range that holds only loopback addresses (inside 127.0.0.0/8, or ::1/128). */
function isLoopbackOnlyRange(family: AddressFamily, bytes: Uint8Array, prefix: number): boolean {
  if (family === 4) return prefix >= 8 && bytes[0] === 127;
  return prefix === 128 && isLoopbackAddress({ family, bytes });
}

function parseIPv4(text: string): Uint8Array | null {
  if (net.isIPv4(text) === false) return null;
  const parts = text.split('.').map((part) => Number(part));
  return Uint8Array.from(parts);
}

function parseIPv6(text: string): Uint8Array | null {
  if (net.isIPv6(text) === false) return null;
  let head = text;
  let tailV4: Uint8Array | null = null;
  const lastColon = text.lastIndexOf(':');
  if (text.slice(lastColon + 1).includes('.')) {
    tailV4 = parseIPv4(text.slice(lastColon + 1));
    if (!tailV4) return null;
    head = `${text.slice(0, lastColon + 1)}0:0`;
  }
  const halves = head.split('::');
  if (halves.length > 2) return null;
  const groups = (part: string): string[] => (part === '' ? [] : part.split(':'));
  const left = groups(halves[0]);
  const right = halves.length === 2 ? groups(halves[1]) : [];
  const missing = 8 - left.length - right.length;
  if (missing < 0 || (halves.length === 1 && missing !== 0)) return null;
  const all = [...left, ...Array<string>(missing).fill('0'), ...right];
  const bytes = new Uint8Array(16);
  all.forEach((group, index) => {
    const value = parseInt(group, 16);
    bytes[index * 2] = value >> 8;
    bytes[index * 2 + 1] = value & 0xff;
  });
  if (tailV4) bytes.set(tailV4, 12);
  return bytes;
}

/** `::ffff:a.b.c.d` (and its hex spelling) as IPv4; anything else unchanged. */
function unmapped(address: ParsedAddress): ParsedAddress {
  if (address.family !== 6) return address;
  const { bytes } = address;
  for (let index = 0; index < 10; index += 1) if (bytes[index] !== 0) return address;
  if (bytes[10] !== 0xff || bytes[11] !== 0xff) return address;
  return { family: 4, bytes: bytes.slice(12) };
}

/** Parse an address as a socket reports it; null when it is not an IP address. */
export function parseAddress(value: string | undefined | null): ParsedAddress | null {
  if (typeof value !== 'string') return null;
  let text = value.trim();
  if (text.startsWith('[') && text.endsWith(']')) text = text.slice(1, -1);
  const zone = text.indexOf('%');
  if (zone !== -1) text = text.slice(0, zone);
  const v4 = parseIPv4(text);
  if (v4) return { family: 4, bytes: v4 };
  const v6 = parseIPv6(text.toLowerCase());
  return v6 ? unmapped({ family: 6, bytes: v6 }) : null;
}

function formatIPv6(bytes: Uint8Array): string {
  const groups: number[] = [];
  for (let index = 0; index < 16; index += 2) groups.push((bytes[index] << 8) | bytes[index + 1]);
  // RFC 5952: compress the longest run of two or more zero groups (the first one on a tie).
  let bestStart = -1;
  let bestLength = 0;
  for (let start = 0; start < 8; ) {
    if (groups[start] !== 0) {
      start += 1;
      continue;
    }
    let end = start;
    while (end < 8 && groups[end] === 0) end += 1;
    if (end - start > bestLength && end - start >= 2) {
      bestStart = start;
      bestLength = end - start;
    }
    start = end;
  }
  const text = groups.map((group) => group.toString(16));
  if (bestStart === -1) return text.join(':');
  const left = text.slice(0, bestStart).join(':');
  const right = text.slice(bestStart + bestLength).join(':');
  return `${left}::${right}`;
}

export function formatAddress(address: ParsedAddress): string {
  return address.family === 4 ? Array.from(address.bytes).join('.') : formatIPv6(address.bytes);
}

/** The peer as the dashboard shows it: normalised, or null when it is not an IP address. */
export function normalizePeerAddress(value: string | undefined | null): string | null {
  const parsed = parseAddress(value);
  return parsed ? formatAddress(parsed) : null;
}

function masked(bytes: Uint8Array, prefix: number): Uint8Array {
  const result = new Uint8Array(bytes.length);
  for (let index = 0; index < bytes.length; index += 1) {
    const bits = Math.max(0, Math.min(8, prefix - index * 8));
    result[index] = bits === 0 ? 0 : bytes[index] & (0xff << (8 - bits)) & 0xff;
  }
  return result;
}

/**
 * One CIDR (`10.6.0.0/24`, `fd00::/8`) or a single address. Host bits are
 * cleared; an IPv4-mapped IPv6 range of /96 or longer becomes the IPv4 range.
 * Returns null for anything else, including a zone, a range wider than /8
 * (IPv4) or /48 (IPv6, outside fc00::/7), a loopback-only range (rule 4 never
 * trusts loopback), and surrounding text.
 */
export function parseTrustedNetwork(value: unknown): TrustedNetwork | null {
  if (typeof value !== 'string' || value.length === 0 || value.length > 64) return null;
  if (value.trim() !== value || value.includes('%') || value.includes('[')) return null;
  const slash = value.indexOf('/');
  const addressText = slash === -1 ? value : value.slice(0, slash);
  const prefixText = slash === -1 ? null : value.slice(slash + 1);
  const v4 = parseIPv4(addressText);
  let family: AddressFamily;
  let bytes: Uint8Array;
  if (v4) {
    family = 4;
    bytes = v4;
  } else {
    const v6 = parseIPv6(addressText.toLowerCase());
    if (!v6) return null;
    family = 6;
    bytes = v6;
  }
  const width = family === 4 ? 32 : 128;
  let prefix = width;
  if (prefixText !== null) {
    if (!/^\d{1,3}$/.test(prefixText)) return null;
    prefix = Number(prefixText);
    if (prefix > width) return null;
  }
  if (family === 6) {
    const mapped = unmapped({ family, bytes });
    if (mapped.family === 4 && prefix >= 96) {
      family = 4;
      bytes = mapped.bytes;
      prefix -= 96;
    }
  }
  const network = masked(bytes, prefix);
  if (prefix < MIN_PREFIX[family] && !(family === 6 && isInsideUniqueLocal(network, prefix))) {
    return null;
  }
  if (isLoopbackOnlyRange(family, network, prefix)) return null;
  return {
    family,
    bytes: network,
    prefix,
    cidr: `${formatAddress({ family, bytes: network })}/${prefix}`,
  };
}

export interface TrustedNetworkList {
  networks: TrustedNetwork[];
  /** How many entries were not valid ranges and were left out (never trusted). */
  rejected: number;
}

/**
 * `dashboard_network.trusted_networks`: absent or null means the defaults; a
 * list (or one string) is used as written, and an entry that is not a valid
 * range is left out, so a typo trusts less, never more. Anything else trusts
 * nothing.
 */
export function parseTrustedNetworks(raw: unknown): TrustedNetworkList {
  const entries: unknown[] =
    raw === undefined || raw === null
      ? [...DEFAULT_TRUSTED_NETWORKS]
      : typeof raw === 'string'
        ? [raw]
        : Array.isArray(raw)
          ? raw.slice(0, MAX_ENTRIES)
          : [];
  const networks: TrustedNetwork[] = [];
  const seen = new Set<string>();
  let rejected = Array.isArray(raw) ? Math.max(0, raw.length - MAX_ENTRIES) : 0;
  if (raw !== undefined && raw !== null && typeof raw !== 'string' && !Array.isArray(raw)) {
    rejected += 1;
  }
  for (const entry of entries) {
    const network = parseTrustedNetwork(entry);
    if (!network) {
      rejected += 1;
      continue;
    }
    if (seen.has(network.cidr)) continue;
    seen.add(network.cidr);
    networks.push(network);
  }
  return { networks, rejected };
}

/** 127.0.0.0/8 or ::1, after normalisation. */
export function isLoopbackAddress(address: ParsedAddress | null): boolean {
  if (!address) return false;
  if (address.family === 4) return address.bytes[0] === 127;
  return address.bytes.every((byte, index) => byte === (index === 15 ? 1 : 0));
}

export function networkContains(network: TrustedNetwork, address: ParsedAddress): boolean {
  if (network.family !== address.family) return false;
  const candidate = masked(address.bytes, network.prefix);
  for (let index = 0; index < candidate.length; index += 1) {
    if (candidate[index] !== network.bytes[index]) return false;
  }
  return true;
}

export function isAddressInNetworks(
  address: ParsedAddress | null,
  networks: readonly TrustedNetwork[]
): boolean {
  return address !== null && networks.some((network) => networkContains(network, address));
}
