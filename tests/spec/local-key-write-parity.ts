import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { registerFileWriter } from '../../src/shared/private-file-lease';
import { settlePrivateFileOwners } from '../../src/shared/private-files';
import { keyFingerprint, LocalKeyStore } from '../../src/web-server/services/account-key-store';

const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http');
const modes = process.argv.includes('--ts-only') ? ['off', 'shadow'] : ['off', 'shadow', 'on'];
for (const mode of modes) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-local-key-')));
  const folder = path.join(root, 'account-usage', 'keys');
  const filename = path.join(folder, 'zai-aabbccdd.json');
  let nativeWrites = 0;
  const stop = mode === 'on' ? registerFileWriter(root, binary, () => nativeWrites++) : () => {};
  settlePrivateFileOwners((group) => mode === 'on' && group === 'writes');
  try {
    const store = new LocalKeyStore(root);
    const input = () => randomBytes(24).toString('hex');
    const put = async () => {
      const value = input();
      const receipt = await store.put('zai', 'aabbccdd', value);
      assert.equal(receipt.fingerprint, keyFingerprint(value));
      assert.deepEqual(Object.keys(receipt).sort(), ['fingerprint', 'last4', 'storedOn']);
      assert.deepEqual(await store.info('zai', 'aabbccdd'), receipt);
      if (process.platform !== 'win32') {
        assert.equal(fs.statSync(folder).mode & 0o777, 0o700);
        assert.equal(fs.statSync(filename).mode & 0o777, 0o600);
      }
      assert.deepEqual(fs.readdirSync(folder), ['zai-aabbccdd.json']);
    };
    await put();
    await put();
    await store.delete('zai', 'aabbccdd');
    await store.delete('zai', 'aabbccdd');
    assert.equal(await store.info('zai', 'aabbccdd'), null);
    assert.equal(nativeWrites, mode === 'on' ? 3 : 0);
    stop();
    await put();
    assert.equal(nativeWrites, mode === 'on' ? 3 : 0);
  } finally {
    stop();
    settlePrivateFileOwners(() => false);
    fs.rmSync(root, { recursive: true, force: true });
  }
}
console.log('local key put, replace, delete and failback: ' + modes.join('/'));
