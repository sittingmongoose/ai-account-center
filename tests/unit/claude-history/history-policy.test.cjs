'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const sync = require('../../../scripts/claude-history/history-index-sync.cjs');
const fx = require('./synthetic-history-fixtures.cjs');
const otherPlatform = value => value === 'mac' ? 'windows' : 'mac';

function privatePolicy(seed, role = 'platyr', targetPlatform = 'windows') {
  const identity = seed.profiles[role];
  return {
    version: 1, enabled: true, sourcePlatform: otherPlatform(targetPlatform),
    identity: {accountUuid: identity.accountUuid, organizationUuid: identity.organizationUuid},
    project: {cwd: seed.project, originCwd: seed.project, transcriptRoot: seed.transcriptRoot},
    ssh: Object.fromEntries(['mac', 'windows'].map(platform => [platform, {
      alias: seed.aliases[platform], ...seed.plainEndpoint,
    }])),
  };
}

function snapshot(seed, role, platform, records = [], changes = {}) {
  const identity = seed.profiles[role];
  return {
    profileId: role, platform,
    identity: {accountSha256: identity.accountSha256, orgSha256: identity.orgSha256},
    records, snapshotStable: true, revision: sync.snapshotRevision(records),
    endpoint: {...seed.endpoint},
    nativeGuard: {...sync.NATIVE[platform], warmGuardVerified: true, autoResumeGuardVerified: true},
    noPendingInput: true, noScheduledWork: true, protectedSnapshotStable: true,
    ...changes,
  };
}

function setup({role = 'platyr', targetPlatform = 'windows', tag = 'A', adapters: overrides = {}} = {}) {
  const seed = fx.bindings(tag), sourcePlatform = otherPlatform(targetPlatform);
  const source = snapshot(seed, role, sourcePlatform, [fx.envelope(fx.record(seed, sourcePlatform))]);
  const target = snapshot(seed, role, targetPlatform);
  const policy = privatePolicy(seed, role, targetPlatform);
  const events = [];
  let received;
  const adapters = {
    async isTargetClosed() {events.push('closed'); return true;},
    async readTarget() {events.push('target'); return target;},
    async readSource() {events.push('source'); return source;},
    async verifyTranscriptReferences() {events.push('stat-references'); return true;},
    async targetProtectedStateUnchanged() {events.push('protected'); return true;},
    async appendCreateOnly(payload) {
      events.push('append'); received = payload; await payload.closedGuard();
      return {status: 'created_metadata', createdCount: payload.records.length, protectedBytesUnchanged: true};
    },
    ...overrides,
  };
  return {seed, source, target, policy, events, adapters, received: () => received,
    run: (changes = {}) => sync.synchronizeBeforeProfileOpen({profileId: role, targetPlatform, policy, adapters, ...changes})};
}

function replaceRows(snapshotValue, rows) {
  snapshotValue.records = rows.map(fx.envelope);
  snapshotValue.revision = sync.snapshotRevision(snapshotValue.records);
}

for(const {role, targetPlatform} of [{role: 'platyr', targetPlatform: 'windows'}, {role: 'gmail', targetPlatform: 'mac'}]) {
  for(const tag of ['A', 'B']) test(`injected private policy ${tag}: ${role} -> ${targetPlatform}`, async () => {
    const f = setup({role, targetPlatform, tag}), out = await f.run();
    assert.equal(out.status, 'synchronized');
    assert.equal(out.createdCount, 1);
    const value = JSON.parse(f.received().records[0].bytes);
    assert.equal(value.sshConfig.sshHost, f.seed.aliases[targetPlatform]);
    assert.equal(value.cwd, f.seed.project);
    assert.equal(value.sshRemoteTranscriptPath, fx.record(f.seed, otherPlatform(targetPlatform)).sshRemoteTranscriptPath);
    assert.equal(out.resumedSessions, 0); assert.equal(out.copiedTranscripts, 0);
    assert.equal(JSON.stringify(out).includes(fx.PRIVATE_TITLE), false);
    assert.equal(JSON.stringify(out).includes(value.cliSessionId), false);
  });
}

for(const role of fx.ROLES) for(const targetPlatform of ['mac', 'windows']) {
  const allowed = role === 'platyr' && targetPlatform === 'windows' || role === 'gmail' && targetPlatform === 'mac';
  if(allowed) continue;
  test(`recognized ${role}/${targetPlatform} preserves disabled direction before reads`, async () => {
    const f = setup({role, targetPlatform});
    replaceRows(f.target, [fx.record(f.seed, targetPlatform, {title: 'SYNTHETIC_EXISTING_NATIVE', isRunning: true, resumeConfirmed: true})]);
    const preserved = Buffer.from(f.target.records[0].bytes), out = await f.run();
    assert.equal(out.status, 'skipped'); assert.equal(out.reason, 'unsupported_direction');
    assert.deepEqual(f.events, []); assert.equal(f.received(), undefined);
    assert.equal(out.createdCount, 0);
    assert.deepEqual(f.target.records[0].bytes, preserved);
  });
}

test('unsupported profile and platform stay distinct from the four recognized profiles', async () => {
  for(const args of [{profileId: 'unknown', targetPlatform: 'windows'}, {profileId: 'platyr', targetPlatform: 'linux'}, {profileId: null, targetPlatform: 'mac'}]) {
    const f = setup(), out = await f.run(args);
    assert.equal(out.status, 'skipped'); assert.equal(out.reason, 'unsupported_profile_or_platform');
    assert.deepEqual(f.events, []);
  }
});
test('private policy requires canonical lowercase UUIDs before transport', async () => {
  const f = setup();
  f.policy.identity.accountUuid = f.policy.identity.accountUuid.toUpperCase();
  f.policy.identity.organizationUuid = f.policy.identity.organizationUuid.toUpperCase();
  assert.equal((await f.run()).reason, 'history_policy_unverified');
  assert.deepEqual(f.events, []); assert.equal(f.received(), undefined);
});

for(const change of [
  p => undefined,
  p => ({...p, version: 2}),
  p => ({...p, enabled: false}),
  p => ({...p, sourcePlatform: 'windows'}),
  p => ({...p, identity: {accountUuid: 'invalid', organizationUuid: p.identity.organizationUuid}}),
  p => ({...p, identity: {accountUuid: p.identity.accountUuid, organizationUuid: 'invalid'}}),
  p => ({...p, project: {...p.project, cwd: '/SYNTHETIC_OTHER_PROJECT'}}),
  p => ({...p, project: {...p.project, originCwd: '/SYNTHETIC_OTHER_PROJECT'}}),
  p => ({...p, project: {...p.project, transcriptRoot: '/SYNTHETIC/../projects'}}),
  p => ({...p, ssh: {...p.ssh, windows: {...p.ssh.windows, hostname: 'different-endpoint'}}}),
  p => ({...p, ssh: {...p.ssh, windows: {...p.ssh.windows, username: 'different-user'}}}),
  p => ({...p, ssh: {...p.ssh, windows: {...p.ssh.windows, port: 0}}}),
]) test(`invalid private policy ${change.toString()} performs no reads`, async () => {
  const f = setup(), out = await f.run({policy: change(f.policy)});
  assert.equal(out.reason, 'history_policy_unverified');
  assert.deepEqual(f.events, []); assert.equal(f.received(), undefined);
});

for(const which of ['source', 'target']) for(const key of ['accountSha256', 'orgSha256']) {
  test(`${which} manifest exact ${key} binding`, async () => {
    const f = setup(); f[which].identity[key] = '0'.repeat(64);
    const out = await f.run(); assert.equal(out.reason, 'identity_mismatch'); assert.equal(f.received(), undefined);
  });
}
for(const which of ['source', 'target']) for(const key of ['hostnameSha256', 'usernameSha256', 'portSha256']) {
  test(`${which} manifest exact endpoint ${key}`, async () => {
    const f = setup(); f[which].endpoint[key] = '0'.repeat(64);
    assert.equal((await f.run()).reason, 'endpoint_unverified'); assert.equal(f.received(), undefined);
  });
}
test('manifest cannot be rebound to another recognized role sharing the request', async () => {
  const f = setup(); f.source.profileId = 'gmail';
  assert.equal((await f.run()).reason, 'identity_mismatch'); assert.equal(f.received(), undefined);
});

test('already-open destination skips before registry or peer reads', async () => {
  const f = setup({adapters: {async isTargetClosed() {return false;}}});
  assert.equal((await f.run()).reason, 'profile_already_open_or_unknown');
  assert.deepEqual(f.events, []); assert.equal(f.received(), undefined);
});
for(const key of ['noPendingInput', 'noScheduledWork', 'protectedSnapshotStable']) {
  for(const value of [false, undefined]) test(`destination ${key}=${value} skips before peer read`, async () => {
    const f = setup(); f.target[key] = value;
    assert.equal((await f.run()).reason, 'destination_work_or_state_unknown');
    assert.equal(f.events.includes('source'), false); assert.equal(f.received(), undefined);
  });
}
test('waiting-directory presence alone leaves queue state unknown', async () => {
  const f = setup(); f.target.waitingDirectoryPresent = true; delete f.target.noPendingInput;
  assert.equal((await f.run()).reason, 'destination_work_or_state_unknown');
  assert.equal(f.events.includes('source'), false); assert.equal(f.received(), undefined);
});
test('present waiting directory with separately verified empty queue is not a queued-work refusal', async () => {
  const f = setup(); f.target.waitingDirectoryPresent = true; f.target.noPendingInput = true;
  assert.equal((await f.run()).status, 'synchronized');
});

for(const which of ['source', 'target']) for(const [key, value] of [
  ['version', 'unknown'], ['managerSha256', '0'.repeat(64)], ['warmGuardVerified', false], ['autoResumeGuardVerified', false],
]) test(`${which} exact native ${key} pin mismatch skips`, async () => {
  const f = setup(); f[which].nativeGuard[key] = value;
  assert.equal((await f.run()).reason, 'native_guard_unverified'); assert.equal(f.received(), undefined);
});

test('new index stays cold and neutral while keeping distinct original desktop and CLI IDs', async () => {
  const f = setup(), original = fx.record(f.seed, 'mac', {
    sshRemoteProcessId: 'SYNTHETIC_PROCESS', sshReattach: {state: 'adoptable', processId: 'SYNTHETIC_PROCESS'},
    query: {}, isRunning: true, armedWorkAtQuit: {}, interruptedByQuitAt: 1,
    pendingMessages: ['SYNTHETIC_INPUT'], pendingFirstStart: {task: 'SYNTHETIC_TASK'},
    scheduledTaskId: 'SYNTHETIC_TASK', resumeConfirmed: true, importedFrom: 'untrusted-source',
    permissionMode: 'bypassPermissions', remoteControlAutoEligible: true,
    unknown: fx.PRIVATE_ERROR,
  });
  replaceRows(f.source, [original]);
  const out = await f.run(), value = JSON.parse(f.received().records[0].bytes);
  assert.equal(out.status, 'synchronized');
  assert.equal(value.sessionId, original.sessionId); assert.equal(value.cliSessionId, original.cliSessionId);
  assert.notEqual(value.sessionId.slice(6), value.cliSessionId);
  assert.equal(value.importedFrom, 'local-1p-code'); assert.equal(value.resumeConfirmed, false);
  assert.equal(value.permissionMode, 'default'); assert.equal(value.remoteControlAutoEligible, false);
  for(const key of ['sshRemoteProcessId', 'sshReattach', 'query', 'isRunning', 'armedWorkAtQuit', 'interruptedByQuitAt',
    'pendingMessages', 'pendingFirstStart', 'scheduledTaskId', 'unknown']) assert.equal(key in value, false, key);
  assert.equal(value.sshRemoteTranscriptPath, original.sshRemoteTranscriptPath);
  assert.equal(out.resumedSessions, 0); assert.equal(out.copiedTranscripts, 0);
});

test('existing ID binding preserves destination runtime and mutable state byte-for-byte', async () => {
  const f = setup(), existing = fx.record(f.seed, 'windows', {
    title: 'SYNTHETIC_NATIVE_EDIT', isArchived: true, permissionMode: 'bypassPermissions',
    resumeConfirmed: true, pendingMessages: ['SYNTHETIC_NATIVE_PENDING'], isRunning: true,
    sshRemoteProcessId: 'SYNTHETIC_NATIVE_PID', sshReattach: {state: 'adoptable'},
  });
  replaceRows(f.target, [existing]);
  const preserved = Buffer.from(f.target.records[0].bytes);
  const missing = fx.record(f.seed, 'mac', {}, 2);
  replaceRows(f.source, [fx.record(f.seed), missing]);
  const out = await f.run();
  assert.equal(out.createdCount, 1); assert.equal(out.alreadyPresentCount, 1);
  assert.deepEqual(f.target.records[0].bytes, preserved);
  assert.equal(f.received().records.length, 1);
  assert.equal(JSON.parse(f.received().records[0].bytes).sessionId, missing.sessionId);
});
test('existing-only destination binding performs no append', async () => {
  const f = setup(); replaceRows(f.target, [fx.record(f.seed, 'windows')]);
  const out = await f.run();
  assert.equal(out.status, 'unchanged'); assert.equal(out.alreadyPresentCount, 1); assert.equal(f.received(), undefined);
});
for(const changes of [
  {cliSessionId: fx.uuid(99)}, {sessionId: 'local_' + fx.uuid(99)},
  {cwd: '/SYNTHETIC_OTHER'}, {originCwd: '/SYNTHETIC_OTHER'},
  {sshConfig: {sshHost: 'SYNTHETIC_OTHER_ALIAS'}}, {sshRemoteTranscriptPath: '/SYNTHETIC_OTHER_REFERENCE'},
]) test(`existing immutable collision ${Object.keys(changes)} refuses append`, async () => {
  const f = setup(); replaceRows(f.target, [fx.record(f.seed, 'windows', changes)]);
  assert.equal((await f.run()).reason, 'identity_collision'); assert.equal(f.received(), undefined);
});

for(const changes of [{cwd: '/SYNTHETIC_OTHER'}, {originCwd: '/SYNTHETIC_OTHER'}, {sshConfig: undefined},
  {wslConfig: {distro: 'SYNTHETIC_DISTRO'}}, {backend: {kind: 'local'}}]) {
  test(`record outside verified project/SSH mapping ${Object.keys(changes)} is not imported`, async () => {
    const f = setup(); replaceRows(f.source, [fx.record(f.seed, 'mac', changes)]);
    const out = await f.run(); assert.equal(out.status, 'unchanged'); assert.equal(out.createdCount, 0);
    assert.equal(f.received(), undefined);
  });
}
test('transcript prefix and basename must bind to policy root and original CLI ID', async () => {
  for(const path of ['/SYNTHETIC_OTHER_ROOT/'+fx.uuid(2)+'.jsonl',
    '/synthetic-user/.claude/projects/../'+fx.uuid(2)+'.jsonl',
    '/synthetic-user/.claude/projects/-project/'+fx.uuid(99)+'.jsonl']) {
    const f = setup(); replaceRows(f.source, [fx.record(f.seed, 'mac', {sshRemoteTranscriptPath:path})]);
    assert.equal((await f.run()).status, 'unchanged'); assert.equal(f.received(), undefined);
  }
});

test('source changed between reads skips without retry or append', async () => {
  const f = setup(); let reads = 0;
  f.adapters.readSource = async () => ++reads === 1 ? f.source : snapshot(f.seed, 'platyr', 'mac', [fx.envelope(fx.record(f.seed, 'mac', {lastActivityAt: 3}))]);
  assert.equal((await f.run()).reason, 'source_changed'); assert.equal(reads, 2); assert.equal(f.received(), undefined);
});
test('changed source bytes cannot reuse the first declared snapshot revision', async () => {
  const f = setup(); let reads = 0;
  f.adapters.readSource = async () => {
    if(++reads === 1) return f.source;
    return snapshot(f.seed, 'platyr', 'mac', [fx.envelope(fx.record(f.seed, 'mac', {lastActivityAt: 3}))], {revision: f.source.revision});
  };
  assert.equal((await f.run()).reason, 'snapshot_changed');
  assert.equal(reads, 2); assert.equal(f.received(), undefined);
});
for(const [key, value] of [['version', 'unknown'], ['managerSha256', '0'.repeat(64)],
  ['warmGuardVerified', false], ['autoResumeGuardVerified', false]]) {
  test(`second source collect exact native ${key} drift refuses before append`, async () => {
    const f = setup(); let reads = 0;
    f.adapters.readSource = async () => {
      if(++reads === 1) return f.source;
      return {...f.source, nativeGuard: {...f.source.nativeGuard, [key]: value}};
    };
    assert.equal((await f.run()).reason, 'native_guard_unverified');
    assert.equal(reads, 2); assert.equal(f.received(), undefined);
    assert.equal(f.events.includes('append'), false);
  });
}
for(const which of ['source', 'target']) for(const value of [false, undefined]) {
  test(`${which} snapshotStable=${value} never implies a coherent snapshot`, async () => {
    const f = setup(); f[which].snapshotStable = value;
    assert.equal((await f.run()).reason, 'snapshot_changed'); assert.equal(f.received(), undefined);
  });
}
test('destination reopening is never stopped and refuses append', async () => {
  const f = setup(); let calls = 0; f.adapters.isTargetClosed = async () => ++calls === 1;
  assert.equal((await f.run()).reason, 'target_opened'); assert.equal(f.received(), undefined);
});
test('destination protected-state drift refuses append', async () => {
  const f = setup({adapters: {async targetProtectedStateUnchanged() {return false;}}});
  assert.equal((await f.run()).reason, 'target_state_changed'); assert.equal(f.received(), undefined);
});
test('transcript stat availability never grants readiness or resume authority', async () => {
  const f = setup({adapters: {async verifyTranscriptReferences() {return false;}}});
  assert.equal((await f.run()).reason, 'transcript_metadata_unverified'); assert.equal(f.received(), undefined);
});
test('duplicate original CLI ID refuses the snapshot', async () => {
  const f = setup(), first = fx.record(f.seed);
  replaceRows(f.source, [first, {...first, sessionId: 'local_' + fx.uuid(99)}]);
  assert.equal((await f.run()).reason, 'duplicate_identity'); assert.equal(f.received(), undefined);
});
test('raw byte hash mismatch refuses the snapshot', async () => {
  const f = setup(); f.source.records[0].sha256 = '0'.repeat(64);
  assert.equal((await f.run()).reason, 'record_binding_invalid'); assert.equal(f.received(), undefined);
});
test('malformed JSON with a matching raw byte hash still refuses the record', async () => {
  const f = setup(), bytes = Buffer.from('{SYNTHETIC_INVALID_JSON');
  f.source.records[0].bytes = bytes; f.source.records[0].sha256 = fx.digest(bytes);
  f.source.revision = sync.snapshotRevision(f.source.records);
  assert.equal((await f.run()).reason, 'record_invalid'); assert.equal(f.received(), undefined);
});
test('filename/desktop ID binding and original CLI UUID syntax are required', async () => {
  for(const mutate of [
    f => {f.source.records[0].name = 'local_' + fx.uuid(99) + '.json';},
    f => {replaceRows(f.source, [fx.record(f.seed, 'mac', {cliSessionId: 'SYNTHETIC_INVALID_ID'})]);},
  ]) {
    const f = setup(); mutate(f); f.source.revision = sync.snapshotRevision(f.source.records);
    assert.equal((await f.run()).reason, 'record_identity_invalid'); assert.equal(f.received(), undefined);
  }
});
test('duplicate desktop ID with a different CLI ID refuses the snapshot', async () => {
  const f = setup(), first = fx.record(f.seed), second = fx.record(f.seed, 'mac', {sessionId: first.sessionId}, 2);
  replaceRows(f.source, [first, second]);
  assert.equal((await f.run()).reason, 'duplicate_identity'); assert.equal(f.received(), undefined);
});
test('transaction refusal and adapter errors suppress private canary text', async () => {
  const refusal = setup({adapters: {async appendCreateOnly() {return {status: 'refused_replacement_preserved', error: fx.PRIVATE_ERROR};}}});
  const out = await refusal.run(); assert.equal(out.status, 'refused'); assert.equal(out.recoveryRequired, true);
  assert.equal(JSON.stringify(out).includes(fx.PRIVATE_ERROR), false);
  const error = setup({adapters: {async readSource() {throw new Error(fx.PRIVATE_ERROR);}}});
  const unavailable = await error.run(); assert.equal(unavailable.reason, 'unavailable');
  assert.equal(JSON.stringify(unavailable).includes(fx.PRIVATE_ERROR), false);
});
test('lost append receipt conservatively requires recovery without retry or no-writes claim', async () => {
  let attempts = 0;
  const maybeWritten = new Set();
  const f = setup({adapters: {async appendCreateOnly(payload) {
    attempts++;
    maybeWritten.add(payload.records[0].name);
    throw new Error(fx.PRIVATE_ERROR);
  }}});
  const out = await f.run();
  assert.equal(attempts, 1); assert.equal(maybeWritten.size, 1);
  assert.equal(out.status, 'refused'); assert.equal(out.reason, 'create_only_transaction_unconfirmed');
  assert.equal(out.recoveryRequired, true); assert.equal(out.createdCount, 0);
  assert.equal(Object.hasOwn(out, 'noWrites'), false);
  assert.equal(JSON.stringify(out).includes(fx.PRIVATE_ERROR), false);
  assert.equal(JSON.stringify(out).includes([...maybeWritten][0]), false);
});
for(const result of [
  {status: 'refused', ownedFilesRolledBack: true, recoveryRequired: false, createdCountBeforeRefusal: 1},
  {status: 'refused_rolled_back', recoveryRequired: false, createdCountBeforeRefusal: 1},
]) test(`positive rollback proof ${result.status} reports no remaining recovery requirement`, async () => {
  let attempts = 0;
  const f = setup({adapters: {async appendCreateOnly() {attempts++; return {...result, error: fx.PRIVATE_ERROR};}}});
  const out = await f.run();
  assert.equal(attempts, 1); assert.equal(out.status, 'refused');
  assert.equal(out.reason, 'create_only_transaction_refused'); assert.equal(out.recoveryRequired, false);
  assert.equal(out.createdCountBeforeRefusal, 1); assert.equal(out.createdCount, 0);
  assert.equal(JSON.stringify(out).includes(fx.PRIVATE_ERROR), false);
});
for(const result of [
  {status: 'refused', ownedFilesRolledBack: false, recoveryRequired: true},
  {status: 'refused', recoveryRequired: true},
  {status: 'refused', recoveryRequired: false},
  {status: 'refused_replacement_preserved', recoveryRequired: false},
  undefined,
  {status: 'created_metadata', createdCount: 0, protectedBytesUnchanged: true},
  {status: 'created_metadata', createdCount: 1, protectedBytesUnchanged: false},
]) test(`uncertain/refused append ${JSON.stringify(result)} conservatively requires recovery`, async () => {
  let attempts = 0;
  const f = setup({adapters: {async appendCreateOnly() {attempts++; return result;}}});
  const out = await f.run();
  assert.equal(attempts, 1); assert.equal(out.status, 'refused');
  assert.equal(out.reason, 'create_only_transaction_refused'); assert.equal(out.recoveryRequired, true);
  assert.equal(out.createdCount, 0);
});
test('lost optional append receipt defers existing Open without retry or private error', async () => {
  let attempts = 0, opens = 0;
  const f = setup({adapters: {async appendCreateOnly() {attempts++; throw new Error(fx.PRIVATE_ERROR);}}});
  await assert.rejects(sync.beforeOrdinaryProfileOpen(() => f.run(), async () => {opens++; return true;}),
    error => error.reason === 'history_append_pending' && !String(error).includes(fx.PRIVATE_ERROR));
  assert.equal(attempts, 1); assert.equal(opens, 0);
});
test('optional sync skip still permits exactly one existing authorized Open callback', async () => {
  const events = [];
  const out = await sync.beforeOrdinaryProfileOpen(async () => {events.push('sync'); return {status: 'skipped', reason: 'unsupported_direction'};}, async () => {events.push('ordinary-open'); return true;});
  assert.deepEqual(events, ['sync', 'ordinary-open']); assert.equal(out.opened, true);
});

module.exports = {privatePolicy, snapshot, setup, replaceRows};
