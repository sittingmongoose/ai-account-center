import assert from 'node:assert/strict';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { registerFileWriter } from '../../src/shared/private-file-lease';
import {
  runKeyWorkflow,
  type KeyEffect,
  type KeyWorkflow,
} from '../../src/web-server/services/account-key-workflow';

const cases: Array<[KeyWorkflow, KeyEffect | null, boolean, string[]]> = [
  ['add', null, true, ['store', 'publish', 'probe']],
  ['add', 'publish', true, ['store', 'publish', 'discard']],
  ['add', null, false, ['store', 'publish', 'probe', 'unpublish', 'discard']],
  ['replace', null, true, ['store', 'probe', 'publish', 'discard']],
  ['replace', null, false, ['store', 'probe', 'discard']],
  ['replace', 'publish', true, ['store', 'probe', 'publish', 'discard']],
  ['remove', null, true, ['unpublish', 'discard']],
  ['remove', 'discard', true, ['unpublish', 'discard', 'publish']],
];
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http');

for (const mode of ['off', 'shadow', 'on']) {
  const root = await mkdtemp(path.join(tmpdir(), 'aac-key-workflow-'));
  const releaseWriter =
    mode === 'on' ? registerFileWriter(root, binary, () => undefined) : () => {};
  try {
    for (const [workflow, failAt, accepted, expected] of cases) {
      const error = new Error('fixture failure');
      const seen: string[] = [];
      const result = runKeyWorkflow(root, workflow, async (effect) => {
        seen.push(effect);
        if (effect === failAt) throw error;
        if (effect === 'probe') return accepted;
      });
      if (failAt) await assert.rejects(result, (result) => result === error);
      else assert.equal(await result, accepted);
      assert.deepEqual(seen, expected);
    }
    console.log('key workflow ' + mode + ': ' + cases.length + '/' + cases.length);
  } finally {
    releaseWriter();
    await rm(root, { recursive: true, force: true });
  }
}
