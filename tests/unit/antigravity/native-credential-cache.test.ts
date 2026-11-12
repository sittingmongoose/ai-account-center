import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { createHash } from 'crypto';
import { createUbuntuNativeCredentialStore } from '../../../src/antigravity/native-credential-transport';
import type { NativeCredential, VerifiedIdentity } from '../../../src/antigravity/types';
import type { AntigravitySwitchDriver } from '../../../src/antigravity/types';
import type { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import { AntigravitySwitchService } from '../../../src/antigravity/switch-service';
import { stopProcessBroker } from '../../../src/utils/managed-process';

const roots: string[] = [];
afterEach(async () => {
  await stopProcessBroker();
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});
function fixture() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-credential-cache-'));
  roots.push(home);
  const file = path.join(home, '.gemini/antigravity-cli/antigravity-oauth-token');
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  fs.writeFileSync(file, 'fixture-a', { mode: 0o600 });
  let calls = 0;
  let validations = 0;
  let now = Date.now();
  const store = createUbuntuNativeCredentialStore({
    home,
    now: () => now,
    invoke: async (packet) => {
      calls++;
      if (packet.operation !== 'read') throw new Error('fixture write refused');
      const bytes = fs.readFileSync(file);
      await new Promise((resolve) => setTimeout(resolve, 10));
      return {
        format: 'antigravity-consumer-json',
        credentialBase64: bytes.toString('base64'),
        revision: createHash('sha256').update(bytes).digest('hex'),
      };
    },
  });
  const validate = async (credential: NativeCredential): Promise<VerifiedIdentity> => {
    validations++;
    return {
      email: `${credential.bytes.toString()}@example.test`,
      subject: 'fixture-subject',
      plan: null,
      verifiedAt: new Date(now).toISOString(),
      source: 'provider-userinfo',
    };
  };
  return {
    home,
    file,
    store,
    validate,
    calls: () => calls,
    validations: () => validations,
    advance: () => {
      now += 2001;
    },
  };
}

describe.skipIf(process.platform === 'win32')('inventory credential identity cache', () => {
  it('does not join a plain credential read from before a file replacement through the real transport', async () => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-credential-overlap-'));
    roots.push(home);
    const file = path.join(home, '.gemini/antigravity-cli/antigravity-oauth-token');
    const started = path.join(home, 'plain-started');
    const release = path.join(home, 'release-plain');
    const script = path.join(home, 'read-fixture.py');
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    fs.writeFileSync(file, 'account-A', { mode: 0o600 });
    fs.writeFileSync(
      script,
      [
        'import sys,json,pathlib,base64,hashlib,time',
        'home=pathlib.Path(sys.argv[-1])',
        'json.load(sys.stdin)',
        "raw=(home/'.gemini/antigravity-cli/antigravity-oauth-token').read_bytes()",
        "with (home/'reads').open('a') as stream: stream.write(raw.decode()+'\\n')",
        "if raw == b'account-A':",
        " (home/'plain-started').touch()",
        ' deadline=time.monotonic()+2',
        " while not (home/'release-plain').exists() and time.monotonic()<deadline: time.sleep(.01)",
        "print(json.dumps({'format':'antigravity-consumer-json','credentialBase64':base64.b64encode(raw).decode(),'revision':hashlib.sha256(raw).hexdigest()}))",
      ].join('\n')
    );
    const store = createUbuntuNativeCredentialStore({ home, scriptPath: script });
    const validate = async (credential: NativeCredential): Promise<VerifiedIdentity> => ({
      email: `${credential.bytes.toString()}@example.test`,
      subject: 'fixture-subject',
      plan: null,
      verifiedAt: new Date().toISOString(),
      source: 'provider-userinfo',
    });
    const plain = store.read();
    try {
      for (let attempt = 0; attempt < 200 && !fs.existsSync(started); attempt++)
        await new Promise((resolve) => setTimeout(resolve, 10));
      expect(fs.existsSync(started)).toBe(true);
      fs.writeFileSync(`${file}.new`, 'account-B', { mode: 0o600 });
      fs.renameSync(`${file}.new`, file);
      const [guarded, inventories] = await Promise.all([
        store.read(),
        Promise.all(Array.from({ length: 5 }, () => store.readInventoryIdentity!(validate))),
      ]);
      const cached = await store.readInventoryIdentity!(validate);
      expect(guarded.bytes.toString()).toBe('account-B');
      expect(inventories.every((identity) => identity.email === 'account-B@example.test')).toBe(
        true
      );
      expect(cached.email).toBe('account-B@example.test');
      expect(fs.readFileSync(path.join(home, 'reads'), 'utf8').trim().split('\n')).toEqual([
        'account-A',
        'account-B',
        'account-B',
      ]);
    } finally {
      fs.writeFileSync(release, 'release');
      expect((await plain).bytes.toString()).toBe('account-A');
    }
  });
  it('reads no native credential when there are no saved profiles to mark selected', async () => {
    let reads = 0;
    const service = new AntigravitySwitchService({
      registry: { listProfiles: () => [] } as unknown as AntigravityProfileRegistry,
      driver: {
        hostId: 'ubuntu',
        readCurrentCredential: async () => {
          reads++;
          throw new Error('unused');
        },
      } as unknown as AntigravitySwitchDriver,
    });
    expect(await service.readInventory()).toEqual([]);
    expect(reads).toBe(0);
  });
  it('coalesces identical reads, retains only identity, expires and leaves guarded reads fresh', async () => {
    const f = fixture();
    const read = () => f.store.readInventoryIdentity!(f.validate);
    const results = await Promise.all(Array.from({ length: 10 }, read));
    expect(f.calls()).toBe(1);
    expect(f.validations()).toBe(1);
    results[0].email = 'changed@example.test';
    expect((await read()).email).toBe('fixture-a@example.test');
    fs.writeFileSync(path.join(f.home, 'unrelated-application-file'), 'fixture');
    await read();
    expect(f.calls()).toBe(1);
    expect(JSON.stringify(await read())).not.toContain('credentialBase64');
    f.advance();
    await read();
    expect(f.calls()).toBe(2);
    await f.store.read();
    await f.store.read();
    expect(f.calls()).toBe(4);
  });

  it('sees in-place writes and atomic replacements immediately, even with the old mtime', async () => {
    const f = fixture();
    const read = () => f.store.readInventoryIdentity!(f.validate);
    await read();
    const old = fs.statSync(f.file);
    fs.writeFileSync(f.file, 'fixture-b');
    fs.utimesSync(f.file, old.atime, old.mtime);
    expect((await read()).email).toBe('fixture-b@example.test');
    const replacement = `${f.file}.new`;
    fs.writeFileSync(replacement, 'fixture-c', { mode: 0o600 });
    fs.utimesSync(replacement, old.atime, old.mtime);
    fs.renameSync(replacement, f.file);
    expect((await read()).email).toBe('fixture-c@example.test');
    expect(f.calls()).toBe(3);
  });

  it('refuses permission, symlink and deletion changes instead of returning the cache', async () => {
    const f = fixture();
    const read = () => f.store.readInventoryIdentity!(f.validate);
    await read();
    fs.chmodSync(f.file, 0o644);
    await expect(read()).rejects.toThrow('antigravity-private-store-unavailable');
    fs.chmodSync(f.file, 0o600);
    await read();
    fs.renameSync(f.file, `${f.file}.saved`);
    fs.symlinkSync(`${f.file}.saved`, f.file);
    await expect(read()).rejects.toThrow('antigravity-private-store-unavailable');
    fs.unlinkSync(f.file);
    await expect(read()).rejects.toThrow('antigravity-private-store-unavailable');
  });

  it('never publishes an identity when its file changes during validation', async () => {
    const f = fixture();
    await expect(
      f.store.readInventoryIdentity!(async (credential) => {
        fs.writeFileSync(f.file, 'fixture-new');
        return f.validate(credential);
      })
    ).rejects.toThrow('antigravity-private-store-unavailable');
    expect((await f.store.readInventoryIdentity!(f.validate)).email).toBe(
      'fixture-new@example.test'
    );
  });
});
