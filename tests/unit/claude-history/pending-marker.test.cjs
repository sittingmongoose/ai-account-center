'use strict';
const {test, beforeEach, afterEach} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const corePath = path.resolve(__dirname, '../../../scripts/claude-history/history-index-sync.cjs');
const core = require(corePath);
let root;
const hold = () => core.pendingMarkerState(root, 'platyr', 'windows').held;
const markerRoot = () => path.join(root, 'claude-history-pending');
const markerFile = () => path.join(markerRoot(), fs.readdirSync(markerRoot())[0]);
const receipt = (changes = {}) => ({status:'created_metadata', createdCount:1, writerQuiescent:true, ...changes});
beforeEach(() => { root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-pending-fixture-')); fs.chmodSync(root, 0o700); });
afterEach(() => { fs.rmSync(root, {recursive:true, force:true}); });

test('absent optional marker does not create storage or hold ordinary Open', () => {
  assert.equal(hold(), false); assert.deepEqual(fs.readdirSync(root), []);
});
test('create-only pending marker is private and survives module/server restart', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows'); marker.release();
  assert.equal(fs.statSync(markerRoot()).mode & 0o777, 0o700);
  assert.equal(fs.statSync(markerFile()).mode & 0o777, 0o600);
  delete require.cache[corePath];
  assert.equal(require(corePath).pendingMarkerState(root, 'platyr', 'windows').held, true);
});
test('lost append response retains hold; new click cannot arm a second writer', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows'); marker.release();
  assert.equal(hold(), true); assert.throws(() => core.armPendingMarker(root, 'platyr', 'windows'));
  assert.equal(fs.readdirSync(markerRoot()).length, 1);
});
test('trusted successful terminal acknowledgement finishes only owned marker', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows'); marker.finish(receipt()); marker.release();
  assert.equal(hold(), false); assert.equal(JSON.parse(fs.readFileSync(markerFile())).state, 'finished');
});
test('duplicate JSON state cannot disguise a pending or malformed marker as finished', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows'); marker.finish(receipt()); marker.release();
  const filename=markerFile(),original=fs.readFileSync(filename,'utf8');
  fs.writeFileSync(filename,'{"state":"pending",'+original.slice(1));
  assert.equal(hold(),true);
});
test('terminal refusal may finish hold without asserting zero prior writes or rollback', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows');
  marker.finish(receipt({status:'refused', createdCount:0, recoveryRequired:true})); marker.release();
  assert.equal(hold(), false);
  const row = JSON.parse(fs.readFileSync(markerFile()));
  assert.equal(Object.hasOwn(row, 'zeroWrites'), false); assert.equal(Object.hasOwn(row, 'rolledBack'), false);
});
for (const bad of [{}, receipt({writerQuiescent:false}), receipt({createdCount:-1}), receipt({status:'unknown'})])
test('malformed or nonquiescent terminal response retains durable hold', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows');
  assert.throws(() => marker.finish(bad)); marker.release(); assert.equal(hold(), true);
});
test('foreign pathname replacement is preserved and cannot be completed', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows'); const filename = markerFile();
  fs.renameSync(filename, path.join(root, 'preserved-original'));
  const foreign = Buffer.from('FOREIGN_REPLACEMENT'); fs.writeFileSync(filename, foreign, {mode:0o600});
  assert.throws(() => marker.finish(receipt())); marker.release();
  assert.deepEqual(fs.readFileSync(filename), foreign); assert.equal(hold(), true);
});
test('symlink marker never modifies its target or clears hold', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows'); const filename = markerFile();
  fs.renameSync(filename, path.join(root, 'preserved-original'));
  const target = path.join(root, 'foreign-target'); fs.writeFileSync(target, 'FOREIGN', {mode:0o600}); fs.symlinkSync(target, filename);
  assert.throws(() => marker.finish(receipt())); marker.release(); assert.equal(fs.readFileSync(target, 'utf8'), 'FOREIGN'); assert.equal(hold(), true);
});
test('privacy drift holds Open and is never silently chmodded', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows'); fs.chmodSync(markerFile(), 0o644);
  assert.throws(() => marker.finish(receipt())); marker.release(); assert.equal(hold(), true);
  assert.equal(fs.statSync(markerFile()).mode & 0o777, 0o644);
});
test('pending operation is profile/platform-bound', () => {
  const marker = core.armPendingMarker(root, 'platyr', 'windows'); marker.release();
  assert.equal(core.pendingMarkerState(root, 'platyr', 'mac').held, false);
  assert.equal(core.pendingMarkerState(root, 'gmail', 'mac').held, false);
});
test('unknown storage entries hold safely without reading their body', () => {
  fs.mkdirSync(markerRoot(), {mode:0o700}); fs.writeFileSync(path.join(markerRoot(), 'unknown'), 'PRIVATE_CANARY', {mode:0o600});
  assert.equal(hold(), true);
});
test('finished operations are independent of a later uncertain operation', () => {
  const first = core.armPendingMarker(root, 'platyr', 'windows'); first.finish(receipt()); first.release();
  const second = core.armPendingMarker(root, 'platyr', 'windows'); second.release();
  assert.equal(hold(), true); assert.equal(fs.readdirSync(markerRoot()).length, 2);
});
test('generic optional Open helper rejects an unknown append but still opens initial skips', async () => {
  let opened = 0;
  await assert.rejects(core.beforeOrdinaryProfileOpen(async () => ({reason:'create_only_transaction_unconfirmed'}), async () => opened++));
  assert.equal(opened, 0);
  await core.beforeOrdinaryProfileOpen(async () => ({status:'skipped', reason:'history_policy_unverified'}), async () => opened++);
  assert.equal(opened, 1);
});
test('nonprivate root prevents a new append without creating any hold', () => {
  fs.chmodSync(root, 0o755); assert.throws(() => core.armPendingMarker(root, 'platyr', 'windows'));
  assert.equal(hold(), false); assert.deepEqual(fs.readdirSync(root), []);
});
test('bounded finished-marker capacity skips another copy without inventing a pending writer', () => {
  fs.mkdirSync(markerRoot(), {mode:0o700});
  for(let i=0;i<256;i++) {
    const nonce=i.toString(16).padStart(32,'0');
    fs.writeFileSync(path.join(markerRoot(),`platyr-windows-${nonce}.json`), JSON.stringify({version:1,
      profileId:'platyr',targetPlatform:'windows',nonce,state:'finished',createdAt:'2026-10-02T00:00:00Z',
      finishedAt:'2026-10-02T00:00:01Z',terminalReceiptSha256:'0'.repeat(64)}),{mode:0o600});
  }
  assert.equal(hold(),false);assert.throws(() => core.armPendingMarker(root,'platyr','windows'),
    error => error.reason === 'history_marker_unavailable');assert.equal(hold(),false);
});
