/**
 * The records of `~/.ccs/auth/devices.json` (CONTRACT-auth-devices section 5)
 * and their strict parser: one malformed record makes the whole file invalid,
 * so it is refused rather than half-read.
 */
export type DevicePlatform = 'mac' | 'windows';
export type DeviceRevokeReason = 'dashboard' | 'self' | 'revoke-all' | 'replaced' | 'expired';

export interface ActiveDeviceRecord {
  id: string;
  name: string;
  platform: DevicePlatform;
  installId: string | null;
  appVersion: string | null;
  tokenSha256: string;
  /**
   * The current token crossed the network in plain text: it was handed out,
   * or presented, over plain HTTP (the trusted local network included). The
   * LAN HTTPS proxy refuses such a token. A record from before this field
   * counts as plain.
   */
  tokenPlain: boolean;
  prevTokenSha256: string | null;
  /** The same for the previous token of a rotation. */
  prevTokenPlain: boolean;
  prevTokenValidUntil: string | null;
  pairedAt: string;
  rotatedAt: string | null;
  lastSeenAt: string | null;
  lastSeenAddress: string | null;
  revokedAt: null;
  revokedReason: null;
}

export interface RevokedDeviceRecord {
  id: string;
  revokedAt: string;
  revokedReason: DeviceRevokeReason;
  tokenSha256: string | null;
  prevTokenSha256: string | null;
}

export type DeviceRecord = ActiveDeviceRecord | RevokedDeviceRecord;

export interface DevicesDocument {
  version: 1;
  devices: DeviceRecord[];
}

export const MAX_ACTIVE_DEVICES = 20;
export const DAY_MS = 24 * 60 * 60 * 1000;
export const DEVICE_IDLE_EXPIRY_MS = 90 * DAY_MS;
export const DEVICE_ROTATE_AFTER_MS = 30 * DAY_MS;
export const PREVIOUS_TOKEN_GRACE_MS = DAY_MS;
export const REVOKED_RECORD_RETENTION_MS = 30 * DAY_MS;
export const LAST_SEEN_INTERVAL_MS = 60 * 1000;
export const DEVICE_TOKEN_PATTERN = /^aacd_[A-Za-z0-9_-]{43}$/;

export function isHash(value: unknown): value is string {
  return typeof value === 'string' && /^[0-9a-f]{64}$/.test(value);
}

function optionalString(value: unknown): value is string | null {
  return value === null || typeof value === 'string';
}

function parseRecord(value: unknown): DeviceRecord | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (typeof record.id !== 'string' || !/^dev_[0-9a-f]{16}$/.test(record.id)) return null;
  if (typeof record.revokedAt === 'string') {
    return {
      id: record.id,
      revokedAt: record.revokedAt,
      revokedReason: (typeof record.revokedReason === 'string'
        ? record.revokedReason
        : 'dashboard') as DeviceRevokeReason,
      tokenSha256: isHash(record.tokenSha256) ? record.tokenSha256 : null,
      prevTokenSha256: isHash(record.prevTokenSha256) ? record.prevTokenSha256 : null,
    };
  }
  if (
    typeof record.name !== 'string' ||
    (record.platform !== 'mac' && record.platform !== 'windows') ||
    !isHash(record.tokenSha256) ||
    typeof record.pairedAt !== 'string' ||
    !optionalString(record.installId ?? null) ||
    !optionalString(record.appVersion ?? null) ||
    !optionalString(record.rotatedAt ?? null) ||
    !optionalString(record.lastSeenAt ?? null) ||
    !optionalString(record.lastSeenAddress ?? null) ||
    !optionalString(record.prevTokenValidUntil ?? null)
  )
    return null;
  return {
    id: record.id,
    name: record.name,
    platform: record.platform,
    installId: (record.installId as string | null | undefined) ?? null,
    appVersion: (record.appVersion as string | null | undefined) ?? null,
    tokenSha256: record.tokenSha256,
    // Only an explicit false is trusted: an older record counts as plain.
    tokenPlain: record.tokenPlain !== false,
    prevTokenSha256: isHash(record.prevTokenSha256) ? record.prevTokenSha256 : null,
    prevTokenPlain: record.prevTokenPlain !== false,
    prevTokenValidUntil: (record.prevTokenValidUntil as string | null | undefined) ?? null,
    pairedAt: record.pairedAt,
    rotatedAt: (record.rotatedAt as string | null | undefined) ?? null,
    lastSeenAt: (record.lastSeenAt as string | null | undefined) ?? null,
    lastSeenAddress: (record.lastSeenAddress as string | null | undefined) ?? null,
    revokedAt: null,
    revokedReason: null,
  };
}

export function parseDocument(value: unknown): DevicesDocument | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (record.version !== 1 || !Array.isArray(record.devices)) return null;
  const devices: DeviceRecord[] = [];
  for (const entry of record.devices) {
    const parsed = parseRecord(entry);
    if (!parsed) return null;
    devices.push(parsed);
  }
  return { version: 1, devices };
}
