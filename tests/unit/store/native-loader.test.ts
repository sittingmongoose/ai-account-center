import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  loadNativeStore,
  nativeModuleName,
  STORE_BINDING_VERSION,
} from '../../../src/store/native-loader';
import {
  openAacStore,
  resetAacStoreForTests,
  shareBackupRoot,
} from '../../../src/store/store-service';

function withFile<T>(run: (file: string) => T): T {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-loader-'));
  const file = path.join(dir, nativeModuleName());
  fs.writeFileSync(file, 'not a module');
  try {
    return run(file);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

describe('history store loader', () => {
  it('names the module after the platform and architecture', () => {
    expect(nativeModuleName('linux', 'x64')).toBe('aac-store.linux-x64.node');
    expect(nativeModuleName('win32', 'x64')).toBe('aac-store.win32-x64.node');
  });

  it('AAC_STORE=off disables the store before anything is loaded', () => {
    let loaded = false;
    const result = loadNativeStore({
      env: { AAC_STORE: 'off' },
      requireFn: () => {
        loaded = true;
        return {};
      },
    });
    expect(result).toEqual({ module: null, reason: 'disabled', file: null });
    expect(loaded).toBe(false);
  });

  it('a missing module is an ordinary absence', () => {
    const result = loadNativeStore({ env: {}, file: path.join(os.tmpdir(), 'nope', 'x.node') });
    expect(result.module).toBeNull();
    expect(result.module === null && result.reason).toBe('missing');
  });

  it('a module of another binding version is refused, and a throwing one is a load failure', () => {
    withFile((file) => {
      const wrong = loadNativeStore({
        env: {},
        file,
        requireFn: () => ({
          NativeStore: class {},
          bindingVersion: () => STORE_BINDING_VERSION + 1,
        }),
      });
      expect(wrong.module === null && wrong.reason).toBe('incompatible');
      const thrown = loadNativeStore({
        env: {},
        file,
        requireFn: () => {
          throw new Error('dlopen failed');
        },
      });
      expect(thrown.module === null && thrown.reason).toBe('load_failed');
      const good = loadNativeStore({
        env: {},
        file,
        requireFn: () => ({ NativeStore: class {}, bindingVersion: () => STORE_BINDING_VERSION }),
      });
      expect(good.module).not.toBeNull();
    });
  });

  it('without a module the store state says why and the dashboard has no store', async () => {
    resetAacStoreForTests();
    const state = await openAacStore({
      load: () => ({ module: null, reason: 'missing', file: null }),
    });
    expect(state).toEqual({ store: null, reason: 'missing' });
    resetAacStoreForTests();
  });

  it('an open failure is reported by its code, never thrown into the dashboard', async () => {
    resetAacStoreForTests();
    const state = await openAacStore({
      ccsDir: os.tmpdir(),
      load: () => ({
        module: {
          NativeStore: class {
            async call(): Promise<string> {
              throw new Error('lock_held: another process holds the store lock');
            }
          },
          bindingVersion: () => STORE_BINDING_VERSION,
        },
        file: 'fake',
      }),
    });
    expect(state).toEqual({ store: null, reason: 'lock_held' });
    resetAacStoreForTests();
  });
});

describe('share backup folder (decision D3)', () => {
  const instance = 'a'.repeat(32);
  it('uses an existing share project folder beside the usage snapshot folder', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-share-'));
    try {
      const snapshot = path.join(root, 'aac-usage');
      expect(shareBackupRoot(instance, snapshot, {})).toBeNull();
      fs.mkdirSync(path.join(root, 'share', 'ai-account-center'), { recursive: true });
      expect(shareBackupRoot(instance, snapshot, {})).toBe(
        path.join(root, 'share', 'ai-account-center', 'store-backups', instance)
      );
      expect(shareBackupRoot('bad', snapshot, {})).toBeNull();
      expect(shareBackupRoot(instance, null, {})).toBeNull();
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
  it('honours an explicit folder or off', () => {
    expect(shareBackupRoot(instance, null, { AAC_STORE_SHARE_BACKUP_DIR: '/x/y' })).toBe('/x/y');
    expect(shareBackupRoot(instance, '/a/b', { AAC_STORE_SHARE_BACKUP_DIR: 'off' })).toBeNull();
  });
});
