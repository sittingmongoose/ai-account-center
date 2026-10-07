/**
 * The AAC-owned API-key store (CONTRACT-registry-lifecycle section 3.2):
 * 0600 files in a 0700 folder, last4 and fingerprint only, and the remote
 * helper getting the secret on stdin only. Temporary folders only.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  keyFingerprint,
  keyHelperCommand,
  keyLast4,
  keyStoreFor,
  LocalKeyStore,
  RemoteKeyStore,
} from '../../../src/web-server/services/account-key-store';

const SECRET = 'zai-TEST-key-0123456789-x7Qa';
const dirs: string[] = [];
afterEach(() => {
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

function ccsDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-keys-'));
  dirs.push(dir);
  return dir;
}

describe('local key store', () => {
  it('writes a 0600 record in a 0700 folder and returns only last4 and fingerprint', async () => {
    const dir = ccsDir();
    const store = new LocalKeyStore(dir);
    const info = await store.put('zai', '9f2c41d0', SECRET);
    expect(info).toEqual({
      fingerprint: keyFingerprint(SECRET),
      last4: 'x7Qa',
      storedOn: 'ubuntu',
    });
    expect(info.fingerprint).toMatch(/^sha256:[a-f0-9]{16}$/);
    expect(JSON.stringify(info)).not.toContain(SECRET);
    const folder = path.join(dir, 'account-usage', 'keys');
    const file = path.join(folder, 'zai-9f2c41d0.json');
    expect(fs.statSync(folder).mode & 0o777).toBe(0o700);
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    const record = JSON.parse(fs.readFileSync(file, 'utf8'));
    expect(Object.keys(record).sort()).toEqual(
      ['createdAt', 'fingerprint', 'keyId', 'last4', 'provider', 'secret', 'version'].sort()
    );
    expect(record).toMatchObject({
      version: 1,
      provider: 'zai',
      keyId: '9f2c41d0',
      secret: SECRET,
    });
    expect(record.createdAt).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
    expect(fs.readdirSync(folder)).toEqual(['zai-9f2c41d0.json']);
    expect(await store.info('zai', '9f2c41d0')).toEqual(info);
    expect([...(await store.fingerprints('zai'))]).toEqual([info.fingerprint]);
    expect([...(await store.fingerprints('kimi-code'))]).toEqual([]);
  });

  it('replaces atomically, deletes, and tightens a loose folder', async () => {
    const dir = ccsDir();
    const folder = path.join(dir, 'account-usage', 'keys');
    fs.mkdirSync(folder, { recursive: true });
    fs.chmodSync(folder, 0o755);
    const store = new LocalKeyStore(dir);
    await store.put('opencode-go', 'aaaaaaaa', 'first-secret-1');
    expect(fs.statSync(folder).mode & 0o777).toBe(0o700);
    await store.put('opencode-go', 'aaaaaaaa', 'second-secret-2');
    expect((await store.info('opencode-go', 'aaaaaaaa'))?.last4).toBe('et-2');
    await store.delete('opencode-go', 'aaaaaaaa');
    expect(await store.info('opencode-go', 'aaaaaaaa')).toBeNull();
    await store.delete('opencode-go', 'aaaaaaaa');
  });

  it('refuses bad ids, bad secrets and a symlinked folder, and reads nothing unsafe', async () => {
    const dir = ccsDir();
    const store = new LocalKeyStore(dir);
    await expect(store.put('zai', 'NOT-HEX', SECRET)).rejects.toThrow();
    await expect(store.put('zai', '9f2c41d0', 'short')).rejects.toThrow();
    await expect(store.put('zai', '9f2c41d0', 'has space inside')).rejects.toThrow();
    await expect(store.put('qwen' as never, '9f2c41d0', SECRET)).rejects.toThrow();
    const linked = ccsDir();
    const target = ccsDir();
    fs.mkdirSync(path.join(linked, 'account-usage'), { recursive: true });
    fs.symlinkSync(target, path.join(linked, 'account-usage', 'keys'));
    await expect(new LocalKeyStore(linked).put('zai', '9f2c41d0', SECRET)).rejects.toThrow();
    expect(fs.readdirSync(target)).toEqual([]);
    // A record readable by others is never reported.
    await store.put('zai', '9f2c41d0', SECRET);
    fs.chmodSync(path.join(dir, 'account-usage', 'keys', 'zai-9f2c41d0.json'), 0o644);
    expect(await store.info('zai', '9f2c41d0')).toBeNull();
  });
});

describe('remote key store', () => {
  it('sends the secret on stdin only and parses the helper reply', async () => {
    const calls: Array<{ host: string; command: string; input: string | null }> = [];
    const store = new RemoteKeyStore('mac', 'mac-host', async (host, command, input) => {
      calls.push({ host, command, input });
      return input
        ? `${JSON.stringify({ ok: true, fingerprint: keyFingerprint(input), last4: keyLast4(input) })}\n`
        : '{"ok":true}\n';
    });
    expect(await store.put('kimi-code', '0badc0de', SECRET)).toEqual({
      fingerprint: keyFingerprint(SECRET),
      last4: 'x7Qa',
      storedOn: 'mac',
    });
    await store.delete('kimi-code', '0badc0de');
    expect(calls).toEqual([
      {
        host: 'mac-host',
        command:
          "/usr/bin/python3 \"$HOME/.ccs/account-usage/key_store.py\" put --provider 'kimi-code' --key-id '0badc0de'",
        input: SECRET,
      },
      {
        host: 'mac-host',
        command:
          "/usr/bin/python3 \"$HOME/.ccs/account-usage/key_store.py\" delete --provider 'kimi-code' --key-id '0badc0de'",
        input: null,
      },
    ]);
    for (const call of calls) expect(call.command).not.toContain(SECRET);
  });

  it('rejects a reply that is not exactly the helper shape or does not match the key', async () => {
    for (const reply of [
      'not json',
      '{"ok":true}',
      JSON.stringify({ ok: true, fingerprint: keyFingerprint(SECRET), last4: 'x7Qa', extra: 1 }),
      JSON.stringify({ ok: true, fingerprint: keyFingerprint('other-secret'), last4: 'x7Qa' }),
    ]) {
      const store = new RemoteKeyStore('mac', 'mac-host', async () => reply);
      await expect(store.put('zai', '9f2c41d0', SECRET)).rejects.toThrow();
    }
  });

  it('builds a Windows command with no secret and only enumerated values', () => {
    const command = keyHelperCommand('windows', 'put', 'zai', '9f2c41d0');
    const script = Buffer.from(command.split(' ').pop() ?? '', 'base64').toString('utf16le');
    expect(script).toContain("key_store.py')");
    expect(script).toContain("put --provider 'zai' --key-id '9f2c41d0'");
    expect(() => keyHelperCommand('mac', 'put', 'zai', "9f2c41d0'; rm -rf ~")).toThrow();
  });

  it('chooses the local store for the dashboard host and ssh for an aliased host', () => {
    const dir = ccsDir();
    expect(keyStoreFor({ platform: 'ubuntu', sshHost: null }, { ccsDir: dir })).toBeInstanceOf(
      LocalKeyStore
    );
    expect(keyStoreFor({ platform: 'mac', sshHost: 'mac-host' }, { ccsDir: dir })).toBeInstanceOf(
      RemoteKeyStore
    );
    expect(keyStoreFor({ platform: 'mac', sshHost: null }, { ccsDir: dir })).toBeNull();
    expect(keyStoreFor({ platform: 'mac', sshHost: 'bad alias' }, { ccsDir: dir })).toBeNull();
  });
});
