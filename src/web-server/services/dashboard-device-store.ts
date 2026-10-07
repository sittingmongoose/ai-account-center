import { createHash, randomBytes, timingSafeEqual } from 'crypto';
import { createLogger } from '../../services/logging';
import type { CredentialTransport } from '../middleware/secure-transport';
import {
  authFile,
  authFileStamp,
  authNow,
  isoTime,
  readAuthJsonSync,
  withAuthWriteGate,
  writeAuthJson,
} from './dashboard-auth-files';
import {
  isHash,
  parseDocument,
  DEVICE_IDLE_EXPIRY_MS,
  DEVICE_ROTATE_AFTER_MS,
  DEVICE_TOKEN_PATTERN,
  LAST_SEEN_INTERVAL_MS,
  MAX_ACTIVE_DEVICES,
  PREVIOUS_TOKEN_GRACE_MS,
  REVOKED_RECORD_RETENTION_MS,
  type ActiveDeviceRecord,
  type DevicePlatform,
  type DeviceRecord,
  type DeviceRevokeReason,
  type DevicesDocument,
  type RevokedDeviceRecord,
} from './dashboard-device-records';

export {
  DAY_MS,
  DEVICE_IDLE_EXPIRY_MS,
  DEVICE_TOKEN_PATTERN,
  MAX_ACTIVE_DEVICES,
  type ActiveDeviceRecord,
  type DevicePlatform,
  type DeviceRevokeReason,
  type RevokedDeviceRecord,
} from './dashboard-device-records';

/**
 * Paired tray devices (CONTRACT-auth-devices sections 5, 6 and 7), kept in
 * `~/.ccs/auth/devices.json` (0600, written atomically through the auth write
 * gate). Only `SHA-256(token)` is stored; a token is shown once, in the pair or
 * rotate response.
 *
 * The parsed file is cached in memory and is authoritative while the process
 * runs: a revoke, a rotation's first use or a last-seen stamp take effect for
 * the next request at once, and the file follows. A file that is not private
 * (any group or other bit), not a regular file or not valid is refused and
 * reported as unavailable, never as empty, and is never overwritten.
 *
 * Revoked records keep their id, time and reason, plus the token hashes they
 * held, for 30 days, so a later request is told `device_revoked` rather than
 * `invalid_token`. Hashes of revoked tokens grant nothing.
 */
const logger = createLogger('dashboard-auth');

export class DeviceStoreError extends Error {
  constructor(
    readonly code: 'store_unavailable' | 'too_many_devices' | 'write_failed' | 'unknown_device'
  ) {
    super(code);
    this.name = 'DeviceStoreError';
  }
}

interface CacheEntry {
  stamp: string | null;
  document: DevicesDocument | null;
  pendingWrites: number;
}

const caches = new Map<string, CacheEntry>();

export function hashDeviceToken(token: string): string {
  return createHash('sha256').update(token, 'utf8').digest('hex');
}

function newToken(): string {
  return `aacd_${randomBytes(32).toString('base64url')}`;
}

function newDeviceId(): string {
  return `dev_${randomBytes(8).toString('hex')}`;
}

/** The current document, or null when the file cannot be trusted. */
function loadDocument(): { file: string; entry: CacheEntry } {
  const file = authFile('devices.json');
  const cached = caches.get(file);
  if (cached && (cached.pendingWrites > 0 || cached.stamp === authFileStamp(file))) {
    return { file, entry: cached };
  }
  const read = readAuthJsonSync(file);
  const document =
    read.state === 'absent'
      ? ({ version: 1, devices: [] } as DevicesDocument)
      : read.state === 'ok'
        ? parseDocument(read.value)
        : null;
  const entry: CacheEntry = {
    stamp: read.state === 'ok' ? read.stamp : null,
    document,
    pendingWrites: 0,
  };
  caches.set(file, entry);
  return { file, entry };
}

function isActive(record: DeviceRecord): record is ActiveDeviceRecord {
  return record.revokedAt === null;
}

function idleBase(device: ActiveDeviceRecord): number {
  return Date.parse(device.lastSeenAt ?? device.pairedAt);
}

export function idleExpiresAt(device: ActiveDeviceRecord): number {
  return idleBase(device) + DEVICE_IDLE_EXPIRY_MS;
}

function isExpired(device: ActiveDeviceRecord, now: number): boolean {
  return !(now <= idleExpiresAt(device));
}

/** Expired devices become revoked records; revoked records past 30 days and stale previous tokens go. */
function compact(document: DevicesDocument, now: number): void {
  document.devices = document.devices
    .map((record): DeviceRecord => {
      if (!isActive(record)) return record;
      if (isExpired(record, now)) {
        return {
          id: record.id,
          revokedAt: isoTime(idleExpiresAt(record)),
          revokedReason: 'expired',
          tokenSha256: record.tokenSha256,
          prevTokenSha256: record.prevTokenSha256,
        };
      }
      if (record.prevTokenValidUntil && !(now < Date.parse(record.prevTokenValidUntil))) {
        record.prevTokenSha256 = null;
        record.prevTokenPlain = true;
        record.prevTokenValidUntil = null;
      }
      return record;
    })
    .filter(
      (record) =>
        isActive(record) || !(now - Date.parse(record.revokedAt) > REVOKED_RECORD_RETENTION_MS)
    );
}

/** Write the in-memory document (the latest one when the gate reaches it). */
function persist(file: string, entry: CacheEntry): Promise<void> {
  entry.pendingWrites += 1;
  return withAuthWriteGate(async () => {
    try {
      if (!entry.document) throw new DeviceStoreError('store_unavailable');
      compact(entry.document, authNow());
      await writeAuthJson(file, entry.document);
      entry.stamp = authFileStamp(file);
    } finally {
      entry.pendingWrites -= 1;
    }
  });
}

function persistInBackground(file: string, entry: CacheEntry): void {
  persist(file, entry).catch(() => {
    try {
      logger.warn('auth.device.store_write_failed', 'Paired devices could not be saved');
    } catch {
      /* Logging is best effort. */
    }
  });
}

function writable(): { file: string; entry: CacheEntry; document: DevicesDocument } {
  const { file, entry } = loadDocument();
  if (!entry.document) throw new DeviceStoreError('store_unavailable');
  return { file, entry, document: entry.document };
}

export function deviceStoreAvailable(): boolean {
  return loadDocument().entry.document !== null;
}

export function activeDevices(now = authNow()): ActiveDeviceRecord[] {
  const document = loadDocument().entry.document;
  if (!document) return [];
  return document.devices.filter(
    (record): record is ActiveDeviceRecord => isActive(record) && !isExpired(record, now)
  );
}

export interface PairInput {
  name: string;
  platform: DevicePlatform;
  installId: string | null;
  appVersion: string | null;
  address: string | null;
  /** How the pairing request (and so the new token) reached the dashboard. */
  transport: CredentialTransport;
}

export interface PairResult {
  device: ActiveDeviceRecord;
  token: string;
  replacedDeviceId: string | null;
}

/** Add a device (replacing the record of the same installId); the token is returned once. */
export async function pairDevice(input: PairInput): Promise<PairResult> {
  const { file, entry, document } = writable();
  const now = authNow();
  const replaced = input.installId
    ? activeDevices(now).find((device) => device.installId === input.installId)
    : undefined;
  if (activeDevices(now).filter((device) => device !== replaced).length >= MAX_ACTIVE_DEVICES) {
    throw new DeviceStoreError('too_many_devices');
  }
  const token = newToken();
  const device: ActiveDeviceRecord = {
    id: newDeviceId(),
    name: input.name,
    platform: input.platform,
    installId: input.installId,
    appVersion: input.appVersion,
    tokenSha256: hashDeviceToken(token),
    tokenPlain: input.transport === 'plain',
    prevTokenSha256: null,
    prevTokenPlain: true,
    prevTokenValidUntil: null,
    pairedAt: isoTime(now),
    rotatedAt: null,
    lastSeenAt: null,
    lastSeenAddress: input.address,
    revokedAt: null,
    revokedReason: null,
  };
  document.devices = [
    ...document.devices.map((record) =>
      record === replaced ? revoked(record, 'replaced') : record
    ),
    device,
  ];
  try {
    await persist(file, entry);
  } catch {
    document.devices = document.devices
      .filter((record) => record.id !== device.id)
      .map((record) => (replaced && record.id === replaced.id ? replaced : record));
    throw new DeviceStoreError('write_failed');
  }
  return { device, token, replacedDeviceId: replaced?.id ?? null };
}

function revoked(record: ActiveDeviceRecord, reason: DeviceRevokeReason): RevokedDeviceRecord {
  return {
    id: record.id,
    revokedAt: isoTime(authNow()),
    revokedReason: reason,
    tokenSha256: record.tokenSha256,
    prevTokenSha256: record.prevTokenSha256,
  };
}

export type DeviceAuthResult =
  | { ok: true; device: ActiveDeviceRecord; viaPreviousToken: boolean }
  | {
      ok: false;
      code:
        | 'device_revoked'
        | 'device_expired'
        | 'invalid_token'
        | 'store_unavailable'
        | 'plain_http_token';
      deviceId: string | null;
    };

export interface DeviceAuthContext {
  /** The client address, stamped as "last seen". */
  address: string | null;
  /** How this request reached the dashboard; `plain` marks the token as plain from now on. */
  transport?: CredentialTransport;
  /** True through the LAN HTTPS proxy: a token that ever crossed the network in plain text is refused. */
  requireNeverPlain?: boolean;
}

function sameHash(left: string | null, right: Buffer): boolean {
  if (!left) return false;
  const candidate = Buffer.from(left, 'hex');
  return candidate.length === right.length && timingSafeEqual(candidate, right);
}

/**
 * Check a bearer token. A valid token stamps `lastSeenAt` at most once a
 * minute; the first use of a rotated token ends the previous one. A token
 * presented over plain HTTP is marked plain for good, and with
 * `requireNeverPlain` (the LAN HTTPS proxy) a plain token is refused
 * (`plain_http_token`) before it is stamped as seen or ends a previous token.
 */
export function authenticateDeviceToken(
  token: string,
  { address, transport, requireNeverPlain }: DeviceAuthContext
): DeviceAuthResult {
  if (!DEVICE_TOKEN_PATTERN.test(token)) {
    return { ok: false, code: 'invalid_token', deviceId: null };
  }
  const { file, entry } = loadDocument();
  const document = entry.document;
  if (!document) return { ok: false, code: 'store_unavailable', deviceId: null };
  const now = authNow();
  const presented = Buffer.from(hashDeviceToken(token), 'hex');
  let match: { record: DeviceRecord; previous: boolean } | null = null;
  for (const record of document.devices) {
    if (sameHash(record.tokenSha256, presented)) match = { record, previous: false };
    else if (sameHash(record.prevTokenSha256, presented)) match = { record, previous: true };
  }
  if (!match) return { ok: false, code: 'invalid_token', deviceId: null };
  const { record, previous } = match;
  if (!isActive(record)) {
    return {
      ok: false,
      code: record.revokedReason === 'expired' ? 'device_expired' : 'device_revoked',
      deviceId: record.id,
    };
  }
  if (isExpired(record, now)) return { ok: false, code: 'device_expired', deviceId: record.id };
  if (previous && !(record.prevTokenValidUntil && now < Date.parse(record.prevTokenValidUntil))) {
    return { ok: false, code: 'invalid_token', deviceId: record.id };
  }
  let changed = false;
  const plainNow = transport === 'plain';
  const wasPlain = previous ? record.prevTokenPlain : record.tokenPlain;
  if (plainNow && !wasPlain) {
    if (previous) record.prevTokenPlain = true;
    else record.tokenPlain = true;
    changed = true;
  }
  if (requireNeverPlain === true && (wasPlain || plainNow)) {
    if (changed) persistInBackground(file, entry);
    return { ok: false, code: 'plain_http_token', deviceId: record.id };
  }
  if (!previous && record.prevTokenSha256) {
    record.prevTokenSha256 = null;
    record.prevTokenPlain = true;
    record.prevTokenValidUntil = null;
    changed = true;
  }
  const lastSeen = record.lastSeenAt ? Date.parse(record.lastSeenAt) : null;
  if (lastSeen === null || now - lastSeen >= LAST_SEEN_INTERVAL_MS) {
    record.lastSeenAt = isoTime(now);
    record.lastSeenAddress = address;
    changed = true;
  }
  if (changed) persistInBackground(file, entry);
  return { ok: true, device: record, viaPreviousToken: previous };
}

export function findActiveDevice(id: string): ActiveDeviceRecord | null {
  return activeDevices().find((device) => device.id === id) ?? null;
}

/** Revoke one device; false when no active device has that id. */
export async function revokeDevice(id: string, reason: DeviceRevokeReason): Promise<boolean> {
  const { file, entry, document } = writable();
  const target = activeDevices().find((device) => device.id === id);
  if (!target) return false;
  document.devices = document.devices.map((record) =>
    record === target ? revoked(target, reason) : record
  );
  await persist(file, entry);
  return true;
}

/** Revoke every active device; returns how many. */
export async function revokeAllDevices(): Promise<number> {
  const { file, entry, document } = writable();
  let count = 0;
  document.devices = document.devices.map((record) => {
    if (!isActive(record)) return record;
    count += 1;
    return revoked(record, 'revoke-all');
  });
  await persist(file, entry);
  return count;
}

/**
 * Issue a new token for a device. The token presented with this request (its
 * SHA-256) stays valid until the new one is first used or for 24 hours,
 * whichever comes first.
 */
export async function rotateDeviceToken(
  id: string,
  presentedTokenSha256: string,
  transport: CredentialTransport = 'plain'
): Promise<{ token: string; device: ActiveDeviceRecord }> {
  const { file, entry } = writable();
  const device = activeDevices().find((record) => record.id === id);
  if (!device || !isHash(presentedTokenSha256)) throw new DeviceStoreError('unknown_device');
  const now = authNow();
  const token = newToken();
  const before = { ...device };
  // The presented token keeps what is known about it; it was just sent over `transport`.
  const presentedPlain =
    transport === 'plain' ||
    (presentedTokenSha256 === device.tokenSha256
      ? device.tokenPlain
      : presentedTokenSha256 === device.prevTokenSha256
        ? device.prevTokenPlain
        : true);
  device.prevTokenSha256 = presentedTokenSha256;
  device.prevTokenPlain = presentedPlain;
  device.prevTokenValidUntil = isoTime(now + PREVIOUS_TOKEN_GRACE_MS);
  device.tokenSha256 = hashDeviceToken(token);
  device.tokenPlain = transport === 'plain';
  device.rotatedAt = isoTime(now);
  try {
    await persist(file, entry);
  } catch {
    Object.assign(device, before);
    throw new DeviceStoreError('write_failed');
  }
  return { token, device };
}

export function rotateAfter(device: ActiveDeviceRecord): number {
  return Date.parse(device.rotatedAt ?? device.pairedAt) + DEVICE_ROTATE_AFTER_MS;
}

/** Tests only. */
export function resetDeviceStoreForTests(): void {
  caches.clear();
}
