'use strict';

// Reusable staged service. Transport, fixed destination paths and the ordinary
// profile opener belong to the existing authenticated launch service. This
// module never opens an app, resumes a session, stops a process or reads a chat.
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const UUID = /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/;
const SHA = /^[0-9a-f]{64}$/;
const PROJECT = '/mnt/Cursor/PuppetMaster';
const PROFILES = new Set(['platyr', 'gmail', 'party', 'me']);
const DIRECTIONS = Object.freeze({platyr: ['mac', 'windows'], gmail: ['windows', 'mac']});
const NATIVE = Object.freeze({
  mac: {version: '2.19675.0', managerSha256: '102d49173311cfcba1bfa62abc5ea986797f5b97ec16caa3784ee65cd19774ac'},
  windows: {version: '2.19675.0.0', managerSha256: '492242eb2c352f60a886031275b7a37fd2fc791946be98b8c8fdd4bbfc41ded6'},
});
class Refused extends Error {
  constructor(reason) { super(reason); this.reason = reason; }
}
const SAFE_REASONS = new Set(['registry_invalid','record_binding_invalid','record_invalid',
  'record_identity_invalid','duplicate_identity','identity_mismatch','snapshot_changed',
  'endpoint_unverified','identity_collision','target_opened','target_state_changed','history_policy_unverified']);
const sha = value => crypto.createHash('sha256').update(value).digest('hex');
const reject = reason => { throw new Refused(reason); };
const number = value => typeof value === 'number' && Number.isFinite(value) && value >= 0;
const validId = value => typeof value === 'string' && UUID.test(value);
const snapshotRevision = records => sha(JSON.stringify(records.map(r => [r.name, sha(r.bytes)]).sort((a,b) => a[0].localeCompare(b[0]))));

function decodeRecords(snapshot) {
  if (!Array.isArray(snapshot.records) || snapshot.records.length > 200) reject('registry_invalid');
  const ids = new Set(), cliIds = new Set();
  let total = 0;
  return snapshot.records.map(record => {
    if (!record || typeof record.name !== 'string' || !/^local_[0-9a-f-]+\.json$/.test(record.name) ||
        !Buffer.isBuffer(record.bytes) || record.bytes.length > 2_000_000 || !SHA.test(record.sha256 || '') || sha(record.bytes) !== record.sha256) reject('record_binding_invalid');
    total += record.bytes.length;
    if (total > 16_000_000) reject('registry_invalid');
    let value;
    try { value = JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(record.bytes).replace(/^\uFEFF/, '')); }
    catch { reject('record_invalid'); }
    if (!value || Array.isArray(value) || typeof value !== 'object' || typeof value.sessionId !== 'string' ||
        !value.sessionId.startsWith('local_') || !validId(value.sessionId.slice(6)) || !validId(value.cliSessionId) ||
        record.name !== value.sessionId + '.json') reject('record_identity_invalid');
    if (ids.has(value.sessionId) || cliIds.has(value.cliSessionId)) reject('duplicate_identity');
    ids.add(value.sessionId); cliIds.add(value.cliSessionId);
    return value;
  });
}

function validatePolicy(policy) {
  if (!policy || policy.version !== 1 || policy.enabled !== true ||
      !['mac', 'windows'].includes(policy.sourcePlatform) ||
      !validId(policy.identity?.accountUuid) || !validId(policy.identity?.organizationUuid) ||
      policy.project?.cwd !== PROJECT || policy.project?.originCwd !== PROJECT ||
      typeof policy.project.transcriptRoot !== 'string' ||
      !/^\/[^\\\x00-\x1f\x7f]+\/\.claude\/projects$/.test(policy.project.transcriptRoot) ||
      policy.project.transcriptRoot.split('/').some(p => p === '.' || p === '..') ||
      policy.project.transcriptRoot.includes('//')) reject('history_policy_unverified');
  for (const platform of ['mac', 'windows']) {
    const endpoint = policy.ssh?.[platform];
    if (!endpoint || typeof endpoint.alias !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(endpoint.alias) ||
        typeof endpoint.hostname !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9.:-]{0,254}$/.test(endpoint.hostname) ||
        typeof endpoint.username !== 'string' || !/^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$/.test(endpoint.username) ||
        !Number.isSafeInteger(endpoint.port) || endpoint.port < 1 || endpoint.port > 65535) reject('history_policy_unverified');
  }
  const a = policy.ssh.mac, b = policy.ssh.windows;
  if (a.hostname !== b.hostname || a.username !== b.username || a.port !== b.port) reject('history_policy_unverified');
  return policy;
}

function validateSnapshot(snapshot, profileId, platform, policy) {
  if (!snapshot || snapshot.profileId !== profileId || snapshot.platform !== platform ||
      snapshot.identity?.accountSha256 !== sha(policy.identity.accountUuid.toLowerCase()) ||
      snapshot.identity?.orgSha256 !== sha(policy.identity.organizationUuid.toLowerCase())) reject('identity_mismatch');
  const rows = decodeRecords(snapshot);
  if (snapshot.snapshotStable !== true || snapshot.revision !== snapshotRevision(snapshot.records)) reject('snapshot_changed');
  const endpoint = policy.ssh[platform];
  for (const [key, expectedValue] of Object.entries({hostnameSha256: sha(endpoint.hostname), usernameSha256: sha(endpoint.username), portSha256: sha(String(endpoint.port))})) {
    if (snapshot.endpoint?.[key] !== expectedValue) reject('endpoint_unverified');
  }
  return rows;
}

function nativeGuardVerified(snapshot, platform) {
  const expected = NATIVE[platform], actual = snapshot.nativeGuard;
  return actual?.version === expected.version && actual?.managerSha256 === expected.managerSha256 &&
    actual.warmGuardVerified === true && actual.autoResumeGuardVerified === true;
}

function neutral(source, sourcePlatform, targetPlatform, policy) {
  if (source.cwd !== policy.project.cwd || source.originCwd !== policy.project.originCwd || source.sshConfig?.sshHost !== policy.ssh[sourcePlatform].alias) return {reason: 'project_or_backend_not_verified'};
  if (source.wslConfig || (source.backend && source.backend.kind !== 'ssh')) return {reason: 'project_or_backend_not_verified'};
  const reference = source.sshRemoteTranscriptPath;
  if (typeof reference !== 'string' || !reference.startsWith(policy.project.transcriptRoot + '/') ||
      reference.includes('\\') || reference.includes('//') || reference.slice(policy.project.transcriptRoot.length + 1).split('/').length !== 2 ||
      reference.split('/').some(p => p === '.' || p === '..') || reference.split('/').at(-1) !== source.cliSessionId + '.jsonl') return {reason: 'transcript_reference_invalid'};
  if (!number(source.createdAt) || !number(source.lastActivityAt) || typeof source.title !== 'string' ||
      source.title.length < 1 || source.title.length > 1000 || /[\x00-\x1f\x7f]/.test(source.title) ||
      typeof source.isArchived !== 'boolean') return {reason: 'display_metadata_invalid'};
  const descriptor = {
    sessionId: source.sessionId, cliSessionId: source.cliSessionId, cwd: policy.project.cwd, originCwd: policy.project.originCwd,
    createdAt: source.createdAt, lastActivityAt: source.lastActivityAt, lastFocusedAt: 0,
    title: source.title, isArchived: source.isArchived, permissionMode: 'default',
    importedFrom: 'local-1p-code', resumeConfirmed: false, remoteControlAutoEligible: false,
    sshConfig: {sshHost: policy.ssh[targetPlatform].alias}, sshRemoteTranscriptPath: reference,
  };
  if (typeof source.model === 'string' && /^claude-[a-zA-Z0-9._-]{1,100}$/.test(source.model)) descriptor.model = source.model;
  if (['low','medium','high','max','xhigh'].includes(source.effort)) descriptor.effort = source.effort;
  if (Number.isSafeInteger(source.completedTurns) && source.completedTurns >= 0 && source.completedTurns <= 1_000_000) descriptor.completedTurns = source.completedTurns;
  return {descriptor};
}

function planMissing(sourceRows, targetRows, sourcePlatform, targetPlatform, policy) {
  const records = [], skipped = {};
  let alreadyPresentCount = 0;
  for (const source of sourceRows) {
    const projected = neutral(source, sourcePlatform, targetPlatform, policy);
    if (!projected.descriptor) { skipped[projected.reason] = (skipped[projected.reason] || 0) + 1; continue; }
    const candidate = projected.descriptor;
    const matches = targetRows.filter(row => row.sessionId === candidate.sessionId || row.cliSessionId === candidate.cliSessionId);
    if (matches.length) {
      const existing = matches[0];
      if (matches.length !== 1 || existing.sessionId !== candidate.sessionId || existing.cliSessionId !== candidate.cliSessionId ||
          existing.cwd !== candidate.cwd || existing.originCwd !== candidate.originCwd ||
          existing.sshConfig?.sshHost !== candidate.sshConfig.sshHost || existing.sshRemoteTranscriptPath !== candidate.sshRemoteTranscriptPath) reject('identity_collision');
      // Mutable native titles, archive state, runtime and pending fields stay
      // authoritative and untouched; the immutable original binding matches.
      alreadyPresentCount++;
      continue;
    }
    records.push({name: candidate.sessionId + '.json', bytes: Buffer.from(JSON.stringify(candidate) + '\n')});
  }
  return {records, alreadyPresentCount, skipped};
}

async function synchronizeBeforeProfileOpen({profileId, targetPlatform, policy, adapters}) {
  if (typeof profileId !== 'string' || typeof targetPlatform !== 'string' || !PROFILES.has(profileId) || !['mac', 'windows'].includes(targetPlatform)) return {status: 'skipped', reason: 'unsupported_profile_or_platform', createdCount: 0};
  const direction = DIRECTIONS[profileId];
  if (!direction || direction[1] !== targetPlatform) return {status: 'skipped', reason: 'unsupported_direction', createdCount: 0};
  const sourcePlatform = direction[0];
  const skipped = reason => ({status: 'skipped', reason, createdCount: 0});
  try {
    validatePolicy(policy);
    if (policy.sourcePlatform !== sourcePlatform) return skipped('history_policy_unverified');
    if (!adapters || !['isTargetClosed','readSource','readTarget','verifyTranscriptReferences','appendCreateOnly','targetProtectedStateUnchanged'].every(name => typeof adapters[name] === 'function')) return skipped('adapter_unavailable');
    if (await adapters.isTargetClosed(profileId, targetPlatform) !== true) return skipped('profile_already_open_or_unknown');
    const target = await adapters.readTarget(profileId, targetPlatform);
    const targetRows = validateSnapshot(target, profileId, targetPlatform, policy);
    if (!nativeGuardVerified(target, targetPlatform)) return skipped('native_guard_unverified');
    if (target.noPendingInput !== true || target.noScheduledWork !== true || target.protectedSnapshotStable !== true) return skipped('destination_work_or_state_unknown');
    const source = await adapters.readSource(profileId, sourcePlatform);
    const sourceRows = validateSnapshot(source, profileId, sourcePlatform, policy);
    if (!nativeGuardVerified(source, sourcePlatform)) return skipped('native_guard_unverified');
    const plan = planMissing(sourceRows, targetRows, sourcePlatform, targetPlatform, policy);
    const publicCounts = {alreadyPresentCount: plan.alreadyPresentCount, skippedReasonCounts: plan.skipped};
    if (!plan.records.length) return {status: 'unchanged', createdCount: 0, ...publicCounts};
    // Metadata-only stat/identity availability check: never read a transcript,
    // infer writer inactivity, or grant resume authority in this index service.
    if (await adapters.verifyTranscriptReferences(profileId, sourcePlatform, plan.records) !== true) return skipped('transcript_metadata_unverified');
    const sourceAgain = await adapters.readSource(profileId, sourcePlatform);
    validateSnapshot(sourceAgain, profileId, sourcePlatform, policy);
    if (!nativeGuardVerified(sourceAgain, sourcePlatform)) return skipped('native_guard_unverified');
    if (sourceAgain.revision !== source.revision) return skipped('source_changed');
    const guard = async () => {
      if (await adapters.isTargetClosed(profileId, targetPlatform) !== true) reject('target_opened');
      if (await adapters.targetProtectedStateUnchanged?.(target) !== true) reject('target_state_changed');
    };
    await guard();
    let result;
    try { result = await adapters.appendCreateOnly({profileId, targetPlatform, target, records: plan.records, closedGuard: guard}); }
    catch {
      // A lost append receipt is not proof that the remote writer did nothing.
      // Preserve unknown partial metadata/snapshots; never automatically retry.
      return {status: 'refused', reason: 'create_only_transaction_unconfirmed', createdCount: 0, recoveryRequired: true};
    }
    if (result?.status !== 'created_metadata' || result.createdCount !== plan.records.length || result.protectedBytesUnchanged !== true) return {
      status: 'refused', reason: 'create_only_transaction_refused', createdCount: 0,
      recoveryRequired: result?.recoveryRequired === true || result?.status === 'refused_replacement_preserved' ||
        (result?.status !== 'refused_rolled_back' && !(result?.status === 'refused' && result?.ownedFilesRolledBack === true && result?.recoveryRequired === false)),
      createdCountBeforeRefusal: Number.isInteger(result?.createdCountBeforeRefusal) && result.createdCountBeforeRefusal >= 0 && result.createdCountBeforeRefusal <= 200 ? result.createdCountBeforeRefusal : 0,
    };
    return {status: 'synchronized', createdCount: result.createdCount, ...publicCounts,
      originalIdsPreserved: true, explicitNativeConfirmationRequired: true,
      resumedSessions: 0, copiedTranscripts: 0};
  } catch (error) {
    // Adapter/provider/path errors may contain secrets. Only our fixed known
    // reason tokens leave this boundary; no raw error or stack is returned.
    return skipped(error instanceof Refused && SAFE_REASONS.has(error.reason) ? error.reason : 'unavailable');
  }
}

async function beforeOrdinaryProfileOpen(synchronize, ordinaryOpen) {
  // The caller is the existing authorized Open action. Skip/refusal cannot stop
  // users from opening their profile; it only refuses the optional index copy.
  let historySync;
  try { historySync = await synchronize(); }
  catch { historySync = {status: 'skipped', reason: 'unavailable', createdCount: 0}; }
  if (historySync?.reason === 'create_only_transaction_unconfirmed') throw new Refused('history_append_pending');
  const opened = await ordinaryOpen();
  return {opened, historySync};
}

// This is a durable uncertain-append hold, not a writer lease. Each operation
// owns a separate create-only file. Finishing its own file cannot clear another
// operation, and server restart / policy removal never proves quiescence.
const MARKER_LIMIT = 256;
const MARKER_NAME = /^(platyr|gmail|party|me)-(mac|windows)-([0-9a-f]{32})\.json$/;
const markerDirectory = directory => path.join(directory, 'claude-history-pending');
function syncDirectory(directory) {
  const fd = fs.openSync(directory, fs.constants.O_RDONLY | fs.constants.O_DIRECTORY | fs.constants.O_NOFOLLOW);
  try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
}
function ownedPrivate(info, directory = false) {
  return (directory ? info.isDirectory() : info.isFile()) && !info.isSymbolicLink() &&
    process.platform !== 'win32' && info.uid === process.getuid() &&
    (info.mode & 0o777) === (directory ? 0o700 : 0o600) &&
    (directory || info.nlink === 1);
}
function rootIsPrivate(directory) {
  return ownedPrivate(fs.lstatSync(directory), true) && fs.realpathSync(directory) === path.resolve(directory);
}
function readMarker(filename) {
  const before = fs.lstatSync(filename);
  if (!ownedPrivate(before) || before.size > 2048) throw new Refused('history_append_pending');
  const fd = fs.openSync(filename, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try {
    const opened = fs.fstatSync(fd), bytes = fs.readFileSync(fd), after = fs.lstatSync(filename);
    if (!ownedPrivate(opened) || opened.dev !== before.dev || opened.ino !== before.ino ||
        opened.dev !== after.dev || opened.ino !== after.ino || bytes.length > 2048 ||
        opened.size !== after.size || opened.mtimeMs !== after.mtimeMs || opened.ctimeMs !== after.ctimeMs)
      throw new Refused('history_append_pending');
    const record = JSON.parse(new TextDecoder('utf-8', {fatal:true}).decode(bytes));
    if (!bytes.equals(Buffer.from(JSON.stringify(record)))) throw new Refused('history_append_pending');
    const match = MARKER_NAME.exec(path.basename(filename));
    const allowed = record?.state === 'finished'
      ? ['version','profileId','targetPlatform','nonce','state','createdAt','finishedAt','terminalReceiptSha256']
      : ['version','profileId','targetPlatform','nonce','state','createdAt'];
    if (!match || !record || Array.isArray(record) || Object.keys(record).length !== allowed.length ||
        Object.keys(record).some(key => !allowed.includes(key)) || record.version !== 1 ||
        record.profileId !== match[1] || record.targetPlatform !== match[2] || record.nonce !== match[3] ||
        !['pending','finished'].includes(record.state) || typeof record.createdAt !== 'string' ||
        !Number.isFinite(Date.parse(record.createdAt)) || (record.state === 'finished' &&
          (typeof record.finishedAt !== 'string' || !Number.isFinite(Date.parse(record.finishedAt)) ||
           !SHA.test(record.terminalReceiptSha256 || '')))) throw new Refused('history_append_pending');
    return {record, bytes, info: opened};
  } finally { fs.closeSync(fd); }
}
function pendingMarkerState(directory, profileId, targetPlatform) {
  if (!PROFILES.has(profileId) || !['mac','windows'].includes(targetPlatform)) return {held:true};
  const root = markerDirectory(directory);
  try {
    const info = fs.lstatSync(root);
    if (!rootIsPrivate(directory) || !ownedPrivate(info, true)) return {held:true};
    const names = fs.readdirSync(root);
    if (names.length > MARKER_LIMIT) return {held:true};
    for (const name of names) {
      const match = MARKER_NAME.exec(name);
      if (!match) return {held:true};
      const {record} = readMarker(path.join(root, name));
      if (record.profileId === profileId && record.targetPlatform === targetPlatform && record.state === 'pending')
        return {held:true};
    }
    return {held:false, count:names.length};
  } catch (error) {
    if (error.code === 'ENOENT' && !fs.existsSync(root)) return {held:false, count:0};
    return {held:true};
  }
}
function armPendingMarker(directory, profileId, targetPlatform) {
  if (pendingMarkerState(directory, profileId, targetPlatform).held) throw new Refused('history_append_pending');
  if (!rootIsPrivate(directory)) throw new Refused('history_marker_unavailable');
  const root = markerDirectory(directory);
  try { fs.mkdirSync(root, {mode:0o700}); } catch (error) { if (error.code !== 'EEXIST') throw error; }
  syncDirectory(directory);
  const state = pendingMarkerState(directory, profileId, targetPlatform);
  if (state.held) throw new Refused('history_append_pending');
  if (state.count >= MARKER_LIMIT) throw new Refused('history_marker_unavailable');
  const nonce = crypto.randomBytes(16).toString('hex');
  const filename = path.join(root, `${profileId}-${targetPlatform}-${nonce}.json`);
  const record = {version:1, profileId, targetPlatform, nonce, state:'pending', createdAt:new Date().toISOString()};
  const bytes = Buffer.from(JSON.stringify(record));
  const fd = fs.openSync(filename, fs.constants.O_RDWR | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
  let released = false;
  const bound = () => {
    const current = readMarker(filename), original = fs.fstatSync(fd);
    if (!rootIsPrivate(directory) || !ownedPrivate(fs.lstatSync(root), true) ||
        current.info.dev !== original.dev || current.info.ino !== original.ino || !current.bytes.equals(bytes))
      throw new Refused('history_append_pending');
  };
  try { fs.writeSync(fd, bytes, 0, bytes.length, 0); fs.fsyncSync(fd); syncDirectory(root); bound(); }
  catch (error) { fs.closeSync(fd); throw error; }
  return {
    assertBound: bound,
    finish: terminalReceipt => {
      if (released || !terminalReceipt || terminalReceipt.writerQuiescent !== true ||
          !['created_metadata','refused'].includes(terminalReceipt.status) ||
          !Number.isInteger(terminalReceipt.createdCount) || terminalReceipt.createdCount < 0 || terminalReceipt.createdCount > 200)
        throw new Refused('history_append_pending');
      bound();
      const finished = Buffer.from(JSON.stringify({...record, state:'finished', finishedAt:new Date().toISOString(),
        terminalReceiptSha256:sha(JSON.stringify(terminalReceipt))}));
      // Write only our held inode. A pathname replacement is never deleted,
      // overwritten or accepted as this operation's completion.
      fs.ftruncateSync(fd, 0); fs.writeSync(fd, finished, 0, finished.length, 0); fs.fsyncSync(fd);
      const current = readMarker(filename), original = fs.fstatSync(fd);
      if (current.info.dev !== original.dev || current.info.ino !== original.ino || !current.bytes.equals(finished))
        throw new Refused('history_append_pending');
    },
    release: () => { if (!released) { released = true; fs.closeSync(fd); } },
  };
}

function createLocalTargetAppender({profileId, targetPlatform, profileRoot, registryRoot, protectedFiles,
    localClosedGuard, privateStorageGuard}) {
  // Instantiate inside the known target helper only, with internally derived
  // profile/UUID/org paths. Never accept these paths from an HTTP request.
  const {transaction} = require('./history_index_transaction_v1.cjs');
  return async payload => {
    if (payload.profileId !== profileId || payload.targetPlatform !== targetPlatform) reject('identity_mismatch');
    await payload.closedGuard();
    return transaction({profileRoot, registryRoot, records: payload.records,
      protected: protectedFiles, closedGuard: localClosedGuard, privateStorageGuard,
      platform: targetPlatform === 'windows' ? 'win32' : process.platform});
  };
}

module.exports = {synchronizeBeforeProfileOpen, beforeOrdinaryProfileOpen,
  createLocalTargetAppender, planMissing, neutral, decodeRecords, snapshotRevision, validatePolicy, PROFILES, DIRECTIONS, NATIVE, Refused,
  pendingMarkerState, armPendingMarker};
