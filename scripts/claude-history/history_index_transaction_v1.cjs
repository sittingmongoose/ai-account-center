'use strict';
/** Portable, offline filesystem primitive; no app, process, network or provider API.
 * Caller binds fixed internal roots and previously validated immutable bytes.
 * No independent concurrent writer is supported. Path-based checks are not
 * POSIX dirfd anchoring, and cannot remove a malicious syscall-gap path swap.
 */
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const MAX_BYTES = 2000000;
const RECORD_NAME = /^local_[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\.json$/;
const REASONS = new Set([
  'invalid_request', 'invalid_roots', 'invalid_records', 'invalid_record_name',
  'duplicate_record_name', 'invalid_protected', 'duplicate_protected_name',
  'closed_guard_unknown', 'private_storage_unverified', 'symlink_directory',
  'unsafe_directory', 'nonzero_identity_required', 'directory_identity_changed',
  'unsafe_file', 'file_identity_changed', 'protected_metadata_changed',
  'target_already_exists', 'snapshot_identity_changed', 'private_mode_unverified',
  'staging_replaced', 'created_record_changed', 'storage_io_refused', 'nofollow_unavailable',
]);
class Refused extends Error {
  constructor(reason = 'storage_io_refused') {
    super(REASONS.has(reason) ? reason : 'storage_io_refused');
    this.name = 'Refused';
  }
}
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const sameIdentity = (a, b) => a.dev === b.dev && a.ino === b.ino;
function identity(stat) {
  if (stat.dev <= 0n || stat.ino <= 0n) throw new Refused('nonzero_identity_required');
  return { dev: stat.dev, ino: stat.ino };
}
function regularName(name) {
  return typeof name === 'string' && name.length > 0 && name.length <= 240 &&
    name !== '.' && name !== '..' && !/[\\/\x00-\x1f\x7f]/.test(name);
}
function rootPath(value) {
  if (typeof value !== 'string' || !path.isAbsolute(value) || value.includes('\0') ||
      path.normalize(value) !== value) throw new Refused('invalid_roots');
  return value;
}
function directoryIdentity(directory) {
  let component = directory;
  while (true) {
    const info = fs.lstatSync(component, { bigint: true });
    if (info.isSymbolicLink()) throw new Refused('symlink_directory');
    if (!info.isDirectory()) throw new Refused('unsafe_directory');
    const parent = path.dirname(component);
    if (parent === component) break;
    component = parent;
  }
  return identity(fs.lstatSync(directory, { bigint: true }));
}
function readRegular(file) {
  const before = fs.lstatSync(file, { bigint: true });
  if (!before.isFile() || before.isSymbolicLink() || before.size > BigInt(MAX_BYTES)) {
    throw new Refused('unsafe_file');
  }
  const beforeIdentity = identity(before);
  let fd;
  try {
    fd = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0));
    const opened = fs.fstatSync(fd, { bigint: true });
    if (!opened.isFile() || opened.size > BigInt(MAX_BYTES) ||
        !sameIdentity(identity(opened), beforeIdentity)) throw new Refused('file_identity_changed');
    const chunks = [];
    let total = 0;
    while (true) {
      const chunk = Buffer.alloc(Math.min(65536, MAX_BYTES + 1 - total));
      const count = fs.readSync(fd, chunk, 0, chunk.length, null);
      if (count === 0) break;
      total += count;
      if (total > MAX_BYTES) throw new Refused('unsafe_file');
      chunks.push(chunk.subarray(0, count));
    }
    const bytes = Buffer.concat(chunks, total);
    const after = fs.lstatSync(file, { bigint: true });
    if (!after.isFile() || !sameIdentity(identity(after), beforeIdentity)) {
      throw new Refused('file_identity_changed');
    }
    return { bytes, inode: beforeIdentity };
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
  }
}
function transaction(request, offlineHooks = {}) {
  if (!request || typeof request !== 'object' || Array.isArray(request)) throw new Refused('invalid_request');
  const profileRoot = rootPath(request.profileRoot);
  const registryRoot = rootPath(request.registryRoot);
  if (profileRoot === registryRoot || !registryRoot.startsWith(profileRoot + path.sep)) {
    throw new Refused('invalid_roots');
  }
  if (typeof request.closedGuard !== 'function') throw new Refused('closed_guard_unknown');
  // A caller can tighten privacy for synthetic Windows tests, never bypass the native platform.
  const windows = process.platform === 'win32' || request.platform === 'win32';
  if (!windows && !fs.constants.O_NOFOLLOW) throw new Refused('nofollow_unavailable');
  if (windows && typeof request.privateStorageGuard !== 'function') throw new Refused('private_storage_unverified');
  if (request.privateStorageGuard !== undefined && typeof request.privateStorageGuard !== 'function') {
    throw new Refused('private_storage_unverified');
  }
  const closedGuard = request.closedGuard;
  const privateStorageGuard = request.privateStorageGuard;
  const inputRecords = request.records;
  if (!Array.isArray(inputRecords) || inputRecords.length < 1 || inputRecords.length > 200) throw new Refused('invalid_records');
  const recordNames = new Set();
  const records = inputRecords.map(record => {
    if (!record || typeof record !== 'object' || !Buffer.isBuffer(record.bytes) || record.bytes.length > MAX_BYTES) {
      throw new Refused('invalid_records');
    }
    if (typeof record.name !== 'string' || !RECORD_NAME.test(record.name)) throw new Refused('invalid_record_name');
    if (recordNames.has(record.name)) throw new Refused('duplicate_record_name');
    recordNames.add(record.name);
    return { name: record.name, bytes: Buffer.from(record.bytes) };
  });
  if (!Array.isArray(request.protected)) throw new Refused('invalid_protected');
  const protectedNames = new Set();
  const protectedRecords = request.protected.map(record => {
    if (!record || typeof record !== 'object' || !regularName(record.name) ||
        record.name.startsWith('.history-index-') ||
        (record.bytes !== null && (!Buffer.isBuffer(record.bytes) || record.bytes.length > MAX_BYTES))) {
      throw new Refused('invalid_protected');
    }
    if (protectedNames.has(record.name)) throw new Refused('duplicate_protected_name');
    protectedNames.add(record.name);
    return { name: record.name, bytes: record.bytes === null ? null : Buffer.from(record.bytes) };
  });
  const profileIdentity = directoryIdentity(profileRoot);
  const registryIdentity = directoryIdentity(registryRoot);
  let snapshotName = null;
  let snapshotPath = null;
  let snapshotIdentity = null;
  const made = [];
  const stages = [];
  let failureReason = null;
  let createdCountBeforeRefusal = 0;
  let rollbackRemovedCount = 0;
  const hook = (phase, context) => {
    if (typeof offlineHooks.onPhase === 'function') offlineHooks.onPhase(phase, context);
  };
  function rootsBound() {
    if (!sameIdentity(directoryIdentity(profileRoot), profileIdentity) ||
        !sameIdentity(directoryIdentity(registryRoot), registryIdentity)) {
      throw new Refused('directory_identity_changed');
    }
    if (snapshotIdentity !== null && !sameIdentity(directoryIdentity(snapshotPath), snapshotIdentity)) {
      throw new Refused('snapshot_identity_changed');
    }
  }
  function privateStorage(file, kind, phase) {
    if (privateStorageGuard !== undefined) {
      try {
        if (privateStorageGuard({ path: file, kind, phase, windows }) !== true) {
          throw new Refused('private_storage_unverified');
        }
      } catch (_) { throw new Refused('private_storage_unverified'); }
    }
    if (!windows) {
      const stat = fs.lstatSync(file, { bigint: true });
      const expected = kind === 'directory' ? 0o700n : 0o600n;
      if ((stat.mode & 0o777n) !== expected || stat.isSymbolicLink()) throw new Refused('private_mode_unverified');
    }
  }
  function check() {
    rootsBound();
    try {
      if (closedGuard() !== true) throw new Refused('closed_guard_unknown');
    } catch (_) { throw new Refused('closed_guard_unknown'); }
    rootsBound();
    if (windows) {
      privateStorage(profileRoot, 'directory', 'recheckInheritedAcl');
      privateStorage(registryRoot, 'directory', 'recheckInheritedAcl');
    }
    if (snapshotIdentity !== null) privateStorage(snapshotPath, 'directory', 'recheckPrivateStorage');
    rootsBound();
    for (const record of protectedRecords) {
      let current;
      try { current = readRegular(path.join(profileRoot, record.name)).bytes; }
      catch (error) { if (error.code === 'ENOENT') current = null; else throw error; }
      if (record.bytes === null ? current !== null : current === null || !current.equals(record.bytes)) {
        throw new Refused('protected_metadata_changed');
      }
    }
  }
  function mutate(kind, index, file, action) {
    hook('beforeMutation', { kind, index, path: file });
    check();
    return action();
  }
  function exclusive(file, bytes, kind, index, ownedCollection) {
    let fd;
    let item;
    try {
      fd = mutate('exclusiveCreate', index, file, () => fs.openSync(file,
        fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | (fs.constants.O_NOFOLLOW || 0), 0o600));
      const info = fs.fstatSync(fd, { bigint: true });
      if (!info.isFile()) throw new Refused('unsafe_file');
      item = { path: file, inode: identity(info), bytes, complete: false };
      if (ownedCollection) ownedCollection.push(item);
      if (!windows) mutate('privateFileMode', index, file, () => fs.fchmodSync(fd, 0o600));
      privateStorage(file, 'file', 'beforePrivateContent');
      rootsBound();
      const bound = fs.lstatSync(file, { bigint: true });
      if (!sameIdentity(identity(bound), item.inode) || !bound.isFile()) throw new Refused('file_identity_changed');
      let offset = 0;
      while (offset < bytes.length) {
        const written = mutate('writePrivateContent', index, file, () => {
          privateStorage(file, 'file', 'immediatelyBeforePrivateWrite');
          rootsBound();
          const named = fs.lstatSync(file, { bigint: true });
          if (!named.isFile() || !sameIdentity(identity(named), item.inode)) throw new Refused('file_identity_changed');
          return fs.writeSync(fd, bytes, offset, bytes.length - offset);
        });
        if (written <= 0) throw new Refused('storage_io_refused');
        offset += written;
      }
      mutate('flushPrivateContent', index, file, () => fs.fsyncSync(fd));
      const current = readRegular(file);
      if (!sameIdentity(current.inode, item.inode) || !current.bytes.equals(bytes)) throw new Refused('file_identity_changed');
      item.complete = true;
      return item;
    } finally { if (fd !== undefined) fs.closeSync(fd); }
  }
  function removeOwned(item, phase) {
    try { rootsBound(); } catch (_) { return { removed: false, safe: false }; }
    let current;
    try { current = readRegular(item.path); }
    catch (error) { if (error.code === 'ENOENT') return { removed: false, safe: true }; return { removed: false, safe: false }; }
    // Only complete immutable contents can authorize a destructive cleanup.
    if (!item.complete || !sameIdentity(current.inode, item.inode) || !current.bytes.equals(item.bytes)) {
      return { removed: false, safe: false };
    }
    try {
      mutate(phase, -1, item.path, () => {
        const now = readRegular(item.path);
        if (!sameIdentity(now.inode, item.inode) || !now.bytes.equals(item.bytes)) throw new Refused('file_identity_changed');
        fs.unlinkSync(item.path);
      });
      return { removed: true, safe: true };
    } catch (_) { return { removed: false, safe: false }; }
  }
  try {
    check();
    if (windows) {
      privateStorage(profileRoot, 'directory', 'existingInheritedAcl');
      privateStorage(registryRoot, 'directory', 'existingInheritedAcl');
      check();
    }
    for (const record of records) {
      try { fs.lstatSync(path.join(registryRoot, record.name)); }
      catch (error) { if (error.code === 'ENOENT') continue; throw error; }
      throw new Refused('target_already_exists');
    }
    snapshotName = '.history-index-snapshot-' + crypto.randomUUID().replaceAll('-', '');
    snapshotPath = path.join(profileRoot, snapshotName);
    mutate('createSnapshot', -1, snapshotPath, () => fs.mkdirSync(snapshotPath, { mode: 0o700 }));
    snapshotIdentity = directoryIdentity(snapshotPath);
    // Must prove snapshot storage privacy before storing any protected bytes.
    privateStorage(snapshotPath, 'directory', 'beforePrivateContent');
    rootsBound();
    const manifest = [];
    for (let index = 0; index < protectedRecords.length; index++) {
      const record = protectedRecords[index];
      if (record.bytes !== null) exclusive(path.join(snapshotPath, 'protected-' + index + '.bin'), record.bytes, 'snapshot', index);
      manifest.push({ index, present: record.bytes !== null, sha256: record.bytes === null ? null : digest(record.bytes) });
    }
    exclusive(path.join(snapshotPath, 'snapshot-manifest.json'), Buffer.from(JSON.stringify(manifest)), 'snapshot', -1);
    for (let index = 0; index < records.length; index++) {
      const record = records[index];
      const stagePath = path.join(registryRoot, '.history-index-stage-' + crypto.randomUUID().replaceAll('-', ''));
      const stage = exclusive(stagePath, record.bytes, 'stage', index, stages);
      hook('staged', { index, path: stagePath });
      check();
      const staged = readRegular(stagePath);
      if (!sameIdentity(staged.inode, stage.inode) || !staged.bytes.equals(record.bytes)) throw new Refused('staging_replaced');
      const destination = path.join(registryRoot, record.name);
      mutate('linkCreateOnly', index, destination, () => {
        privateStorage(stagePath, 'file', 'immediatelyBeforePublication');
        rootsBound();
        const immediate = readRegular(stagePath);
        if (!sameIdentity(immediate.inode, stage.inode) || !immediate.bytes.equals(record.bytes)) throw new Refused('staging_replaced');
        if (typeof offlineHooks.linkSync === 'function') offlineHooks.linkSync(stagePath, destination, index);
        else fs.linkSync(stagePath, destination);
      });
      const created = { path: destination, inode: stage.inode, bytes: record.bytes, complete: true };
      made.push(created);
      hook('created', { index, path: destination, stagePath });
      check();
      privateStorage(destination, 'file', 'createdRecordRecheck');
      const actual = readRegular(destination);
      if (!sameIdentity(actual.inode, created.inode) || !actual.bytes.equals(created.bytes)) throw new Refused('created_record_changed');
      const cleaned = removeOwned(stage, 'cleanupOwnedStage');
      if (!cleaned.safe) throw new Refused('staging_replaced');
      stages.splice(stages.indexOf(stage), 1);
    }
    check();
    for (const item of made) {
      privateStorage(item.path, 'file', 'finalRecordRecheck');
      const actual = readRegular(item.path);
      if (!sameIdentity(actual.inode, item.inode) || !actual.bytes.equals(item.bytes)) throw new Refused('created_record_changed');
    }
    return {
      status: 'created_metadata', createdCount: made.length, createOnly: true,
      snapshotDirectory: snapshotName, snapshotPrivacy: windows ? 'inherited_acl_guard_verified' : 'posix_0700_0600_verified',
      protectedBytesUnchanged: true, createdSha256: made.map(item => digest(item.bytes)),
      profileApplyEndpointImplemented: false, vendorResumeImplemented: false, distributedLeaseImplemented: false,
    };
  } catch (error) {
    failureReason = error instanceof Refused ? error.message : error.code === 'EEXIST' ? 'target_already_exists' : 'storage_io_refused';
    createdCountBeforeRefusal = made.length;
    let rootsSafe = true;
    try { rootsBound(); } catch (_) { rootsSafe = false; }
    let safe = rootsSafe;
    if (rootsSafe) {
      for (const item of [...made, ...stages].reverse()) {
        const cleanup = removeOwned(item, 'rollbackOwnedFile');
        if (cleanup.removed) rollbackRemovedCount++;
        if (!cleanup.safe) safe = false;
      }
    }
    // Cleanup hooks/guards can invalidate a formerly bound snapshot. Claims
    // describe the current state after the last cleanup attempt, not entry state.
    let finalRootsSafe = true;
    try { rootsBound(); } catch (_) { finalRootsSafe = false; }
    let snapshotPrivacyVerified = false;
    if (finalRootsSafe && snapshotIdentity !== null) {
      try {
        privateStorage(snapshotPath, 'directory', 'refusalPrivacyRecheck');
        rootsBound();
        snapshotPrivacyVerified = true;
      } catch (_) {
        // A privacy callback can itself reveal a changed binding.
        try { rootsBound(); } catch (_) { finalRootsSafe = false; }
      }
    }
    return {
      status: safe ? 'refused_rolled_back' : 'refused_replacement_preserved', reason: failureReason,
      createdCountBeforeRefusal, rollbackRemovedCount, ownedFilesRolledBack: safe,
      snapshotDirectory: snapshotIdentity === null ? null : snapshotName,
      snapshotCreated: snapshotIdentity !== null, snapshotBindingVerified: snapshotIdentity !== null && finalRootsSafe,
      privateSnapshotsPreserved: snapshotIdentity !== null && finalRootsSafe && snapshotPrivacyVerified,
      snapshotPreservationDisposition: snapshotIdentity === null ? 'not_created' : finalRootsSafe ? 'snapshot_not_removed_at_bound_basename' : 'snapshot_not_removed_current_binding_unknown',
      createOnly: true, profileApplyEndpointImplemented: false, vendorResumeImplemented: false, distributedLeaseImplemented: false,
    };
  }
}
module.exports = { transaction, Refused, digest };
