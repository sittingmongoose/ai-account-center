'use strict';
// Frozen production primitive and public receipt mapping, exercised only on synthetic roots.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {transaction} = require('../../../scripts/claude-history/history_index_transaction_v1.cjs');
const {publicResult} = require('../../../scripts/claude-history/history_index_node_bridge_v2.cjs');
const scenario = process.argv[2];
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'history-node-'));
const registry = path.join(root, 'registry');
fs.mkdirSync(registry);
fs.writeFileSync(path.join(root, 'config.json'), 'protected');
const name = 'local_12345678-1234-1234-1234-123456789abc.json';
if (scenario === 'exists') fs.writeFileSync(path.join(registry, name), 'foreign');
let fired = false;
const onPhase = (phase, context) => {
  const event = phase === 'beforeMutation' ? context.kind : phase;
  const trigger = {protected: 'writePrivateContent', stage: 'staged', created: 'created', registry: 'staged', 'publication-private': 'linkCreateOnly'}[scenario];
  if (fired || event !== trigger) return;
  fired = true;
  if (scenario === 'protected') fs.writeFileSync(path.join(root, 'config.json'), 'changed');
  if (scenario === 'stage') {
    fs.renameSync(context.path, context.path + '.foreign');
    fs.writeFileSync(context.path, 'replacement');
  }
  if (scenario === 'created') fs.writeFileSync(context.path, 'replacement');
  if (scenario === 'registry') {fs.renameSync(registry, path.join(root, 'moved')); fs.mkdirSync(registry);}
};
let result;
try {
  result = publicResult(transaction({
    profileRoot: root, registryRoot: registry,
    records: [{name, bytes: Buffer.from('{"fixture":true}')}],
    protected: [{name: 'config.json', bytes: Buffer.from('protected')}],
    closedGuard: () => scenario !== 'closed',
    platform: ['private', 'publication-private'].includes(scenario) ? 'win32' : 'darwin',
    privateStorageGuard: context => scenario !== 'private' && !(scenario === 'publication-private' && fired && path.basename(context.path).startsWith('.history-index-stage-')),
  }, {onPhase}));
} catch (_) {result = publicResult({});}
process.stdout.write(JSON.stringify(result));
