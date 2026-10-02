import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { afterEach, describe, expect, spyOn, test } from 'bun:test';
import { AntigravityAutoSwitchFileStore } from '../../../../src/antigravity/auto-switch/store';
import { NOW, state } from './fixtures';

const ownedRoots: string[] = [];
function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-auto-fixture-'));
  ownedRoots.push(root);
  const directory = path.join(root, 'antigravity-switching');
  return {
    root,
    directory,
    store: new AntigravityAutoSwitchFileStore(directory),
  };
}
afterEach(() => {
  for (const root of ownedRoots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

describe('explicit own private state file', () => {
  test('a read of absent state returns off without creating files', () => {
    const value = fixture();
    expect(value.store.read().settings.enabled).toBe(false);
    expect(fs.existsSync(value.directory)).toBe(false);
  });

  test('writes its own settings atomically at 0600 and creates its own directory at 0700', () => {
    const value = fixture();
    const saved = state();
    saved.lastSwitch = {
      at: new Date(NOW).toISOString(),
      hostId: 'ubuntu',
      profileId: 'party',
    };
    value.store.write(saved);
    expect(value.store.read()).toEqual(saved);
    expect(fs.statSync(value.directory).mode & 0o777).toBe(0o700);
    expect(fs.statSync(value.store.file).mode & 0o777).toBe(0o600);
    expect(fs.readdirSync(value.directory)).toEqual(['antigravity-auto-switch.json']);
    value.store.write({
      ...saved,
      settings: { ...saved.settings, enabled: false },
    });
    expect(value.store.read().settings.enabled).toBe(false);
  });

  test('never reads or changes Codex, Claude or native Antigravity authentication files', () => {
    const value = fixture();
    const sentinels = [
      'codex-auto-switch.json',
      'config.yaml',
      'codex-profiles.yaml',
      'antigravity-oauth-token',
      'claude-settings.json',
    ];
    for (const name of sentinels)
      fs.writeFileSync(path.join(value.root, name), `fixture-private:${name}`, {
        mode: 0o600,
      });
    value.store.write(state());
    value.store.read();
    for (const name of sentinels)
      expect(fs.readFileSync(path.join(value.root, name), 'utf8')).toBe(`fixture-private:${name}`);
  });

  test('refuses a directory symlink and preserves its target', () => {
    const value = fixture();
    const foreign = path.join(value.root, 'foreign');
    fs.mkdirSync(foreign, { mode: 0o700 });
    fs.symlinkSync(foreign, value.directory);
    expect(() => value.store.write(state())).toThrow('not private');
    expect(fs.readdirSync(foreign)).toEqual([]);
  });

  test('refuses settings symlink, foreign file, permissive permissions and oversized data', () => {
    for (const mutate of [
      (file: string, root: string) => {
        const target = path.join(root, 'foreign');
        fs.writeFileSync(target, 'untouched', { mode: 0o600 });
        fs.symlinkSync(target, file);
      },
      (file: string) => {
        fs.mkdirSync(file, { mode: 0o700 });
      },
      (file: string) => {
        fs.writeFileSync(file, '{}', { mode: 0o644 });
      },
      (file: string) => {
        fs.writeFileSync(file, 'x'.repeat(16_385), { mode: 0o600 });
      },
    ]) {
      const value = fixture();
      fs.mkdirSync(value.directory, { mode: 0o700 });
      mutate(value.store.file, value.root);
      expect(() => value.store.read()).toThrow();
      expect(() => value.store.write(state())).toThrow();
      const foreign = path.join(value.root, 'foreign');
      if (fs.existsSync(foreign)) expect(fs.readFileSync(foreign, 'utf8')).toBe('untouched');
    }
  });

  test('invalid stored JSON/schema fails closed without replacing it', () => {
    const value = fixture();
    fs.mkdirSync(value.directory, { mode: 0o700 });
    const bytes = '{"version":2,"enabled":true}';
    fs.writeFileSync(value.store.file, bytes, { mode: 0o600 });
    expect(() => value.store.read()).toThrow();
    expect(fs.readFileSync(value.store.file, 'utf8')).toBe(bytes);
  });

  test('rejects an unsafe relative path before any write', () => {
    expect(() => new AntigravityAutoSwitchFileStore('relative-private-home')).toThrow('absolute');
  });

  test('rejects a symlinked ancestor instead of traversing it to create state', () => {
    const value = fixture();
    const target = path.join(value.root, 'target');
    fs.mkdirSync(target, { mode: 0o700 });
    const alias = path.join(value.root, 'alias');
    fs.symlinkSync(target, alias);
    const store = new AntigravityAutoSwitchFileStore(path.join(alias, 'private-switch-state'));
    expect(() => store.write(state())).toThrow('canonical');
    expect(fs.readdirSync(target)).toEqual([]);
  });

  test('refuses a hardlinked foreign settings file without modifying either path', () => {
    const value = fixture();
    fs.mkdirSync(value.directory, { mode: 0o700 });
    const target = path.join(value.root, 'foreign-settings.json');
    fs.writeFileSync(target, JSON.stringify(state()), { mode: 0o600 });
    fs.linkSync(target, value.store.file);
    const before = fs.readFileSync(target);
    expect(() => value.store.write(state())).toThrow('not private');
    expect(fs.readFileSync(target)).toEqual(before);
    expect(fs.readFileSync(value.store.file)).toEqual(before);
  });

  test('a replacement directory during staging receives no settings write or cleanup', () => {
    const value = fixture();
    value.store.write(state());
    const backup = path.join(value.root, 'previous-owned-state');
    const originalSync = fs.fsyncSync;
    let injected = false;
    const hook = spyOn(fs, 'fsyncSync').mockImplementation((descriptor) => {
      originalSync(descriptor);
      if (!injected) {
        injected = true;
        fs.renameSync(value.directory, backup);
        fs.mkdirSync(value.directory, { mode: 0o700 });
        fs.writeFileSync(value.store.file, 'foreign replacement sentinel', {
          mode: 0o600,
        });
      }
    });
    try {
      expect(() => value.store.write(state())).toThrow('directory changed');
    } finally {
      hook.mockRestore();
    }
    expect(fs.readFileSync(value.store.file, 'utf8')).toBe('foreign replacement sentinel');
    expect(fs.readdirSync(backup)).toEqual(['antigravity-auto-switch.json']);
    expect(
      JSON.parse(fs.readFileSync(path.join(backup, 'antigravity-auto-switch.json'), 'utf8'))
    ).toEqual(state());
  });

  test('a replaced settings inode during staging is preserved instead of overwritten', () => {
    const value = fixture();
    value.store.write(state());
    const replacement = path.join(value.directory, 'foreign.json');
    const foreign = {
      ...state(),
      settings: { ...state().settings, enabled: false },
    };
    fs.writeFileSync(replacement, JSON.stringify(foreign), { mode: 0o600 });
    const originalSync = fs.fsyncSync;
    let injected = false;
    const hook = spyOn(fs, 'fsyncSync').mockImplementation((descriptor) => {
      originalSync(descriptor);
      if (!injected) {
        injected = true;
        fs.renameSync(replacement, value.store.file);
      }
    });
    try {
      expect(() => value.store.write(state())).toThrow('settings changed');
    } finally {
      hook.mockRestore();
    }
    expect(value.store.read()).toEqual(foreign);
    expect(fs.readdirSync(value.directory)).toEqual(['antigravity-auto-switch.json']);
  });
});
