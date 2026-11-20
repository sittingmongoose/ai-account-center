import assert from 'node:assert/strict';
import path from 'node:path';
import { startLifecycleMaintenance } from '../../src/web-server/services/account-maintenance';

const expected = [
  'codex-staging',
  'orphan-keys',
  'antigravity-staging',
  'antigravity-orphans',
  'claude-trash',
];
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http');

for (const mode of ['off', 'shadow', 'on', 'missing-binary']) {
  const seen: string[] = [];
  let finish!: () => void;
  const complete = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const maintenance = startLifecycleMaintenance(
    async (step) => {
      seen.push(step);
      if (step === 'orphan-keys') throw new Error('fixture failure');
      if (step === 'claude-trash') finish();
    },
    mode === 'on' ? binary : mode === 'missing-binary' ? binary + '.missing' : undefined
  );
  const guard = setTimeout(() => {
    maintenance.stop();
    process.exitCode = 1;
    finish();
  }, 5000);
  try {
    await complete;
    assert.deepEqual(seen, expected);
    assert.equal(maintenance.native(), mode === 'on');
    console.log('daily lifecycle dispatch ' + mode + ': 5/5');
  } finally {
    clearTimeout(guard);
    maintenance.stop();
  }
}
