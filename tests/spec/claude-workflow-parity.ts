import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { registerFileWriter } from '../../src/shared/private-file-lease';
import {
  runClaudeWorkflow,
  type ClaudeEffect,
  type ClaudeWorkflow,
} from '../../src/web-server/services/claude-lifecycle-workflow';

const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http');
const cases: Array<[ClaudeWorkflow, ClaudeEffect | null, string[]]> = [
  ['add', null, ['apply-mac', 'apply-windows', 'commit']],
  ['add', 'apply-windows', ['apply-mac', 'apply-windows', 'undo-mac']],
  ['add', 'commit', ['apply-mac', 'apply-windows', 'commit', 'undo-windows', 'undo-mac']],
  ['remove', 'apply-windows', ['apply-mac', 'apply-windows', 'undo-mac']],
  ['remove', 'commit', ['apply-mac', 'apply-windows', 'commit', 'undo-windows', 'undo-mac']],
  ['restore', 'apply-windows', ['apply-mac', 'apply-windows', 'undo-mac']],
  ['restore', 'commit', ['apply-mac', 'apply-windows', 'commit']],
  ['purge', 'apply-mac', ['apply-mac', 'apply-windows']],
  ['purge', null, ['apply-mac', 'apply-windows', 'commit']],
  ['recheck', null, ['session', 'commit']],
  ['recheck', 'session', ['session']],
];

for (const mode of ['off', 'shadow', 'on']) {
  const root = await mkdtemp(path.join(tmpdir(), 'aac-workflow-'));
  const releaseWriter =
    mode === 'on' ? registerFileWriter(root, binary, () => undefined) : () => {};
  try {
    for (const [workflow, failAt, expected] of cases) {
      const error = new Error('fixture failure');
      const seen: string[] = [];
      const operation = runClaudeWorkflow(root, workflow, async (effect) => {
        seen.push(effect);
        if (effect === failAt || effect.startsWith('undo')) throw error;
      });
      if (failAt) await assert.rejects(operation, (result) => result === error);
      else await operation;
      assert.deepEqual(seen, expected, workflow + ' ' + failAt);
    }
    const signedOut: string[] = [];
    await runClaudeWorkflow(root, 'recheck', async (effect) => {
      signedOut.push(effect);
      return false;
    });
    assert.deepEqual(signedOut, ['session']);
    console.log('Claude workflow ' + mode + ': ' + (cases.length + 1) + '/' + (cases.length + 1));
  } finally {
    releaseWriter();
    await rm(root, { recursive: true, force: true });
  }
}
