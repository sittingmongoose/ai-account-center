import * as fs from 'fs';
import * as path from 'path';
import { createHash, randomBytes } from 'crypto';
import type { AntigravityHostId, NativeCredential, ProfileDto, VerifiedIdentity } from './types';

const MAX_NATIVE_BYTES = 1024 * 1024;
const MAX_REGISTRY_BYTES = 1024 * 1024;
const PROFILE_ID = /^[a-z][a-z0-9_-]{0,47}$/;
const HASH = /^[a-f0-9]{64}$/;
const FORMAT = /^[a-z][a-z0-9_.-]{1,63}$/;

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
 * Locks are never silently broken after a crash. An unresolved intent blocks
 * another switch until a separately reviewed recovery verifies the native state.
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
    try {
      fs.mkdirSync(lock, { mode: 0o700 });
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'EEXIST') throw new PrivateStorageError('busy');
      throw new PrivateStorageError('unsafe');
    }
    const owner = fs.lstatSync(lock);
    this.lockHeld = true;
    try {
      return await operation();
    } finally {
      this.lockHeld = false;
      assertDirectory(this.profilesDirectory, this.profilesIdentity);
      const current = fs.lstatSync(lock);
      if (!sameDirectory(owner, current)) throw new PrivateStorageError('unsafe');
      // Empty directory removal refuses foreign contents and foreign replacement.
      fs.rmdirSync(lock);
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
