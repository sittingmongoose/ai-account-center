import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  hashUsageHubKey,
  isUsageHubKeyShape,
  readUsageHubKeyState,
  removeUsageHubKey,
  usageHubKeyMatches,
  usageHubKeyPath,
  writeUsageHubKey,
  UsageHubKeyExistsError,
} from '../../../src/web-server/usage-hub/usage-hub-key-store';

let tempHome = '';
let previous: Record<'CCS_HOME' | 'CCS_DIR', string | undefined> = {
  CCS_HOME: undefined,
  CCS_DIR: undefined,
};

beforeEach(() => {
  tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-usage-hub-key-'));
  previous = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
  delete process.env.CCS_DIR;
  process.env.CCS_HOME = tempHome;
});

afterEach(() => {
  for (const [name, value] of Object.entries(previous)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  fs.rmSync(tempHome, { recursive: true, force: true });
});

describe('usage hub key store', () => {
  it('is off until a key is generated', async () => {
    expect(await readUsageHubKeyState()).toEqual({ state: 'off' });
    expect(usageHubKeyPath().startsWith(tempHome)).toBe(true);
  });

  it('stores only the SHA-256 of a strong key, in a private file', async () => {
    const key = await writeUsageHubKey({ replace: false });
    expect(isUsageHubKeyShape(key)).toBe(true);
    const contents = fs.readFileSync(usageHubKeyPath(), 'utf8');
    expect(contents).not.toContain(key);
    expect(contents).not.toContain(key.slice(5));
    expect(JSON.parse(contents).keySha256).toBe(hashUsageHubKey(key));
    if (process.platform !== 'win32') {
      expect(fs.statSync(usageHubKeyPath()).mode & 0o777).toBe(0o600);
      expect(fs.statSync(path.dirname(usageHubKeyPath())).mode & 0o077).toBe(0);
    }
    const state = await readUsageHubKeyState();
    expect(state.state).toBe('on');
    if (state.state !== 'on') throw new Error('expected on');
    expect(usageHubKeyMatches(key, state.record)).toBe(true);
    expect(usageHubKeyMatches(`${key}x`, state.record)).toBe(false);
    expect(usageHubKeyMatches('', state.record)).toBe(false);
    expect(usageHubKeyMatches('x'.repeat(10_000), state.record)).toBe(false);
  });

  it('keeps an existing key unless asked to rotate, and a rotated key retires the old one', async () => {
    const first = await writeUsageHubKey({ replace: false });
    await expect(writeUsageHubKey({ replace: false })).rejects.toBeInstanceOf(
      UsageHubKeyExistsError
    );
    const second = await writeUsageHubKey({ replace: true });
    expect(second).not.toBe(first);
    const state = await readUsageHubKeyState();
    if (state.state !== 'on') throw new Error('expected on');
    expect(usageHubKeyMatches(first, state.record)).toBe(false);
    expect(usageHubKeyMatches(second, state.record)).toBe(true);
  });

  it('turns off when the key is removed', async () => {
    await writeUsageHubKey({ replace: false });
    expect(await removeUsageHubKey()).toBe(true);
    expect(await removeUsageHubKey()).toBe(false);
    expect(await readUsageHubKeyState()).toEqual({ state: 'off' });
  });

  it('treats a malformed or loosely permitted file as invalid, never as a key', async () => {
    await writeUsageHubKey({ replace: false });
    fs.writeFileSync(usageHubKeyPath(), JSON.stringify({ version: 1, keySha256: 'short' }), {
      mode: 0o600,
    });
    expect(await readUsageHubKeyState()).toEqual({ state: 'invalid' });
    if (process.platform !== 'win32') {
      await writeUsageHubKey({ replace: true });
      fs.chmodSync(usageHubKeyPath(), 0o644);
      expect(await readUsageHubKeyState()).toEqual({ state: 'invalid' });
    }
  });
});
