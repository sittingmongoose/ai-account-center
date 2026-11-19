import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { once } from 'node:events';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { registerFileWriter } from '../../src/shared/private-file-lease';
import { keyQueue, serialized } from '../../src/web-server/services/account-lifecycle-queue';

const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http');
const [operation, childRoot] = process.argv.slice(2);
if (operation === 'hold') {
  const releaseWriter = registerFileWriter(childRoot, binary, () => undefined);
  try {
    await serialized(childRoot, keyQueue('zai'), async () => {
      process.stdout.write('held\n');
      await once(process.stdin, 'end');
    });
  } finally {
    releaseWriter();
  }
} else {
  for (const mode of ['off', 'shadow', 'on']) {
    const root = await mkdtemp(path.join(tmpdir(), 'aac-queue-'));
    try {
      const releaseWriter =
        mode === 'on' ? registerFileWriter(root, binary, () => undefined) : () => {};
      try {
        if (mode === 'on') {
          const child = spawn(process.execPath, [import.meta.path, 'hold', root], {
            stdio: ['pipe', 'pipe', 'inherit'],
          });
          const finished = once(child, 'exit');
          try {
            const [message] = await Promise.race([
              once(child.stdout, 'data'),
              finished.then(() => {
                throw new Error('queue fixture exited before acquiring the lock');
              }),
            ]);
            assert.equal(String(message), 'held\n');
            let entered = false;
            const pending = serialized(root, keyQueue('zai'), async () => {
              entered = true;
            });
            await serialized(root, keyQueue('opencode-go'), async () => {});
            assert.equal(entered, false);
            child.stdin.end();
            await pending;
            assert.equal(entered, true);
            assert.deepEqual(await finished, [0, null]);
          } finally {
            child.stdin.end();
            child.kill();
          }
        }
        const order: string[] = [];
        const failed = serialized(root, keyQueue('zai'), async () => {
          order.push('first');
          throw new Error('fixture failure');
        });
        const next = serialized(root, keyQueue('zai'), async () => order.push('second'));
        await assert.rejects(failed, /fixture failure/);
        await next;
        assert.deepEqual(order, ['first', 'second']);
      } finally {
        releaseWriter();
      }
      console.log('lifecycle queue ' + mode + ': passed');
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  }
}
