/**
 * The private JSON store's folder rules and lock key
 * (CONTRACT-registry-lifecycle section 1, rule 6). Temporary folders only.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  privateFileLockKey,
  readPrivateJsonFile,
  writePrivateJsonFile,
} from '../../../src/web-server/services/private-json-store';
import {
  ACCOUNT_REGISTRY_FILE,
  readAccountRegistry,
  updateAccountRegistry,
  type AccountRegistryV2,
} from '../../../src/web-server/services/account-registry-v2';
import {
  readAccountVisibility,
  writeAccountVisibility,
} from '../../../src/web-server/services/account-visibility';

const posix = process.platform !== 'win32';
const temporaryDirs: string[] = [];
afterEach(() => {
  for (const dir of temporaryDirs.splice(0)) {
    fs.chmodSync(dir, 0o700);
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

function root(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-private-store-'));
  temporaryDirs.push(dir);
  return dir;
}

/** A store folder with an explicit mode (chmod, so the umask does not apply). */
function folder(mode: number): string {
  const dir = path.join(root(), '.ccs');
  fs.mkdirSync(dir);
  fs.chmodSync(dir, mode);
  return dir;
}

function writeValue(dir: string, name: string, value: unknown): string {
  const file = path.join(dir, name);
  fs.writeFileSync(file, JSON.stringify(value), { mode: 0o600 });
  fs.chmodSync(file, 0o600);
  return file;
}

describe.skipIf(!posix)('private store folder', () => {
  it('refuses a folder that others can write, on read and on write', async () => {
    const dir = folder(0o700);
    const file = writeValue(dir, 'store.json', { version: 1 });
    expect((await readPrivateJsonFile(file, 1024)).state).toBe('ok');
    for (const mode of [0o777, 0o703, 0o1777]) {
      fs.chmodSync(dir, mode);
      expect({ mode, read: await readPrivateJsonFile(file, 1024) }).toEqual({
        mode,
        read: { state: 'invalid' },
      });
      // Even an absent file is "could not be read", never "empty".
      expect(await readPrivateJsonFile(path.join(dir, 'absent.json'), 1024)).toEqual({
        state: 'invalid',
      });
      await expect(writePrivateJsonFile(file, { version: 2 })).rejects.toThrow(
        'The private store folder is not safe to write.'
      );
    }
    fs.chmodSync(dir, 0o700);
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toEqual({ version: 1 });
    expect(fs.readdirSync(dir)).toEqual(['store.json']);
  });

  it('accepts a group-writable folder (Ubuntu private-group umask) with a warning only', async () => {
    const dir = folder(0o775);
    const file = writeValue(dir, 'store.json', { version: 1 });
    expect((await readPrivateJsonFile(file, 1024)).state).toBe('ok');
    await writePrivateJsonFile(file, { version: 2 });
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toEqual({ version: 2 });
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
  });

  it('checks the target of a symlinked folder, and refuses a folder that is not a directory', async () => {
    const target = folder(0o700);
    writeValue(target, 'store.json', { version: 1 });
    const link = path.join(root(), '.ccs-link');
    fs.symlinkSync(target, link);
    expect((await readPrivateJsonFile(path.join(link, 'store.json'), 1024)).state).toBe('ok');
    fs.chmodSync(target, 0o777);
    expect(await readPrivateJsonFile(path.join(link, 'store.json'), 1024)).toEqual({
      state: 'invalid',
    });
    fs.chmodSync(target, 0o700);
    const notFolder = path.join(root(), 'plain-file');
    fs.writeFileSync(notFolder, 'x');
    expect(await readPrivateJsonFile(path.join(notFolder, 'store.json'), 1024)).toEqual({
      state: 'invalid',
    });
  });

  it('applies to the registry and the visibility store', async () => {
    const dir = folder(0o777);
    expect(await readAccountRegistry(dir)).toEqual({ state: 'invalid' });
    expect(await readAccountVisibility(dir)).toEqual({ state: 'unavailable' });
    await expect(
      writeAccountVisibility(dir, {
        hiddenProviders: ['zai'],
        hiddenAccountIds: [],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [],
      })
    ).rejects.toThrow();
    await expect(updateAccountRegistry(dir, (registry) => registry)).rejects.toThrow();
    expect(fs.readdirSync(dir)).toEqual([]);
  });
});

describe.skipIf(!posix)('private store lock key', () => {
  it('is the same for a file reached through a symlinked CCS_HOME, even before the folder exists', () => {
    const home = root();
    const link = path.join(root(), 'home-link');
    fs.symlinkSync(home, link);
    const real = privateFileLockKey(path.join(home, '.ccs', 'store.json'));
    expect(privateFileLockKey(path.join(link, '.ccs', 'store.json'))).toBe(real);
    fs.mkdirSync(path.join(home, '.ccs'), { mode: 0o700 });
    expect(privateFileLockKey(path.join(link, '.ccs', 'store.json'))).toBe(real);
    expect(privateFileLockKey(path.join(link, '.ccs', '..', '.ccs', 'store.json'))).toBe(real);
  });

  it('serializes registry writes made through the real and the symlinked path', async () => {
    const home = root();
    const link = path.join(root(), 'home-link');
    fs.symlinkSync(home, link);
    const add =
      (hex: string) =>
      (registry: AccountRegistryV2): AccountRegistryV2 => ({
        version: 2,
        accounts: [
          ...registry.accounts,
          {
            id: `zai:acct:${hex}`,
            provider: 'zai',
            platform: 'ubuntu',
            sshHost: null,
            label: null,
            credential: { kind: 'aac-key', keyId: hex },
            createdAt: null,
            createdBy: 'dashboard',
          },
        ],
      });
    const real = path.join(home, '.ccs');
    const linked = path.join(link, '.ccs');
    await Promise.all(
      ['00000001', '00000002', '00000003', '00000004', '00000005', '00000006'].map((hex, index) =>
        updateAccountRegistry(index % 2 === 0 ? real : linked, add(hex))
      )
    );
    const read = await readAccountRegistry(real);
    expect(
      read.state === 'ok' &&
        read.registry.accounts
          .filter((entry) => entry.id.startsWith('zai:acct:'))
          .map((entry) => entry.id)
          .sort()
    ).toEqual([1, 2, 3, 4, 5, 6].map((index) => `zai:acct:0000000${index}`));
    expect(fs.readdirSync(real)).toEqual([ACCOUNT_REGISTRY_FILE]);
  });
});
