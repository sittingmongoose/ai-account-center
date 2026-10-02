import * as fs from 'fs';
import * as path from 'path';
import { createHash, randomBytes } from 'crypto';
import type { AntigravityHostId, NativeCredential, ProfileDto, VerifiedIdentity } from './types';

const MAX_NATIVE_BYTES = 1024 * 1024;
const MAX_REGISTRY_BYTES = 1024 * 1024;
const PROFILE_ID = /^[a-z][a-z0-9_-]{0,47}$/;
const HASH = /^[a-f0-9]{64}$/;
const FORMAT = /^[a-z][a-z0-9_.-]{1,63}$/;
const LOCK_HOLDER_FILE = 'holder.json';
const LOCK_HOLDER_BYTES = 512;
/** The switch flow bounds nothing longer (broker approval TTL 60s, runtime-proof
 * budget 10s, identity max age 5min); stale locks age out after 10 minutes. */
const STALE_LOCK_TIMEOUT_MS = 10 * 60_000;
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
  constructor(public readonly code: 'busy' | 'unsafe' | 'corrupt' | 'recovery-required') {
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

function readLockHolder(lock: string): { pid: number; startedAt: number } | null {
  const holder = path.join(lock, LOCK_HOLDER_FILE);
  try {
    const stat = fs.lstatSync(holder);
    if (
      !stat.isFile() ||
      stat.isSymbolicLink() ||
      stat.size > LOCK_HOLDER_BYTES ||
      (process.platform !== 'win32' &&
        ((stat.mode & 0o077) !== 0 || (process.getuid && stat.uid !== process.getuid())))
    )
      return null;
    const fd = fs.openSync(holder, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    let parsed: unknown;
    try {
      parsed = JSON.parse(fs.readFileSync(fd, 'utf8'));
    } finally {
      fs.closeSync(fd);
    }
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
    const { pid, startedAt } = parsed as { pid?: unknown; startedAt?: unknown };
    if (!Number.isSafeInteger(pid) || (pid as number) <= 0 || typeof startedAt !== 'string')
      return null;
    const started = Date.parse(startedAt);
    return Number.isFinite(started) ? { pid: pid as number, startedAt: started } : null;
  } catch {
    return null;
  }
}

function holderAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // ESRCH proves exit; anything else (for example EPERM) still counts as live.
    return (error as NodeJS.ErrnoException).code !== 'ESRCH';
  }
}

/** One-line reason when the lock is provably abandoned, otherwise null. */
function staleLockReason(lock: string, now: number): string | null {
  const holder = readLockHolder(lock);
  if (holder) {
    if (!holderAlive(holder.pid)) return `holder pid ${holder.pid} is dead`;
    if (now - holder.startedAt > STALE_LOCK_TIMEOUT_MS)
      return `holder pid ${holder.pid} held the lock for ${now - holder.startedAt}ms`;
    return null;
  }
  // A crashed writer or a pre-holder record lock ages out on its directory time.
  try {
    if (now - fs.lstatSync(lock).mtimeMs > STALE_LOCK_TIMEOUT_MS)
      return `lock is older than ${STALE_LOCK_TIMEOUT_MS}ms without a holder record`;
  } catch {
    return null;
  }
  return null;
}

/** A displaced lock is removed only when it holds nothing but our own holder record. */
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

export interface CredentialPruneReport {
  deleted: string[];
  skipped: string[];
}

/**
 * Credential retention: keep the current file plus ONE previous copy for
 * rollback. Unlinks only owned 0600 regular files directly inside `directory`,
 * selected by the exact credential filename; symlinks are never followed and
 * anything else is left alone and reported.
 */
export function pruneSupersededCredentials(
  directory: string,
  currentFingerprint: string,
  previousFingerprint?: string
): CredentialPruneReport {
  if (
    !HASH.test(currentFingerprint) ||
    (previousFingerprint !== undefined && !HASH.test(previousFingerprint))
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
    previousFingerprint !== undefined && previousFingerprint !== currentFingerprint
      ? previousFingerprint
      : candidates
          .filter((candidate) => candidate.safe)
          .sort((left, right) => right.mtimeMs - left.mtimeMs || (right.name > left.name ? 1 : -1))
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
 * A held transaction lock is taken over only when its recorded holder is dead
 * or the lock aged past a bounded timeout, and every takeover is logged; an
 * unresolved intent still blocks another switch until a separately reviewed
 * recovery verifies the native state. Superseded credential generations are
 * pruned by pruneSupersededCredentials to the current plus one previous copy.
 */
export class AntigravityProfileRegistry {
  private readonly profilesDirectory: string;
  private readonly instancesDirectory: string;
  private readonly profilesIdentity: DirectoryIdentity;
  private readonly instancesIdentity: DirectoryIdentity;
  private lockHeld = false;

  constructor(ccsDirectory: string) {
    privateDirectory(path.resolve(ccsDirectory));
    this.profilesDirectory = path.join(ccsDirectory, 'antigravity-profiles');
    this.instancesDirectory = path.join(ccsDirectory, 'antigravity-instances');
    this.profilesIdentity = privateDirectory(this.profilesDirectory);
    this.instancesIdentity = privateDirectory(this.instancesDirectory);
  }

  async withLock<T>(operation: () => Promise<T>): Promise<T> {
    assertDirectory(this.profilesDirectory, this.profilesIdentity);
    const lock = path.join(this.profilesDirectory, '.transaction-lock');
    const owner = this.acquireLockDirectory(lock);
    this.lockHeld = true;
    try {
      return await operation();
    } finally {
      this.lockHeld = false;
      assertDirectory(this.profilesDirectory, this.profilesIdentity);
      const current = fs.lstatSync(lock);
      if (!sameDirectory(owner, current)) throw new PrivateStorageError('unsafe');
      // Remove our own holder record; empty directory removal still refuses
      // foreign contents and foreign replacement.
      try {
        fs.unlinkSync(path.join(lock, LOCK_HOLDER_FILE));
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      }
      fs.rmdirSync(lock);
    }
  }

  /** A live, recent holder always wins; only a provably abandoned lock is taken over. */
  private acquireLockDirectory(lock: string): DirectoryIdentity {
    try {
      fs.mkdirSync(lock, { mode: 0o700 });
      this.writeLockHolder(lock);
      return fs.lstatSync(lock);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST')
        throw new PrivateStorageError('unsafe');
    }
    const reason = staleLockReason(lock, Date.now());
    if (!reason) throw new PrivateStorageError('busy');
    // Atomic takeover: exactly one racer can rename a given source aside; every
    // other racer observes it gone (ENOENT) and yields to that winner as busy.
    const aside = `${lock}.stale-${process.pid}-${Date.now()}`;
    try {
      fs.renameSync(lock, aside);
    } catch (error) {
      throw new PrivateStorageError(
        (error as NodeJS.ErrnoException).code === 'ENOENT' ? 'busy' : 'unsafe'
      );
    }
    try {
      fs.mkdirSync(lock, { mode: 0o700 });
      this.writeLockHolder(lock);
    } catch (error) {
      discardStaleLock(aside);
      if ((error as NodeJS.ErrnoException).code === 'EEXIST') throw new PrivateStorageError('busy');
      throw new PrivateStorageError('unsafe');
    }
    const owner = fs.lstatSync(lock);
    discardStaleLock(aside);
    console.warn(`antigravity: recovered a stale transaction lock (${reason})`);
    return owner;
  }

  private writeLockHolder(lock: string): void {
    const holder = path.join(lock, LOCK_HOLDER_FILE);
    try {
      fs.writeFileSync(
        holder,
        JSON.stringify({ pid: process.pid, startedAt: new Date().toISOString() }),
        { flag: 'wx', mode: 0o600 }
      );
    } catch {
      // A lock without a holder record could survive our own crash; never hold
      // it half-written.
      try {
        fs.rmSync(holder, { force: true });
        fs.rmdirSync(lock);
      } catch {
        /* Release-time identity checks still refuse foreign replacement. */
      }
      throw new PrivateStorageError('unsafe');
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
    assertDirectory(this.instancesDirectory, this.instancesIdentity);
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
    assertDirectory(this.instancesDirectory, this.instancesIdentity);
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
    // one previous generation for rollback and delete older copies safely.
    try {
      const pruned = pruneSupersededCredentials(
        hostDirectory,
        fingerprint,
        existing && existing.credential.fingerprint !== fingerprint
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
}
