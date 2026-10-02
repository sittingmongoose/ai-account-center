import * as fs from 'fs';
import * as path from 'path';
import { createHash, randomBytes } from 'crypto';
import {
  HOLDER_RECORD_BYTES,
  holderStaleReason,
  ownHolderRecord,
  parseHolderRecord,
  type HolderRecord,
} from './holder-record';
import type { AntigravityHostId, NativeCredential, ProfileDto, VerifiedIdentity } from './types';

const MAX_NATIVE_BYTES = 1024 * 1024;
const MAX_REGISTRY_BYTES = 1024 * 1024;
const PROFILE_ID = /^[a-z][a-z0-9_-]{0,47}$/;
const HASH = /^[a-f0-9]{64}$/;
const FORMAT = /^[a-z][a-z0-9_.-]{1,63}$/;
const LOCK_NAME = '.transaction-lock';
const LOCK_HOLDER_FILE = 'holder.json';
/** A live holder refreshes its record's mtime this often while it holds the lock. */
const LOCK_HEARTBEAT_MS = 60_000;
/**
 * A live holder silent this long is taken over: its event loop has been
 * blocked for ten heartbeats. The switch flow bounds nothing longer (broker
 * approval TTL 60s, runtime-proof budget 10s, identity max age 5min).
 */
const STALE_LOCK_TIMEOUT_MS = 10 * 60_000;
/** A takeover claim is held for a few synchronous file calls; one this old is abandoned. */
const STALE_CLAIM_MS = 30_000;
const CREDENTIAL_FILE = /^credential-([a-f0-9]{64})\.bin$/;

interface CredentialRecord {
  fingerprint: string;
  format: string;
  identity: VerifiedIdentity;
  savedAt: string;
}

interface ProfileRecord {
  id: string;
  email: string;
  identityKey: string;
  plan: string | null;
  credential: CredentialRecord;
}

interface ActiveRecord {
  profileId: string;
  verifiedAt: string;
}

interface TransactionRecord {
  id: string;
  state: 'pending' | 'recovery-required';
  targetId: string;
  previousId: string;
  startedAt: string;
}

interface RegistryState {
  version: 1;
  revision: number;
  profiles: ProfileRecord[];
  active: ActiveRecord | null;
  transaction: TransactionRecord | null;
}

interface DirectoryIdentity {
  dev: number;
  ino: number;
}

export class PrivateStorageError extends Error {
  constructor(
    public readonly code: 'busy' | 'unsafe' | 'corrupt' | 'recovery-required' | 'missing'
  ) {
    super(`Antigravity private storage: ${code}.`);
    this.name = 'PrivateStorageError';
  }
}

export function credentialFingerprint(credential: NativeCredential): string {
  return createHash('sha256').update(credential.bytes).digest('hex');
}

export function canonicalEmail(email: string): string {
  if (
    typeof email !== 'string' ||
    email.length > 254 ||
    !/^[^\s@\x00-\x1f]+@[^\s@\x00-\x1f]+\.[^\s@\x00-\x1f]+$/.test(email)
  ) {
    throw new PrivateStorageError('corrupt');
  }
  return email.toLowerCase();
}

export function identityKey(identity: VerifiedIdentity): string {
  return createHash('sha256')
    .update(JSON.stringify(['antigravity', canonicalEmail(identity.email), identity.subject]))
    .digest('hex');
}

export function checkedIdentity(identity: VerifiedIdentity): VerifiedIdentity {
  const email = canonicalEmail(identity.email);
  if (
    typeof identity.subject !== 'string' ||
    identity.subject.length === 0 ||
    identity.subject.length > 256 ||
    /[\x00-\x1f]/.test(identity.subject) ||
    (identity.plan !== null &&
      (typeof identity.plan !== 'string' ||
        identity.plan.length > 128 ||
        /[\x00-\x1f]/.test(identity.plan))) ||
    !Number.isFinite(Date.parse(identity.verifiedAt)) ||
    !['provider-userinfo', 'native-runtime'].includes(identity.source)
  ) {
    throw new PrivateStorageError('corrupt');
  }
  return {
    email,
    subject: identity.subject,
    plan: identity.plan,
    verifiedAt: new Date(identity.verifiedAt).toISOString(),
    source: identity.source,
  };
}

export function assertUbuntu(hostId: AntigravityHostId): void {
  if (hostId !== 'ubuntu') throw new PrivateStorageError('unsafe');
}

function assertProfileId(profileId: string): void {
  if (!PROFILE_ID.test(profileId)) throw new PrivateStorageError('unsafe');
}

function sameDirectory(expected: DirectoryIdentity, actual: fs.Stats): boolean {
  return (
    actual.isDirectory() &&
    !actual.isSymbolicLink() &&
    actual.dev === expected.dev &&
    actual.ino === expected.ino
  );
}

function checkAncestors(target: string): void {
  let current = path.resolve(target);
  for (;;) {
    const stat = fs.lstatSync(current);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new PrivateStorageError('unsafe');
    const parent = path.dirname(current);
    if (parent === current) return;
    current = parent;
  }
}

function privateDirectory(target: string): DirectoryIdentity {
  const parent = path.dirname(target);
  checkAncestors(parent);
  try {
    fs.mkdirSync(target, { mode: 0o700 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
  }
  checkAncestors(target);
  const stat = fs.lstatSync(target);
  if (
    process.platform !== 'win32' &&
    ((stat.mode & 0o077) !== 0 || (process.getuid && stat.uid !== process.getuid()))
  ) {
    throw new PrivateStorageError('unsafe');
  }
  return { dev: stat.dev, ino: stat.ino };
}

function assertDirectory(target: string, expected: DirectoryIdentity): void {
  checkAncestors(target);
  if (!sameDirectory(expected, fs.lstatSync(target))) throw new PrivateStorageError('unsafe');
}

function ownedPrivateDirectory(target: string): DirectoryIdentity {
  checkAncestors(target);
  const stat = fs.lstatSync(target);
  if (
    !stat.isDirectory() ||
    stat.isSymbolicLink() ||
    (process.platform !== 'win32' &&
      ((stat.mode & 0o077) !== 0 || (process.getuid && stat.uid !== process.getuid())))
  )
    throw new PrivateStorageError('unsafe');
  return { dev: stat.dev, ino: stat.ino };
}

/** The holder record inside a lock or claim folder, read without following links. */
function readLockHolder(lock: string): HolderRecord | null {
  const holder = path.join(lock, LOCK_HOLDER_FILE);
  try {
    const stat = fs.lstatSync(holder);
    if (
      !stat.isFile() ||
      stat.isSymbolicLink() ||
      stat.size > HOLDER_RECORD_BYTES ||
      (process.platform !== 'win32' &&
        ((stat.mode & 0o077) !== 0 || (process.getuid && stat.uid !== process.getuid())))
    )
      return null;
    const fd = fs.openSync(holder, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    try {
      return parseHolderRecord(fs.readFileSync(fd, 'utf8'), fs.fstatSync(fd).mtimeMs);
    } finally {
      fs.closeSync(fd);
    }
  } catch {
    return null;
  }
}

/**
 * One-line reason when the folder is provably abandoned, otherwise null: its
 * holder process is gone (dead, its pid reused, or from an earlier boot), or
 * it is alive but sent no heartbeat for `silentAfterMs`; a folder without a
 * readable holder record ages out on its own mtime.
 */
function staleLockReason(lock: string, now: number, silentAfterMs: number): string | null {
  const holder = readLockHolder(lock);
  if (holder) return holderStaleReason(holder, now, silentAfterMs);
  try {
    if (now - fs.lstatSync(lock).mtimeMs > silentAfterMs)
      return `lock is older than ${silentAfterMs}ms without a holder record`;
  } catch {
    return null;
  }
  return null;
}

/** A displaced lock is removed only when it holds nothing but a holder record. */
function discardStaleLock(aside: string): void {
  try {
    const entries = fs.readdirSync(aside);
    if (entries.length === 1 && entries[0] === LOCK_HOLDER_FILE)
      fs.unlinkSync(path.join(aside, LOCK_HOLDER_FILE));
    if (fs.readdirSync(aside).length === 0) fs.rmdirSync(aside);
  } catch {
    /* Whatever cannot be proven ours is preserved for review. */
  }
}

interface StagedFolder {
  directory: string;
  identity: DirectoryIdentity;
  /** Open on our own holder record: its inode proves the record is ours. */
  holderFd: number;
}

/**
 * A new lock or claim folder, complete with its holder record, made beside
 * `target` before it is renamed into place. A lock or claim folder therefore
 * never sits empty at its path, so a rename onto that path fails rather than
 * replacing one (rename(2) replaces only an empty folder).
 */
function stageHolderFolder(target: string): StagedFolder {
  const directory = `${target}.new-${process.pid}-${randomBytes(6).toString('hex')}`;
  fs.mkdirSync(directory, { mode: 0o700 });
  const made = fs.lstatSync(directory);
  const identity = { dev: made.dev, ino: made.ino };
  let holderFd: number | undefined;
  try {
    holderFd = fs.openSync(
      path.join(directory, LOCK_HOLDER_FILE),
      fs.constants.O_WRONLY |
        fs.constants.O_CREAT |
        fs.constants.O_EXCL |
        (fs.constants.O_NOFOLLOW ?? 0),
      0o600
    );
    fs.writeFileSync(holderFd, ownHolderRecord());
    return { directory, identity, holderFd };
  } catch {
    discardStagedFolder(directory, identity, holderFd);
    throw new PrivateStorageError('unsafe');
  }
}

/** Remove only what this process made: its own holder record by inode, then its own folder by inode. */
function discardStagedFolder(
  directory: string,
  identity: DirectoryIdentity,
  holderFd: number | undefined
): void {
  if (holderFd !== undefined) {
    unlinkOwnHolder(directory, holderFd);
    try {
      fs.closeSync(holderFd);
    } catch {
      /* Already closed. */
    }
  }
  try {
    if (sameDirectory(identity, fs.lstatSync(directory))) fs.rmdirSync(directory);
  } catch {
    /* Not ours any more, or not empty: left for review. */
  }
}

function unlinkOwnHolder(directory: string, holderFd: number): void {
  try {
    const own = fs.fstatSync(holderFd);
    const holder = path.join(directory, LOCK_HOLDER_FILE);
    const named = fs.lstatSync(holder);
    if (named.isFile() && named.dev === own.dev && named.ino === own.ino) fs.unlinkSync(holder);
  } catch {
    /* Gone already, or not ours: never removed. */
  }
}

/** Rename a staged folder onto `target` only when nothing is there; false when something is. */
function placeStagedFolder(staged: StagedFolder, target: string): boolean {
  try {
    fs.lstatSync(target);
    return false;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw new PrivateStorageError('unsafe');
  }
  try {
    fs.renameSync(staged.directory, target);
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code === 'ENOTEMPTY' || code === 'EEXIST') return false;
    throw new PrivateStorageError('unsafe');
  }
  if (!sameDirectory(staged.identity, fs.lstatSync(target)))
    throw new PrivateStorageError('unsafe');
  return true;
}

/**
 * Move `target` aside when it is still the judged folder; returns the aside
 * path, or null when another folder was there (which is then put back). The
 * caller holds the takeover claim, so only a judged-stale holder that was in
 * fact alive and released at this instant can leave a different folder here.
 * A lock or claim folder is never empty at its path, so putting it back can
 * only fill a free path and never replaces a newer one.
 */
function moveAsideIfJudged(target: string, judged: DirectoryIdentity): string | null {
  const aside = `${target}.stale-${process.pid}-${Date.now()}-${randomBytes(4).toString('hex')}`;
  try {
    fs.renameSync(target, aside);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw new PrivateStorageError('unsafe');
  }
  if (sameDirectory(judged, fs.lstatSync(aside))) return aside;
  try {
    fs.renameSync(aside, target);
  } catch {
    console.warn(
      'antigravity: a lock moved during a takeover could not be put back; kept for review'
    );
  }
  return null;
}

/** Clear a takeover claim whose holder is gone or that outlived any takeover; true when cleared. */
function clearAbandonedClaim(claim: string): boolean {
  let judged: fs.Stats;
  try {
    judged = fs.lstatSync(claim);
  } catch {
    return true;
  }
  if (!judged.isDirectory() || judged.isSymbolicLink()) return false;
  const reason = staleLockReason(claim, Date.now(), STALE_CLAIM_MS);
  if (!reason) return false;
  const aside = moveAsideIfJudged(claim, judged);
  if (!aside) return false;
  discardStaleLock(aside);
  console.warn(`antigravity: cleared an abandoned lock takeover claim (${reason})`);
  return true;
}

export interface CredentialPruneReport {
  deleted: string[];
  skipped: string[];
}

/**
 * Credential retention: keep the current file plus ONE previous copy for
 * rollback. Unlinks only owned 0600 regular files directly inside `directory`,
 * selected by the exact credential filename; symlinks are never followed and
 * anything else is left alone and reported. Without `previousFingerprint` the
 * newest other safe copy is kept; `null` keeps no previous copy.
 */
export function pruneSupersededCredentials(
  directory: string,
  currentFingerprint: string,
  previousFingerprint?: string | null
): CredentialPruneReport {
  if (
    !HASH.test(currentFingerprint) ||
    (typeof previousFingerprint === 'string' && !HASH.test(previousFingerprint))
  )
    throw new PrivateStorageError('unsafe');
  ownedPrivateDirectory(directory);
  const report: CredentialPruneReport = { deleted: [], skipped: [] };
  const candidates: Array<{
    name: string;
    fingerprint: string;
    mtimeMs: number;
    safe: boolean;
  }> = [];
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const match = CREDENTIAL_FILE.exec(entry.name);
    if (!match || match[1] === currentFingerprint) continue;
    let mtimeMs = -Infinity;
    let safe = false;
    try {
      const stat = fs.lstatSync(path.join(directory, entry.name));
      mtimeMs = stat.mtimeMs;
      safe =
        stat.isFile() &&
        !stat.isSymbolicLink() &&
        stat.size <= MAX_NATIVE_BYTES &&
        (process.platform === 'win32' ||
          ((stat.mode & 0o777) === 0o600 && (!process.getuid || stat.uid === process.getuid())));
    } catch {
      safe = false;
    }
    candidates.push({ name: entry.name, fingerprint: match[1], mtimeMs, safe });
  }
  const previous =
    previousFingerprint === null
      ? null
      : previousFingerprint !== undefined && previousFingerprint !== currentFingerprint
        ? previousFingerprint
        : candidates
            .filter((candidate) => candidate.safe)
            .sort(
              (left, right) => right.mtimeMs - left.mtimeMs || (right.name > left.name ? 1 : -1)
            )
            .map((candidate) => candidate.fingerprint)[0];
  for (const candidate of candidates) {
    if (candidate.fingerprint === previous) continue;
    if (!candidate.safe) {
      report.skipped.push(candidate.name);
      continue;
    }
    try {
      fs.unlinkSync(path.join(directory, candidate.name));
      report.deleted.push(candidate.name);
    } catch {
      report.skipped.push(candidate.name);
    }
  }
  return report;
}

function readPrivateFile(filename: string, maxBytes: number): Buffer {
  checkAncestors(path.dirname(filename));
  const before = fs.lstatSync(filename);
  if (
    !before.isFile() ||
    before.isSymbolicLink() ||
    before.nlink !== 1 ||
    before.size > maxBytes ||
    (process.platform !== 'win32' &&
      ((before.mode & 0o077) !== 0 || (process.getuid && before.uid !== process.getuid())))
  )
    throw new PrivateStorageError('unsafe');
  const fd = fs.openSync(filename, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
  try {
    const opened = fs.fstatSync(fd);
    if (opened.dev !== before.dev || opened.ino !== before.ino) {
      throw new PrivateStorageError('unsafe');
    }
    const bytes = fs.readFileSync(fd);
    const after = fs.lstatSync(filename);
    if (after.dev !== opened.dev || after.ino !== opened.ino || bytes.length > maxBytes) {
      throw new PrivateStorageError('unsafe');
    }
    return bytes;
  } finally {
    fs.closeSync(fd);
  }
}

/** Immutable publication: an existing/foreign file is never renamed over or deleted. */
function writeExclusive(filename: string, bytes: Buffer, parentIdentity: DirectoryIdentity): void {
  const parent = path.dirname(filename);
  assertDirectory(parent, parentIdentity);
  const fd = fs.openSync(
    filename,
    fs.constants.O_WRONLY |
      fs.constants.O_CREAT |
      fs.constants.O_EXCL |
      (fs.constants.O_NOFOLLOW ?? 0),
    0o600
  );
  try {
    fs.writeFileSync(fd, bytes);
    fs.fsyncSync(fd);
    assertDirectory(parent, parentIdentity);
    const owned = fs.fstatSync(fd);
    const named = fs.lstatSync(filename);
    if (owned.dev !== named.dev || owned.ino !== named.ino || !named.isFile()) {
      throw new PrivateStorageError('unsafe');
    }
  } finally {
    fs.closeSync(fd);
  }
  const directoryFd = fs.openSync(parent, 'r');
  try {
    fs.fsyncSync(directoryFd);
  } finally {
    fs.closeSync(directoryFd);
  }
}

function freshState(): RegistryState {
  return {
    version: 1,
    revision: 0,
    profiles: [],
    active: null,
    transaction: null,
  };
}

function validateState(raw: unknown): RegistryState {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw))
    throw new PrivateStorageError('corrupt');
  const state = raw as RegistryState;
  if (
    state.version !== 1 ||
    !Number.isSafeInteger(state.revision) ||
    state.revision < 1 ||
    !Array.isArray(state.profiles) ||
    state.profiles.length > 64
  ) {
    throw new PrivateStorageError('corrupt');
  }
  const ids = new Set<string>();
  const emails = new Set<string>();
  for (const profile of state.profiles) {
    assertProfileId(profile.id);
    if (ids.has(profile.id) || emails.has(canonicalEmail(profile.email))) {
      throw new PrivateStorageError('corrupt');
    }
    ids.add(profile.id);
    emails.add(canonicalEmail(profile.email));
    const identity = checkedIdentity(profile.credential.identity);
    if (
      profile.email !== identity.email ||
      profile.identityKey !== identityKey(identity) ||
      profile.plan !== identity.plan ||
      !HASH.test(profile.credential.fingerprint) ||
      !FORMAT.test(profile.credential.format) ||
      !Number.isFinite(Date.parse(profile.credential.savedAt))
    ) {
      throw new PrivateStorageError('corrupt');
    }
  }
  if (
    state.active !== null &&
    (!state.active ||
      !ids.has(state.active.profileId) ||
      !Number.isFinite(Date.parse(state.active.verifiedAt)))
  )
    throw new PrivateStorageError('corrupt');
  if (
    state.transaction !== null &&
    (!state.transaction ||
      !/^[a-f0-9]{32}$/.test(state.transaction.id) ||
      !['pending', 'recovery-required'].includes(state.transaction.state) ||
      !ids.has(state.transaction.previousId) ||
      !ids.has(state.transaction.targetId) ||
      !Number.isFinite(Date.parse(state.transaction.startedAt)))
  )
    throw new PrivateStorageError('corrupt');
  return state;
}

/**
 * Storage lives below the existing private .ccs directory. Both credential and
 * metadata publications are immutable; revisions never overwrite predecessors.
 * A held transaction lock is taken over only when its recorded holder process
 * is gone (dead, its pid reused, or from an earlier boot) or alive but silent
 * past a bounded heartbeat timeout. Takeovers are serialized on a claim folder
 * and every one is logged; an unresolved intent still blocks another switch
 * until a separately reviewed recovery verifies the native state. Superseded
 * credential generations are pruned by pruneSupersededCredentials to the
 * current plus one previous copy.
 */
export class AntigravityProfileRegistry {
  private readonly profilesDirectory: string;
  private readonly instancesDirectory: string;
  private readonly profilesIdentity: DirectoryIdentity;
  private readonly instancesIdentity: DirectoryIdentity | null;
  private readonly readOnly: boolean;
  private readonly heartbeatMs: number;
  private lockHeld = false;

  /**
   * `readOnly` opens existing storage without creating any folder; such a
   * registry reads but never locks or writes. It throws when the profiles
   * folder does not exist. `heartbeatMs` exists for tests.
   */
  constructor(ccsDirectory: string, options: { readOnly?: boolean; heartbeatMs?: number } = {}) {
    this.readOnly = options.readOnly === true;
    this.heartbeatMs = options.heartbeatMs ?? LOCK_HEARTBEAT_MS;
    this.profilesDirectory = path.join(ccsDirectory, 'antigravity-profiles');
    this.instancesDirectory = path.join(ccsDirectory, 'antigravity-instances');
    if (this.readOnly) {
      ownedPrivateDirectory(path.resolve(ccsDirectory));
      this.profilesIdentity = ownedPrivateDirectory(this.profilesDirectory);
      let instances: DirectoryIdentity | null = null;
      try {
        instances = ownedPrivateDirectory(this.instancesDirectory);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      }
      this.instancesIdentity = instances;
      return;
    }
    privateDirectory(path.resolve(ccsDirectory));
    this.profilesIdentity = privateDirectory(this.profilesDirectory);
    this.instancesIdentity = privateDirectory(this.instancesDirectory);
  }

  /** A read-only registry over existing storage, or null when none was ever saved. */
  static openExisting(ccsDirectory: string): AntigravityProfileRegistry | null {
    if (!fs.existsSync(path.join(path.resolve(ccsDirectory), 'antigravity-profiles'))) return null;
    return new AntigravityProfileRegistry(ccsDirectory, { readOnly: true });
  }

  private instances(): DirectoryIdentity {
    if (!this.instancesIdentity) throw new PrivateStorageError('missing');
    assertDirectory(this.instancesDirectory, this.instancesIdentity);
    return this.instancesIdentity;
  }

  async withLock<T>(operation: () => Promise<T>): Promise<T> {
    if (this.readOnly) throw new PrivateStorageError('unsafe');
    assertDirectory(this.profilesDirectory, this.profilesIdentity);
    const lock = path.join(this.profilesDirectory, LOCK_NAME);
    const held = this.acquireLock(lock);
    this.lockHeld = true;
    // The heartbeat goes to our own record by its descriptor, never by path,
    // so a lock that was taken over is never refreshed on another's behalf.
    const heartbeat = setInterval(() => {
      try {
        const now = new Date();
        fs.futimesSync(held.holderFd, now, now);
      } catch {
        /* A missed beat only brings the takeover timeout closer. */
      }
    }, this.heartbeatMs);
    heartbeat.unref?.();
    try {
      return await operation();
    } finally {
      this.lockHeld = false;
      clearInterval(heartbeat);
      this.releaseLock(lock, held);
    }
  }

  private releaseLock(lock: string, held: StagedFolder): void {
    try {
      assertDirectory(this.profilesDirectory, this.profilesIdentity);
      let current: fs.Stats;
      try {
        current = fs.lstatSync(lock);
      } catch {
        throw new PrivateStorageError('unsafe');
      }
      if (!sameDirectory(held.identity, current)) throw new PrivateStorageError('unsafe');
      // Our own holder record only; empty-folder removal still refuses foreign contents.
      unlinkOwnHolder(lock, held.holderFd);
      fs.rmdirSync(lock);
    } finally {
      fs.closeSync(held.holderFd);
    }
  }

  /**
   * A live holder with a recent heartbeat always wins; only a provably
   * abandoned lock is taken over. A new lock is staged complete with its
   * holder record and renamed into place, so the lock path is never empty
   * and never free while a holder runs.
   */
  private acquireLock(lock: string): StagedFolder {
    const staged = stageHolderFolder(lock);
    let placed = false;
    try {
      placed = placeStagedFolder(staged, lock);
      if (placed) return staged;
      // The staleness verdict belongs to this exact folder; a lock released or
      // replaced meanwhile is never judged on another lock's record.
      let judged: fs.Stats;
      try {
        judged = fs.lstatSync(lock);
      } catch {
        throw new PrivateStorageError('busy');
      }
      if (!judged.isDirectory() || judged.isSymbolicLink()) throw new PrivateStorageError('unsafe');
      const reason = staleLockReason(lock, Date.now(), STALE_LOCK_TIMEOUT_MS);
      if (!reason) throw new PrivateStorageError('busy');
      this.withTakeoverClaim(lock, () => {
        // Under the claim no other takeover moves this lock and a gone holder
        // never releases it, so the judged lock, if still here, is what moves.
        let current: fs.Stats;
        try {
          current = fs.lstatSync(lock);
        } catch {
          throw new PrivateStorageError('busy');
        }
        if (
          !sameDirectory(judged, current) ||
          !staleLockReason(lock, Date.now(), STALE_LOCK_TIMEOUT_MS)
        )
          throw new PrivateStorageError('busy');
        const aside = moveAsideIfJudged(lock, judged);
        if (!aside) throw new PrivateStorageError('busy');
        placed = placeStagedFolder(staged, lock);
        discardStaleLock(aside);
        if (!placed) throw new PrivateStorageError('busy');
      });
      console.warn(`antigravity: recovered a stale transaction lock (${reason})`);
      return staged;
    } finally {
      if (!placed) discardStagedFolder(staged.directory, staged.identity, staged.holderFd);
    }
  }

  /**
   * Run `critical` (synchronous file calls only) while holding the takeover
   * claim beside `lock`; busy when another takeover holds it. An abandoned
   * claim is cleared first.
   */
  private withTakeoverClaim(lock: string, critical: () => void): void {
    const claim = `${lock}.takeover`;
    const staged = stageHolderFolder(claim);
    let placed = false;
    try {
      placed = placeStagedFolder(staged, claim);
      if (!placed && clearAbandonedClaim(claim)) placed = placeStagedFolder(staged, claim);
      if (!placed) throw new PrivateStorageError('busy');
      critical();
    } finally {
      if (placed) {
        try {
          if (sameDirectory(staged.identity, fs.lstatSync(claim))) {
            unlinkOwnHolder(claim, staged.holderFd);
            fs.rmdirSync(claim);
          }
        } catch {
          /* A claim that is not ours any more is left to its holder. */
        }
        fs.closeSync(staged.holderFd);
      } else {
        discardStagedFolder(staged.directory, staged.identity, staged.holderFd);
      }
    }
  }

  private readState(): RegistryState {
    assertDirectory(this.profilesDirectory, this.profilesIdentity);
    const names = fs
      .readdirSync(this.profilesDirectory)
      .filter((name) => /^registry-\d{12}\.json$/.test(name))
      .sort();
    if (!names.length) return freshState();
    const name = names[names.length - 1];
    try {
      const state = validateState(
        JSON.parse(
          readPrivateFile(path.join(this.profilesDirectory, name), MAX_REGISTRY_BYTES).toString(
            'utf8'
          )
        )
      );
      if (Number(name.slice(9, 21)) !== state.revision) throw new PrivateStorageError('corrupt');
      return state;
    } catch (error) {
      if (error instanceof PrivateStorageError) throw error;
      // Parser exceptions may contain native data. Deliberately discard details.
      throw new PrivateStorageError('corrupt');
    }
  }

  private publish(state: RegistryState): void {
    if (!this.lockHeld) throw new PrivateStorageError('unsafe');
    const next = { ...state, revision: state.revision + 1 };
    const name = `registry-${String(next.revision).padStart(12, '0')}.json`;
    validateState(next);
    writeExclusive(
      path.join(this.profilesDirectory, name),
      Buffer.from(JSON.stringify(next)),
      this.profilesIdentity
    );
  }

  hasRecovery(): boolean {
    return this.readState().transaction !== null;
  }

  listProfiles(): ProfileDto[] {
    const state = this.readState();
    return state.profiles.map((profile) => ({
      id: profile.id,
      email: profile.email,
      plan: profile.plan,
      identityKey: profile.identityKey,
      hosts: [
        {
          hostId: 'ubuntu',
          available: true,
          active: state.active?.profileId === profile.id,
          verifiedAt: state.active?.profileId === profile.id ? state.active.verifiedAt : null,
          verification: state.active?.profileId === profile.id ? 'runtime' : 'stored-only',
        },
      ],
    }));
  }

  findIdentity(identity: VerifiedIdentity): string | undefined {
    const key = identityKey(checkedIdentity(identity));
    return this.readState().profiles.find((profile) => profile.identityKey === key)?.id;
  }

  readCredential(
    profileId: string,
    hostId: AntigravityHostId
  ): {
    credential: NativeCredential;
    identity: VerifiedIdentity;
    credentialRevision: string;
  } {
    assertUbuntu(hostId);
    assertProfileId(profileId);
    const profile = this.readState().profiles.find((entry) => entry.id === profileId);
    if (!profile) throw new PrivateStorageError('corrupt');
    this.instances();
    const directory = path.join(this.instancesDirectory, profileId, 'ubuntu');
    const bytes = readPrivateFile(
      path.join(directory, `credential-${profile.credential.fingerprint}.bin`),
      MAX_NATIVE_BYTES
    );
    const credential = { format: profile.credential.format, bytes };
    if (credentialFingerprint(credential) !== profile.credential.fingerprint) {
      throw new PrivateStorageError('corrupt');
    }
    return {
      credential,
      identity: { ...profile.credential.identity },
      credentialRevision: profile.credential.fingerprint,
    };
  }

  /** Must be called under withLock after driver validation; import never changes native auth. */
  saveCredential(
    profileId: string,
    hostId: AntigravityHostId,
    credential: NativeCredential,
    verified: VerifiedIdentity,
    now: number
  ): void {
    assertUbuntu(hostId);
    assertProfileId(profileId);
    if (
      !this.lockHeld ||
      !FORMAT.test(credential.format) ||
      credential.bytes.length === 0 ||
      credential.bytes.length > MAX_NATIVE_BYTES
    )
      throw new PrivateStorageError('unsafe');
    const identity = checkedIdentity(verified);
    const key = identityKey(identity);
    const state = this.readState();
    const existing = state.profiles.find((profile) => profile.id === profileId);
    if (
      (existing && existing.identityKey !== key) ||
      state.profiles.some(
        (profile) =>
          profile.id !== profileId &&
          (profile.identityKey === key || profile.email === identity.email)
      )
    ) {
      throw new PrivateStorageError('unsafe');
    }
    this.instances();
    const profileDirectory = path.join(this.instancesDirectory, profileId);
    privateDirectory(profileDirectory);
    const hostDirectory = path.join(profileDirectory, hostId);
    const hostIdentity = privateDirectory(hostDirectory);
    const fingerprint = credentialFingerprint(credential);
    const filename = path.join(hostDirectory, `credential-${fingerprint}.bin`);
    if (fs.existsSync(filename)) {
      if (!readPrivateFile(filename, MAX_NATIVE_BYTES).equals(credential.bytes)) {
        throw new PrivateStorageError('corrupt');
      }
    } else {
      writeExclusive(filename, credential.bytes, hostIdentity);
    }
    const record: ProfileRecord = {
      id: profileId,
      email: identity.email,
      identityKey: key,
      plan: identity.plan,
      credential: {
        fingerprint,
        format: credential.format,
        identity,
        savedAt: new Date(now).toISOString(),
      },
    };
    state.profiles = [...state.profiles.filter((profile) => profile.id !== profileId), record];
    this.publish(state);
    // Retention: the published revision references `fingerprint`; keep exactly
    // one previous generation for rollback and delete older copies safely. A
    // new profile has no previous generation: files a removed or failed
    // profile of the same name left behind are not kept as one.
    try {
      const pruned = pruneSupersededCredentials(
        hostDirectory,
        fingerprint,
        !existing
          ? null
          : existing.credential.fingerprint !== fingerprint
            ? existing.credential.fingerprint
            : undefined
      );
      for (const name of pruned.skipped)
        console.warn(`antigravity: superseded credential ${name} was left in place for review`);
    } catch {
      console.warn('antigravity: superseded credential cleanup was skipped for safety');
    }
  }

  beginTransaction(targetId: string, previousId: string, now: number): void {
    const state = this.readState();
    if (state.transaction) throw new PrivateStorageError('recovery-required');
    state.transaction = {
      id: randomBytes(16).toString('hex'),
      state: 'pending',
      targetId,
      previousId,
      startedAt: new Date(now).toISOString(),
    };
    this.publish(state);
  }

  completeTransaction(profileId: string, verifiedAt: string): void {
    const state = this.readState();
    state.active = { profileId, verifiedAt };
    state.transaction = null;
    this.publish(state);
  }

  markRecovery(): void {
    const state = this.readState();
    if (!state.transaction) throw new PrivateStorageError('unsafe');
    state.transaction.state = 'recovery-required';
    this.publish(state);
  }

  abortTransaction(): void {
    const state = this.readState();
    state.transaction = null;
    this.publish(state);
  }

  /**
   * True while a live switch, import or remove holds the transaction lock. An
   * abandoned lock does not count: the next withLock takes it over.
   */
  lockHeldLive(): boolean {
    const lock = path.join(this.profilesDirectory, LOCK_NAME);
    try {
      fs.lstatSync(lock);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
      throw new PrivateStorageError('unsafe');
    }
    return staleLockReason(lock, Date.now(), STALE_LOCK_TIMEOUT_MS) === null;
  }

  /** The fingerprint of a profile's saved credential as the registry records it; null when unknown. */
  savedRevision(profileId: string): string | null {
    return (
      this.readState().profiles.find((entry) => entry.id === profileId)?.credential.fingerprint ??
      null
    );
  }

  /**
   * Remove one saved profile snapshot (CONTRACT-registry-lifecycle 6.7): a new
   * registry revision without it, then its own credential generations. Must be
   * called under withLock. The native login, shared history and every other
   * profile stay untouched; the registry's last runtime-verified active profile
   * and a profile an unresolved switch names are refused. Credential files that
   * cannot be proven ours (wrong name, owner, mode or type) are left in place.
   */
  removeProfile(profileId: string): { leftInPlace: number } {
    assertProfileId(profileId);
    if (!this.lockHeld) throw new PrivateStorageError('unsafe');
    const state = this.readState();
    if (!state.profiles.some((profile) => profile.id === profileId))
      throw new PrivateStorageError('missing');
    if (state.transaction) throw new PrivateStorageError('recovery-required');
    if (state.active?.profileId === profileId) throw new PrivateStorageError('unsafe');
    state.profiles = state.profiles.filter((profile) => profile.id !== profileId);
    this.publish(state);
    return { leftInPlace: this.deleteProfileFiles(profileId) };
  }

  /**
   * Delete the credential files of profile folders no saved profile names
   * (left by a crash between a Remove's registry revision and its file
   * deletion, or by a failed Add). Must be called under withLock, so no save
   * is between writing its file and publishing its revision.
   */
  sweepOrphanedInstances(): { removed: number; leftInPlace: number } {
    if (!this.lockHeld) throw new PrivateStorageError('unsafe');
    const saved = new Set(this.readState().profiles.map((profile) => profile.id));
    this.instances();
    let removed = 0;
    let leftInPlace = 0;
    for (const name of fs.readdirSync(this.instancesDirectory)) {
      if (!PROFILE_ID.test(name) || saved.has(name)) continue;
      const left = this.deleteProfileFiles(name);
      leftInPlace += left;
      if (!fs.existsSync(path.join(this.instancesDirectory, name))) removed += 1;
    }
    return { removed, leftInPlace };
  }

  private deleteProfileFiles(profileId: string): number {
    let left = 0;
    try {
      this.instances();
    } catch {
      return 1;
    }
    const profileDirectory = path.join(this.instancesDirectory, profileId);
    const hostDirectory = path.join(profileDirectory, 'ubuntu');
    let entries: string[] = [];
    try {
      ownedPrivateDirectory(profileDirectory);
      ownedPrivateDirectory(hostDirectory);
      entries = fs.readdirSync(hostDirectory);
    } catch (error) {
      return (error as NodeJS.ErrnoException).code === 'ENOENT' ? 0 : 1;
    }
    for (const name of entries) {
      const filename = path.join(hostDirectory, name);
      try {
        const stat = fs.lstatSync(filename);
        if (
          !CREDENTIAL_FILE.test(name) ||
          !stat.isFile() ||
          stat.isSymbolicLink() ||
          stat.size > MAX_NATIVE_BYTES ||
          (process.platform !== 'win32' &&
            ((stat.mode & 0o777) !== 0o600 || (process.getuid && stat.uid !== process.getuid())))
        ) {
          left += 1;
          continue;
        }
        fs.unlinkSync(filename);
      } catch {
        left += 1;
      }
    }
    for (const directory of [hostDirectory, profileDirectory]) {
      try {
        fs.rmdirSync(directory);
      } catch {
        /* Not empty: whatever is left stays for review. */
      }
    }
    return left;
  }
}
