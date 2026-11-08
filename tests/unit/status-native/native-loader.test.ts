import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  loadNativeStatus,
  nativeStatusModuleName,
  STATUS_BINDING_VERSION,
  statusUnavailableSentence,
} from '../../../src/status-native/native-loader';

function withFile<T>(run: (file: string) => T): T {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-status-loader-'));
  const file = path.join(dir, nativeStatusModuleName());
  fs.writeFileSync(file, 'not a module');
  try {
    return run(file);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

describe('status readers loader', () => {
  it('names the module after the platform and architecture', () => {
    expect(nativeStatusModuleName('linux', 'x64')).toBe('aac-status.linux-x64.node');
    expect(nativeStatusModuleName('win32', 'x64')).toBe('aac-status.win32-x64.node');
  });

  it('AAC_STATUS_NATIVE=off disables the readers before anything is loaded', () => {
    let loaded = false;
    const result = loadNativeStatus({
      env: { AAC_STATUS_NATIVE: 'off' },
      requireFn: () => {
        loaded = true;
        return {};
      },
    });
    expect(result).toEqual({ module: null, reason: 'disabled', file: null });
    expect(loaded).toBe(false);
    expect(statusUnavailableSentence('disabled')).toBe(
      'the aac-status native module is disabled'
    );
  });

  it('a missing module is an ordinary absence, never a throw', () => {
    const result = loadNativeStatus({ env: {}, file: path.join(os.tmpdir(), 'nope', 'x.node') });
    expect(result.module).toBeNull();
    expect(result.module === null && result.reason).toBe('missing');
    expect(statusUnavailableSentence('missing')).toBe('the aac-status native module is missing');
  });

  it('a module of another binding version is refused, and a throwing one is a load failure', () => {
    withFile((file) => {
      const wrong = loadNativeStatus({
        env: {},
        file,
        requireFn: () => ({
          NativeStatus: class {},
          bindingVersion: () => STATUS_BINDING_VERSION + 1,
        }),
      });
      expect(wrong.module === null && wrong.reason).toBe('incompatible');
      const thrown = loadNativeStatus({
        env: {},
        file,
        requireFn: () => {
          throw new Error('dlopen failed');
        },
      });
      expect(thrown.module === null && thrown.reason).toBe('load_failed');
      const good = loadNativeStatus({
        env: {},
        file,
        requireFn: () => ({
          NativeStatus: class {},
          bindingVersion: () => STATUS_BINDING_VERSION,
        }),
      });
      expect(good.module).not.toBeNull();
    });
  });
});
