'use strict';
// Every filesystem touched by tests is a newly created OS temporary directory.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const assert = require('node:assert/strict');
const { transaction, Refused, digest } = require('../../../scripts/claude-history/history_index_transaction_v1.cjs');
const cases = [];
const test = (name, run) => cases.push({ name, run });
function record(index) {
  const id = index.toString(16).padStart(12, '0');
  return { name: 'local_00000000-0000-4000-8000-' + id + '.json', bytes: Buffer.from(JSON.stringify({ synthetic: index })) };
}
function fixture(count = 1) {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), 'history-index-synthetic-'));
  const profileRoot = path.join(base, 'profile');
  const registryRoot = path.join(profileRoot, 'registry');
  fs.mkdirSync(profileRoot, { mode: 0o700 });
  fs.mkdirSync(registryRoot, { mode: 0o700 });
  const config = Buffer.from('SYNTHETIC_PROTECTED_CONFIG');
  fs.writeFileSync(path.join(profileRoot, 'config.json'), config, { mode: 0o600 });
  const request = { profileRoot, registryRoot, records: Array.from({ length: count }, (_, index) => record(index + 1)),
    protected: [{ name: 'config.json', bytes: config }, { name: 'absent.json', bytes: null }], closedGuard: () => true };
  return { base, profileRoot, registryRoot, request, close() { fs.rmSync(base, { recursive: true, force: true }); } };
}
function withFixture(count, run) {
  const f = fixture(count);
  try { run(f); } finally { f.close(); }
}
const targets = f => fs.readdirSync(f.registryRoot).filter(name => name.startsWith('local_'));
const stages = f => fs.readdirSync(f.registryRoot).filter(name => name.startsWith('.history-index-stage-'));
function verifyProtected(f) {
  assert.deepEqual(fs.readFileSync(path.join(f.profileRoot, 'config.json')), f.request.protected[0].bytes);
  assert.equal(fs.existsSync(path.join(f.profileRoot, 'absent.json')), false);
}
function requireRefused(call, reason) {
  assert.throws(call, error => error instanceof Refused && error.message === reason);
}
for (const count of [1, 18, 32, 200]) {
  test('create_' + count + '_exact_records_and_private_snapshots', () => withFixture(count, f => {
    const result = transaction(f.request);
    assert.equal(result.status, 'created_metadata');
    assert.equal(result.createdCount, count);
    assert.equal(result.createOnly, true);
    assert.equal(result.snapshotPrivacy, 'posix_0700_0600_verified');
    assert.equal(result.profileApplyEndpointImplemented, false);
    assert.equal(result.vendorResumeImplemented, false);
    assert.equal(result.distributedLeaseImplemented, false);
    assert.equal(targets(f).length, count);
    assert.equal(stages(f).length, 0);
    for (const item of f.request.records) {
      const target = path.join(f.registryRoot, item.name);
      assert.deepEqual(fs.readFileSync(target), item.bytes);
      assert.equal(fs.statSync(target).mode & 0o777, 0o600);
    }
    const snapshot = path.join(f.profileRoot, result.snapshotDirectory);
    assert.equal(fs.statSync(snapshot).mode & 0o777, 0o700);
    for (const name of fs.readdirSync(snapshot)) assert.equal(fs.statSync(path.join(snapshot, name)).mode & 0o777, 0o600);
    assert.deepEqual(fs.readFileSync(path.join(snapshot, 'protected-0.bin')), f.request.protected[0].bytes);
    assert.deepEqual(result.createdSha256, f.request.records.map(item => digest(item.bytes)));
    const output = JSON.stringify(result);
    assert.equal(output.includes(f.profileRoot), false);
    assert.equal(output.includes('SYNTHETIC_PROTECTED_CONFIG'), false);
    verifyProtected(f);
  }));
}
for (const count of [0, 201]) test('reject_record_bound_' + count, () => withFixture(1, f => {
  f.request.records = Array.from({ length: count }, (_, index) => record(index));
  requireRefused(() => transaction(f.request), 'invalid_records');
  assert.equal(fs.readdirSync(f.profileRoot).length, 2);
}));
test('duplicate_names_refused_without_writes', () => withFixture(1, f => {
  f.request.records.push({ ...f.request.records[0], bytes: Buffer.from('SYNTHETIC_DIFFERENT') });
  requireRefused(() => transaction(f.request), 'duplicate_record_name');
  assert.equal(targets(f).length, 0);
}));
for (const [label, name] of Object.entries({ traversal: '../local_x.json', slash: 'a/local_x.json',
  backslash: 'a\\local_x.json', wrongPrefix: 'remote_00000000-0000-4000-8000-000000000001.json',
  badUuid: 'local_invalid.json', uppercase: 'local_A0000000-0000-4000-8000-000000000001.json',
  suffix: 'local_00000000-0000-4000-8000-000000000001.json.bak', control: 'local_\x00.json' })) {
  test('reject_bad_name_' + label, () => withFixture(1, f => {
    f.request.records[0].name = name;
    requireRefused(() => transaction(f.request), 'invalid_record_name');
    assert.equal(targets(f).length, 0);
  }));
}
for (const [label, value] of Object.entries({ nonBuffer: 'SYNTHETIC', oversized: Buffer.alloc(2000001), null: null })) {
  test('reject_invalid_bytes_' + label, () => withFixture(1, f => {
    f.request.records[0].bytes = value;
    requireRefused(() => transaction(f.request), 'invalid_records');
  }));
}
test('invalid_protected_traversal_refused', () => withFixture(1, f => {
  f.request.protected = [{ name: '../foreign', bytes: null }];
  requireRefused(() => transaction(f.request), 'invalid_protected');
}));
test('duplicate_protected_refused', () => withFixture(1, f => {
  f.request.protected.push(f.request.protected[0]);
  requireRefused(() => transaction(f.request), 'duplicate_protected_name');
}));
test('existing_target_and_backup_preserved', () => withFixture(1, f => {
  const target = path.join(f.registryRoot, f.request.records[0].name);
  const foreign = Buffer.from('SYNTHETIC_FOREIGN_EXISTING');
  fs.writeFileSync(target, foreign);
  fs.writeFileSync(target + '.bak', 'SYNTHETIC_BACKUP');
  const result = transaction(f.request);
  assert.equal(result.reason, 'target_already_exists');
  assert.deepEqual(fs.readFileSync(target), foreign);
  assert.equal(fs.readFileSync(target + '.bak', 'utf8'), 'SYNTHETIC_BACKUP');
  assert.equal(result.snapshotDirectory, null);
}));
test('existing_target_symlink_preserved', () => withFixture(1, f => {
  const outside = path.join(f.base, 'foreign');
  fs.writeFileSync(outside, 'SYNTHETIC_FOREIGN');
  const target = path.join(f.registryRoot, f.request.records[0].name);
  fs.symlinkSync(outside, target);
  const result = transaction(f.request);
  assert.equal(result.reason, 'target_already_exists');
  assert.equal(fs.lstatSync(target).isSymbolicLink(), true);
  assert.equal(fs.readFileSync(outside, 'utf8'), 'SYNTHETIC_FOREIGN');
}));
test('registry_symlink_refused', () => withFixture(1, f => {
  const renamed = path.join(f.profileRoot, 'real-registry');
  fs.renameSync(f.registryRoot, renamed);
  fs.symlinkSync(renamed, f.registryRoot);
  requireRefused(() => transaction(f.request), 'symlink_directory');
}));
test('ancestor_symlink_refused', () => withFixture(1, f => {
  const alias = path.join(f.base, 'alias');
  fs.symlinkSync(f.profileRoot, alias);
  f.request.profileRoot = alias;
  f.request.registryRoot = path.join(alias, 'registry');
  requireRefused(() => transaction(f.request), 'symlink_directory');
}));
test('protected_byte_CAS_refuses_without_restoring', () => withFixture(1, f => {
  fs.writeFileSync(path.join(f.profileRoot, 'config.json'), 'SYNTHETIC_NATIVE_CHANGED');
  const result = transaction(f.request);
  assert.equal(result.reason, 'protected_metadata_changed');
  assert.equal(targets(f).length, 0);
  assert.equal(fs.readFileSync(path.join(f.profileRoot, 'config.json'), 'utf8'), 'SYNTHETIC_NATIVE_CHANGED');
}));
test('protected_absence_CAS_refuses', () => withFixture(1, f => {
  fs.writeFileSync(path.join(f.profileRoot, 'absent.json'), 'SYNTHETIC_NEW_NATIVE');
  const result = transaction(f.request);
  assert.equal(result.reason, 'protected_metadata_changed');
  assert.equal(fs.readFileSync(path.join(f.profileRoot, 'absent.json'), 'utf8'), 'SYNTHETIC_NEW_NATIVE');
}));
test('protected_symlink_refuses', () => withFixture(1, f => {
  fs.unlinkSync(path.join(f.profileRoot, 'config.json'));
  fs.symlinkSync(path.join(f.base, 'foreign'), path.join(f.profileRoot, 'config.json'));
  const result = transaction(f.request);
  assert.equal(result.reason, 'unsafe_file');
  assert.equal(fs.lstatSync(path.join(f.profileRoot, 'config.json')).isSymbolicLink(), true);
}));
for (const [label, guard] of Object.entries({ false: () => false, unknown: () => undefined, error: () => { throw new Error('SYNTHETIC_PRIVATE_EXCEPTION'); } })) {
  test('closed_guard_' + label + '_refuses', () => withFixture(1, f => {
    f.request.closedGuard = guard;
    const result = transaction(f.request);
    assert.equal(result.reason, 'closed_guard_unknown');
    assert.equal(result.snapshotDirectory, null);
    assert.equal(JSON.stringify(result).includes('SYNTHETIC_PRIVATE_EXCEPTION'), false);
  }));
}
test('closed_guard_each_mutation_blocks_creation', () => withFixture(1, f => {
  let closed = true;
  f.request.closedGuard = () => closed;
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'beforeMutation' && context.kind === 'linkCreateOnly') closed = false;
  } });
  assert.equal(result.reason, 'closed_guard_unknown');
  assert.equal(targets(f).length, 0);
  assert.equal(result.ownedFilesRolledBack, false);
  assert.equal(stages(f).length, 1, 'Cleanup also obeys closed guard');
}));
test('mutation_checks_include_cleanup_and_writes', () => withFixture(1, f => {
  let guards = 0;
  const mutations = [];
  f.request.closedGuard = () => { guards++; return true; };
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'beforeMutation') mutations.push(context.kind);
  } });
  assert.equal(result.status, 'created_metadata');
  assert.equal(mutations.includes('writePrivateContent'), true);
  assert.equal(mutations.includes('cleanupOwnedStage'), true);
  assert.equal(guards >= mutations.length, true);
}));
test('staging_inode_replacement_is_preserved', () => withFixture(1, f => {
  let replacement;
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'staged') {
      fs.unlinkSync(context.path);
      fs.writeFileSync(context.path, 'SYNTHETIC_FOREIGN_STAGE');
      replacement = context.path;
    }
  } });
  assert.equal(result.reason, 'staging_replaced');
  assert.equal(targets(f).length, 0);
  assert.equal(fs.readFileSync(replacement, 'utf8'), 'SYNTHETIC_FOREIGN_STAGE');
  assert.equal(result.ownedFilesRolledBack, false);
}));
test('staging_same_inode_changed_bytes_preserved', () => withFixture(1, f => {
  let replacement;
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'staged') { fs.writeFileSync(context.path, 'SYNTHETIC_CHANGED_STAGE'); replacement = context.path; }
  } });
  assert.equal(result.reason, 'staging_replaced');
  assert.equal(fs.readFileSync(replacement, 'utf8'), 'SYNTHETIC_CHANGED_STAGE');
}));
test('destination_foreign_inode_replacement_preserved', () => withFixture(1, f => {
  let replacement;
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'created') { fs.unlinkSync(context.path); fs.writeFileSync(context.path, 'SYNTHETIC_FOREIGN_TARGET', { mode: 0o600 }); replacement = context.path; }
  } });
  assert.equal(result.reason, 'created_record_changed');
  assert.equal(fs.readFileSync(replacement, 'utf8'), 'SYNTHETIC_FOREIGN_TARGET');
  assert.equal(result.ownedFilesRolledBack, false);
  assert.equal(stages(f).length, 0, 'Still-owned stage is rolled back independently');
}));
test('destination_same_inode_changed_bytes_preserved', () => withFixture(1, f => {
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'created') fs.writeFileSync(context.path, 'SYNTHETIC_NATIVE_EDIT');
  } });
  assert.equal(result.reason, 'created_record_changed');
  assert.equal(fs.readFileSync(path.join(f.registryRoot, f.request.records[0].name), 'utf8'), 'SYNTHETIC_NATIVE_EDIT');
  assert.equal(result.ownedFilesRolledBack, false);
}));
test('root_path_replacement_preserves_both_trees', () => withFixture(1, f => {
  const moved = path.join(f.profileRoot, 'moved-registry');
  const result = transaction(f.request, { onPhase(phase) {
    if (phase === 'staged') {
      fs.renameSync(f.registryRoot, moved);
      fs.mkdirSync(f.registryRoot, { mode: 0o700 });
      fs.writeFileSync(path.join(f.registryRoot, 'foreign.txt'), 'SYNTHETIC_FOREIGN_ROOT');
    }
  } });
  assert.equal(result.reason, 'directory_identity_changed');
  assert.equal(result.ownedFilesRolledBack, false);
  assert.equal(fs.readFileSync(path.join(f.registryRoot, 'foreign.txt'), 'utf8'), 'SYNTHETIC_FOREIGN_ROOT');
  assert.equal(fs.readdirSync(moved).filter(name => name.startsWith('.history-index-stage-')).length, 1);
}));
test('snapshot_path_replacement_blocks_private_content', () => withFixture(1, f => {
  let swapped;
  const result = transaction(f.request, { onPhase(phase, context) {
    if (!swapped && phase === 'beforeMutation' && context.kind === 'exclusiveCreate' && context.path.includes('protected-0.bin')) {
      const snapshot = path.dirname(context.path);
      fs.renameSync(snapshot, snapshot + '-original');
      fs.mkdirSync(snapshot, { mode: 0o700 });
      swapped = snapshot;
    }
  } });
  assert.equal(result.reason, 'snapshot_identity_changed');
  assert.equal(result.snapshotBindingVerified, false);
  assert.equal(result.privateSnapshotsPreserved, false);
  assert.equal(fs.readdirSync(swapped).length, 0);
  assert.equal(targets(f).length, 0);
}));
test('atomic_link_collision_does_not_overwrite', () => withFixture(1, f => {
  const result = transaction(f.request, { linkSync(stage, target) {
    fs.writeFileSync(target, 'SYNTHETIC_CONCURRENT_FOREIGN');
    fs.linkSync(stage, target);
  } });
  assert.equal(result.reason, 'target_already_exists');
  assert.equal(fs.readFileSync(path.join(f.registryRoot, f.request.records[0].name), 'utf8'), 'SYNTHETIC_CONCURRENT_FOREIGN');
  assert.equal(stages(f).length, 0);
}));
test('link_failure_rolls_back_owned_files_and_preserves_snapshots', () => withFixture(3, f => {
  const result = transaction(f.request, { linkSync(stage, target, index) {
    if (index === 1) { const error = new Error('SYNTHETIC_LINK_FAIL'); error.code = 'EIO'; throw error; }
    fs.linkSync(stage, target);
  } });
  assert.equal(result.status, 'refused_rolled_back');
  assert.equal(result.createdCountBeforeRefusal, 1);
  assert.equal(targets(f).length, 0);
  assert.equal(stages(f).length, 0);
  assert.equal(result.privateSnapshotsPreserved, true);
  assert.equal(fs.existsSync(path.join(f.profileRoot, result.snapshotDirectory, 'protected-0.bin')), true);
  verifyProtected(f);
}));
test('partial_rollback_removes_only_unchanged_owned_entries', () => withFixture(3, f => {
  const first = path.join(f.registryRoot, f.request.records[0].name);
  const result = transaction(f.request, { linkSync(stage, target, index) {
    if (index === 2) {
      fs.unlinkSync(first); fs.writeFileSync(first, 'SYNTHETIC_FOREIGN_REPLACEMENT');
      throw new Error('SYNTHETIC_FAIL');
    }
    fs.linkSync(stage, target);
  } });
  assert.equal(result.status, 'refused_replacement_preserved');
  assert.equal(result.createdCountBeforeRefusal, 2);
  assert.equal(fs.readFileSync(first, 'utf8'), 'SYNTHETIC_FOREIGN_REPLACEMENT');
  assert.equal(fs.existsSync(path.join(f.registryRoot, f.request.records[1].name)), false);
  assert.equal(stages(f).length, 0);
  assert.equal(result.ownedFilesRolledBack, false);
}));
test('source_and_protected_buffers_copied_at_entry', () => withFixture(1, f => {
  const expected = Buffer.from(f.request.records[0].bytes);
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'beforeMutation' && context.kind === 'createSnapshot') f.request.records[0].bytes.fill(0);
  } });
  assert.equal(result.status, 'created_metadata');
  assert.deepEqual(fs.readFileSync(path.join(f.registryRoot, f.request.records[0].name)), expected);
}));
test('windows_requires_injected_inherited_acl_validation', () => withFixture(1, f => {
  f.request.platform = 'win32';
  requireRefused(() => transaction(f.request), 'private_storage_unverified');
  assert.equal(fs.readdirSync(f.profileRoot).length, 2);
}));
test('windows_unknown_storage_guard_refuses', () => withFixture(1, f => {
  f.request.platform = 'win32';
  f.request.privateStorageGuard = () => undefined;
  const result = transaction(f.request);
  assert.equal(result.reason, 'private_storage_unverified');
  assert.equal(result.snapshotDirectory, null);
}));
test('windows_snapshot_privacy_proven_before_contents_no_chmod_claim', () => withFixture(1, f => {
  f.request.platform = 'win32';
  const validations = [];
  const mutations = [];
  f.request.privateStorageGuard = context => {
    validations.push(context);
    if (context.kind === 'directory' && context.phase === 'beforePrivateContent') assert.equal(fs.readdirSync(context.path).length, 0);
    if (context.kind === 'file' && context.phase === 'beforePrivateContent') assert.equal(fs.statSync(context.path).size, 0);
    return true;
  };
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'beforeMutation') mutations.push(context.kind);
  } });
  assert.equal(result.status, 'created_metadata');
  assert.equal(result.snapshotPrivacy, 'inherited_acl_guard_verified');
  assert.equal(result.snapshotPrivacy.includes('0700'), false);
  assert.equal(mutations.includes('privateFileMode'), false);
  assert.equal(validations.filter(item => item.phase === 'existingInheritedAcl').length, 2);
}));
test('windows_snapshot_guard_refusal_prevents_snapshot_bytes', () => withFixture(1, f => {
  f.request.platform = 'win32';
  f.request.privateStorageGuard = context => context.phase !== 'beforePrivateContent';
  const result = transaction(f.request);
  assert.equal(result.reason, 'private_storage_unverified');
  assert.equal(fs.readdirSync(path.join(f.profileRoot, result.snapshotDirectory)).length, 0);
  assert.equal(targets(f).length, 0);
}));
test('nonzero_inode_identity_required', () => withFixture(1, f => {
  const original = fs.lstatSync;
  fs.lstatSync = function(file, options) {
    const result = original(file, options);
    if (file === f.profileRoot && options && options.bigint) result.ino = 0n;
    return result;
  };
  try { requireRefused(() => transaction(f.request), 'nonzero_identity_required'); }
  finally { fs.lstatSync = original; }
  assert.equal(targets(f).length, 0);
}));
test('nonzero_device_identity_required', () => withFixture(1, f => {
  const original = fs.lstatSync;
  fs.lstatSync = function(file, options) {
    const result = original(file, options);
    if (file === f.registryRoot && options && options.bigint) result.dev = 0n;
    return result;
  };
  try { requireRefused(() => transaction(f.request), 'nonzero_identity_required'); }
  finally { fs.lstatSync = original; }
  assert.equal(targets(f).length, 0);
}));
test('snapshot_mode_change_blocks_private_content', () => withFixture(1, f => {
  let changed = false;
  const result = transaction(f.request, { onPhase(phase, context) {
    if (!changed && phase === 'beforeMutation' && context.kind === 'writePrivateContent') {
      fs.chmodSync(path.dirname(context.path), 0o755); changed = true;
    }
  } });
  assert.equal(result.reason, 'private_mode_unverified');
  assert.equal(fs.statSync(path.join(f.profileRoot, result.snapshotDirectory, 'protected-0.bin')).size, 0);
  assert.equal(targets(f).length, 0);
}));
test('file_mode_change_immediately_before_private_write_blocks_content', () => withFixture(1, f => {
  let changed = false;
  const result = transaction(f.request, { onPhase(phase, context) {
    if (!changed && phase === 'beforeMutation' && context.kind === 'writePrivateContent') {
      fs.chmodSync(context.path, 0o644); changed = true;
    }
  } });
  assert.equal(result.reason, 'private_mode_unverified');
  assert.equal(fs.statSync(path.join(f.profileRoot, result.snapshotDirectory, 'protected-0.bin')).size, 0);
  assert.equal(targets(f).length, 0);
}));
test('stage_mode_change_refuses_publication', () => withFixture(1, f => {
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'staged') fs.chmodSync(context.path, 0o644);
  } });
  assert.equal(result.reason, 'private_mode_unverified');
  assert.equal(targets(f).length, 0);
}));
test('committed_file_mode_change_refuses_success', () => withFixture(1, f => {
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'created') fs.chmodSync(context.path, 0o644);
  } });
  assert.equal(result.reason, 'private_mode_unverified');
  assert.equal(result.status, 'refused_rolled_back');
  assert.equal(targets(f).length, 0);
}));
test('windows_acl_change_immediately_before_private_write_blocks_content', () => withFixture(1, f => {
  f.request.platform = 'win32';
  let privateStorage = true;
  f.request.privateStorageGuard = () => privateStorage;
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'beforeMutation' && context.kind === 'writePrivateContent') privateStorage = false;
  } });
  assert.equal(result.reason, 'private_storage_unverified');
  assert.equal(fs.statSync(path.join(f.profileRoot, result.snapshotDirectory, 'protected-0.bin')).size, 0);
  assert.equal(targets(f).length, 0);
}));
test('protected_buffer_copied_at_entry', () => withFixture(1, f => {
  const expected = Buffer.from(f.request.protected[0].bytes);
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'beforeMutation' && context.kind === 'createSnapshot') f.request.protected[0].bytes.fill(0);
  } });
  assert.equal(result.status, 'created_metadata');
  assert.deepEqual(fs.readFileSync(path.join(f.profileRoot, 'config.json')), expected);
  assert.deepEqual(fs.readFileSync(path.join(f.profileRoot, result.snapshotDirectory, 'protected-0.bin')), expected);
}));
test('protected_CAS_change_during_staging_preserved', () => withFixture(1, f => {
  const result = transaction(f.request, { onPhase(phase) {
    if (phase === 'staged') fs.writeFileSync(path.join(f.profileRoot, 'config.json'), 'SYNTHETIC_NATIVE_CHANGED_DURING_COPY');
  } });
  assert.equal(result.reason, 'protected_metadata_changed');
  assert.equal(fs.readFileSync(path.join(f.profileRoot, 'config.json'), 'utf8'), 'SYNTHETIC_NATIVE_CHANGED_DURING_COPY');
  assert.equal(fs.readFileSync(path.join(f.profileRoot, result.snapshotDirectory, 'protected-0.bin'), 'utf8'), 'SYNTHETIC_PROTECTED_CONFIG');
  assert.equal(targets(f).length, 0);
  assert.equal(stages(f).length, 1, 'Unknown current protected state also blocks cleanup');
}));
test('partial_stage_write_failure_tracks_and_preserves_incomplete_owned_file', () => withFixture(1, f => {
  const original = fs.writeSync;
  let broken = false;
  let stagePath;
  try {
    const result = transaction(f.request, { onPhase(phase, context) {
      if (!broken && phase === 'beforeMutation' && context.kind === 'writePrivateContent' && context.path.includes('.history-index-stage-')) {
        broken = true; stagePath = context.path;
        fs.writeSync = () => { throw new Error('SYNTHETIC_WRITE_FAILURE'); };
      }
    } });
    assert.equal(result.status, 'refused_replacement_preserved');
    assert.equal(fs.statSync(stagePath).size, 0);
    assert.equal(targets(f).length, 0);
  } finally { fs.writeSync = original; }
}));
test('windows_validated_acl_callback_captured_at_entry', () => withFixture(1, f => {
  f.request.platform = 'win32';
  let calls = 0;
  f.request.privateStorageGuard = () => { calls++; return true; };
  const result = transaction(f.request, { onPhase(phase, context) {
    if (phase === 'beforeMutation' && context.kind === 'createSnapshot') delete f.request.privateStorageGuard;
  } });
  assert.equal(result.status, 'created_metadata');
  assert.equal(calls > 10, true);
}));
test('snapshot_replaced_during_rollback_has_current_binding_unknown_and_foreign_preserved', () => withFixture(3, f => {
  let originalSnapshot;
  let foreignSnapshot;
  let replaced = false;
  const result = transaction(f.request, {
    linkSync(stage, target, index) {
      if (index === 1) throw new Error('SYNTHETIC_TRIGGER_ROLLBACK');
      fs.linkSync(stage, target);
    },
    onPhase(phase, context) {
      if (!replaced && phase === 'beforeMutation' && context.kind === 'rollbackOwnedFile') {
        foreignSnapshot = fs.readdirSync(f.profileRoot).find(name => name.startsWith('.history-index-snapshot-'));
        const current = path.join(f.profileRoot, foreignSnapshot);
        originalSnapshot = current + '-preserved-original';
        fs.renameSync(current, originalSnapshot);
        fs.mkdirSync(current, { mode: 0o700 });
        fs.writeFileSync(path.join(current, 'foreign-marker.json'), 'SYNTHETIC_FOREIGN_SNAPSHOT', { mode: 0o600 });
        replaced = true;
      }
    },
  });
  assert.equal(replaced, true);
  assert.equal(result.status, 'refused_replacement_preserved');
  assert.equal(result.snapshotCreated, true);
  assert.equal(result.snapshotBindingVerified, false);
  assert.equal(result.privateSnapshotsPreserved, false);
  assert.equal(result.snapshotPreservationDisposition, 'snapshot_not_removed_current_binding_unknown');
  assert.equal(fs.readFileSync(path.join(f.profileRoot, foreignSnapshot, 'foreign-marker.json'), 'utf8'), 'SYNTHETIC_FOREIGN_SNAPSHOT');
  assert.deepEqual(fs.readdirSync(path.join(f.profileRoot, foreignSnapshot)), ['foreign-marker.json']);
  assert.deepEqual(fs.readFileSync(path.join(originalSnapshot, 'protected-0.bin')), f.request.protected[0].bytes);
  assert.equal(targets(f).length, 1, 'Snapshot mismatch blocks destructive cleanup through uncertain roots');
  assert.equal(stages(f).length, 1);
  verifyProtected(f);
}));
function run() {
  const results = [];
  for (const item of cases) {
    const begin = Date.now();
    try { item.run(); results.push({ name: item.name, status: 'PASS', elapsedMs: Date.now() - begin }); }
    catch (error) { results.push({ name: item.name, status: 'FAIL', error: String(error.stack), elapsedMs: Date.now() - begin }); }
  }
  return { testCount: cases.length, passed: results.filter(item => item.status === 'PASS').length,
    failed: results.filter(item => item.status === 'FAIL').length, results };
}
if (require.main === module) {
  const result = run();
  console.log(JSON.stringify(result, null, 2));
  process.exitCode = result.failed === 0 ? 0 : 1;
}
module.exports = { run };
