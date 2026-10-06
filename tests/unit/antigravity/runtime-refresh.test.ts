/**
 * The descriptor refresh over temporary folders: the pin decision is
 * injected, so no native binary is hashed and nothing outside tmp is touched.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  createInstalledAntigravityComponents,
  readInstalledAntigravityRuntime,
} from '../../../src/antigravity/production-runtime';
import {
  refreshRuntimeDescriptor,
  type RuntimeRefreshResult,
} from '../../../src/antigravity/runtime-refresh';

const V14 = '1.2.14';
const V16 = '1.2.16';
const SHA14 = '0d0d3eba22daf29504dd290151c7ed9a4d33b0c6aa0acfc5da27bc3b01d2f029';
const SHA16 = 'a759ce7c7a235d9b6c281a25ead97cbbf2e92314a3ffd224e2f9144f3fae7a86';
const V130 = '1.3.0';
const SHA130 = '19be6af38f7beeaa0db415df9297e314ab3d33fdd6f853434d49f88819bc68e4';

let root: string;
let home: string;
let ccsDir: string;
let stateDir: string;
let descriptorFile: string;
let releaseFile: string;
let bundleLib: string;

function writeRelease(reviewed: Array<{ nativeVersion: string; nativeSha256: string }> | null): void {
  fs.writeFileSync(
    releaseFile,
    JSON.stringify({
      schemaVersion: 1,
      nativeActivationReleased: true,
      nativeVersion: V16,
      nativeSha256: SHA16,
      nativeProofReceiptSha256: 'b'.repeat(64),
      ...(reviewed === null ? {} : { reviewedNatives: reviewed }),
    })
  );
}

function writeDescriptor(pin: string): Buffer {
  const record = {
    schemaVersion: 1,
    bundleDirectory: path.join(
      home,
      '.local/share/ai-account-center/antigravity-runtime/bundles',
      'c'.repeat(64)
    ),
    nativeBinary: path.join(home, '.local/bin/agy'),
    nativeSha256: pin,
    socketPath: path.join(ccsDir, 'antigravity-runtime/control.sock'),
  };
  const raw = Buffer.from(`${JSON.stringify(record)}\n`);
  fs.writeFileSync(descriptorFile, raw, { mode: 0o600 });
  fs.chmodSync(descriptorFile, 0o600);
  return raw;
}

function writeBundleGate(open: boolean): void {
  fs.writeFileSync(
    path.join(bundleLib, 'release.json'),
    JSON.stringify({
      nativeActivationReleased: open,
      nativeProofReceiptSha256: open ? 'd'.repeat(64) : null,
    })
  );
}

beforeEach(() => {
  root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-refresh-')));
  home = path.join(root, 'home');
  ccsDir = path.join(root, '.ccs');
  stateDir = path.join(ccsDir, 'antigravity-switching');
  descriptorFile = path.join(stateDir, 'runtime-installation.json');
  releaseFile = path.join(root, 'release.json');
  bundleLib = path.join(
    home,
    '.local/share/ai-account-center/antigravity-runtime/bundles',
    'c'.repeat(64),
    'lib'
  );
  fs.mkdirSync(path.join(home, '.local', 'bin'), { recursive: true });
  fs.mkdirSync(stateDir, { recursive: true, mode: 0o700 });
  fs.chmodSync(stateDir, 0o700);
  fs.mkdirSync(bundleLib, { recursive: true });
  const binary = path.join(home, '.local', 'bin', 'agy');
  fs.writeFileSync(binary, '#!/bin/sh\nexit 0\n');
  fs.chmodSync(binary, 0o755);
  writeRelease([
    { nativeVersion: V14, nativeSha256: SHA14 },
    { nativeVersion: V16, nativeSha256: SHA16 },
  ]);
  writeDescriptor(SHA14);
  writeBundleGate(true);
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

function refresh(
  pinMatches: (binary: string, sha256: string) => boolean,
  extra: { readVersion?: () => Promise<string | null> } = {}
): Promise<RuntimeRefreshResult> {
  return refreshRuntimeDescriptor({
    ccsDir,
    home,
    releaseFile,
    pinMatches,
    readVersion: extra.readVersion ?? (async () => null),
  });
}

describe('runtime descriptor refresh', () => {
  it('leaves a current descriptor untouched without writing a backup', async () => {
    const before = fs.readFileSync(descriptorFile);
    const result = await refresh((binary, sha) => sha === SHA14);
    expect(result).toEqual({ status: 'current', version: V14, sha256: SHA14 });
    expect(fs.readFileSync(descriptorFile).equals(before)).toBe(true);
    expect(fs.existsSync(path.join(stateDir, 'descriptor-backups'))).toBe(false);
  });

  it('re-pins an old descriptor to a reviewed native atomically with a backup', async () => {
    const before = fs.readFileSync(descriptorFile);
    const result = await refresh((binary, sha) => sha === SHA16);
    expect(result).toEqual({
      status: 'refreshed',
      version: V16,
      sha256: SHA16,
      previousSha256: SHA14,
    });
    const reread = readInstalledAntigravityRuntime(ccsDir, home);
    expect(reread?.nativeSha256).toBe(SHA16);
    expect(reread?.nativeBinary).toBe(path.join(home, '.local/bin/agy'));
    expect(reread?.socketPath).toBe(path.join(ccsDir, 'antigravity-runtime/control.sock'));
    expect(fs.lstatSync(descriptorFile).mode & 0o777).toBe(0o600);
    const backup = path.join(stateDir, 'descriptor-backups', `${SHA14}.json`);
    expect(fs.readFileSync(backup).equals(before)).toBe(true);
    expect(fs.lstatSync(backup).mode & 0o777).toBe(0o600);
    expect(fs.lstatSync(path.dirname(backup)).mode & 0o777).toBe(0o700);
    expect(fs.readdirSync(stateDir).filter((name) => name.includes('.tmp-'))).toEqual([]);
  });

  it('pauses on an unreviewed native without touching the descriptor', async () => {
    const before = fs.readFileSync(descriptorFile);
    let reads = 0;
    const result = await refresh(
      () => false,
      {
        readVersion: async () => {
          reads++;
          return '1.2.99';
        },
      }
    );
    expect(result).toEqual({ status: 'unreviewed', installedVersion: '1.2.99' });
    expect(fs.readFileSync(descriptorFile).equals(before)).toBe(true);
    expect(fs.existsSync(path.join(stateDir, 'descriptor-backups'))).toBe(false);
    // The refusal is cached by binary identity: no re-proof, no re-read.
    let proofs = 0;
    const again = await refreshRuntimeDescriptor({
      ccsDir,
      home,
      releaseFile,
      pinMatches: () => {
        proofs++;
        return true;
      },
      readVersion: async () => {
        reads++;
        return '9.9.9';
      },
    });
    expect(again).toEqual({ status: 'unreviewed', installedVersion: '1.2.99' });
    expect(proofs).toBe(0);
    expect(reads).toBe(1);
  });

  it('re-proves after the binary changes', async () => {
    await refresh(() => false);
    const binary = path.join(home, '.local', 'bin', 'agy');
    fs.writeFileSync(binary, '#!/bin/sh\n# replaced build\nexit 0\n');
    fs.chmodSync(binary, 0o755);
    const result = await refresh((candidate, sha) => sha === SHA16);
    expect(result.status).toBe('refreshed');
  });

  it('reports a missing or broken installation without writing', async () => {
    fs.rmSync(descriptorFile);
    expect((await refresh(() => true)).status).toBe('no-installation');
    fs.writeFileSync(descriptorFile, '{"schemaVersion":1}\n', { mode: 0o600 });
    expect((await refresh(() => true)).status).toBe('no-installation');
    expect(fs.existsSync(path.join(stateDir, 'descriptor-backups'))).toBe(false);
  });

  it('refuses when either release gate is closed', async () => {
    const before = fs.readFileSync(descriptorFile);
    fs.writeFileSync(
      releaseFile,
      JSON.stringify({ schemaVersion: 1, nativeActivationReleased: false })
    );
    expect((await refresh(() => true)).status).toBe('gate-closed');
    writeRelease([
      { nativeVersion: V14, nativeSha256: SHA14 },
      { nativeVersion: V16, nativeSha256: SHA16 },
    ]);
    writeBundleGate(false);
    expect((await refresh(() => true)).status).toBe('gate-closed');
    expect(fs.readFileSync(descriptorFile).equals(before)).toBe(true);
  });

  it('fails safe when the pin proof throws or the set is malformed', async () => {
    const before = fs.readFileSync(descriptorFile);
    expect(
      (
        await refresh(() => {
          throw new Error('pin reader failed');
        })
      ).status
    ).toBe('failed');
    for (const reviewed of [
      [{ nativeVersion: V16, nativeSha256: SHA16, extra: 1 }],
      [
        { nativeVersion: V16, nativeSha256: SHA16 },
        { nativeVersion: V16, nativeSha256: SHA14 },
      ],
      [{ nativeVersion: '1.2', nativeSha256: SHA16 }],
      [{ nativeVersion: V16, nativeSha256: 'zz' }],
      [],
    ]) {
      writeRelease(reviewed as Array<{ nativeVersion: string; nativeSha256: string }>);
      expect((await refresh(() => true)).status).toBe('failed');
    }
    expect(fs.readFileSync(descriptorFile).equals(before)).toBe(true);
  });

  it('reads a legacy single-pin release as a one-entry set', async () => {
    writeRelease(null);
    expect(await refresh((binary, sha) => sha === SHA16)).toEqual({
      status: 'refreshed',
      version: V16,
      sha256: SHA16,
      previousSha256: SHA14,
    });
  });

  it('pauses a legacy release when the binary matches no pin', async () => {
    writeRelease(null);
    expect(await refresh((binary, sha) => sha === SHA14)).toEqual({
      status: 'unreviewed',
      installedVersion: null,
    });
  });

  it('reuses an identical backup and keeps a divergent one beside a content-addressed copy', async () => {
    const backups = path.join(stateDir, 'descriptor-backups');
    fs.mkdirSync(backups, { mode: 0o700 });
    const backup = path.join(backups, `${SHA14}.json`);
    fs.writeFileSync(backup, fs.readFileSync(descriptorFile), { mode: 0o600 });
    expect((await refresh((binary, sha) => sha === SHA16)).status).toBe('refreshed');
    expect(fs.readdirSync(backups)).toEqual([`${SHA14}.json`]);
    writeDescriptor(SHA14);
    const divergent = Buffer.from('{"divergent":true}\n');
    fs.writeFileSync(backup, divergent);
    const before = fs.readFileSync(descriptorFile);
    expect((await refresh((binary, sha) => sha === SHA16)).status).toBe('refreshed');
    // The earlier generation's pin-named backup is never overwritten.
    expect(fs.readFileSync(backup).equals(divergent)).toBe(true);
    const named = fs.readdirSync(backups).filter((name) => name !== `${SHA14}.json`);
    expect(named).toEqual([`${SHA14}-${createHash('sha256').update(before).digest('hex').slice(0, 16)}.json`]);
    expect(fs.readFileSync(path.join(backups, named[0])).equals(before)).toBe(true);
    expect(fs.lstatSync(path.join(backups, named[0])).mode & 0o777).toBe(0o600);
    expect(readInstalledAntigravityRuntime(ccsDir, home)?.nativeSha256).toBe(SHA16);
  });

  it('refreshes over a rebuilt bundle whose pin-named backup holds an older bundle (the live 1.3.0 state)', async () => {
    // Oct 4/5 shape: the pin-named backup of the reviewed pin keeps the
    // descriptor of an earlier bundle, a parser rebuild added a content-
    // addressed one, and the CLI then moved to the next reviewed build.
    const backups = path.join(stateDir, 'descriptor-backups');
    fs.mkdirSync(backups, { mode: 0o700 });
    const older = Buffer.from(fs.readFileSync(descriptorFile).toString().replace('c'.repeat(64), 'e'.repeat(64)));
    fs.writeFileSync(path.join(backups, `${SHA16}.json`), older, { mode: 0o600 });
    fs.writeFileSync(path.join(backups, `${SHA16}-${'f'.repeat(16)}.json`), older, { mode: 0o600 });
    writeRelease([
      { nativeVersion: V14, nativeSha256: SHA14 },
      { nativeVersion: V16, nativeSha256: SHA16 },
      { nativeVersion: V130, nativeSha256: SHA130 },
    ]);
    const before = writeDescriptor(SHA16);
    expect(await refresh((binary, sha) => sha === SHA130)).toEqual({
      status: 'refreshed',
      version: V130,
      sha256: SHA130,
      previousSha256: SHA16,
    });
    expect(fs.readFileSync(path.join(backups, `${SHA16}.json`)).equals(older)).toBe(true);
    const kept = path.join(backups, `${SHA16}-${createHash('sha256').update(before).digest('hex').slice(0, 16)}.json`);
    expect(fs.readFileSync(kept).equals(before)).toBe(true);
    expect(readInstalledAntigravityRuntime(ccsDir, home)?.nativeSha256).toBe(SHA130);
    expect((await refresh((binary, sha) => sha === SHA130)).status).toBe('current');
  });

  it('refuses an unsafe pin-named backup or a changed content-addressed one without writing', async () => {
    const backups = path.join(stateDir, 'descriptor-backups');
    fs.mkdirSync(backups, { mode: 0o700 });
    const backup = path.join(backups, `${SHA14}.json`);
    fs.writeFileSync(backup, '{"divergent":true}\n', { mode: 0o644 });
    fs.chmodSync(backup, 0o644);
    const before = fs.readFileSync(descriptorFile);
    expect((await refresh((binary, sha) => sha === SHA16)).status).toBe('failed');
    fs.chmodSync(backup, 0o600);
    const addressed = path.join(backups, `${SHA14}-${createHash('sha256').update(before).digest('hex').slice(0, 16)}.json`);
    fs.writeFileSync(addressed, '{"changed":true}\n', { mode: 0o600 });
    expect((await refresh((binary, sha) => sha === SHA16)).status).toBe('failed');
    fs.rmSync(addressed);
    fs.symlinkSync('/nonexistent', addressed);
    expect((await refresh((binary, sha) => sha === SHA16)).status).toBe('failed');
    expect(fs.readFileSync(descriptorFile).equals(before)).toBe(true);
    expect(fs.readdirSync(stateDir).filter((name) => name.includes('.tmp-'))).toEqual([]);
  });

  it('converges when a rival rewrite settles instead of clobbering it', async () => {
    let flips = 0;
    const pinMatches = (binary: string, sha: string) => {
      if (flips === 0) {
        flips++;
        writeDescriptor(SHA16);
      }
      return sha === SHA16;
    };
    expect(await refresh(pinMatches)).toEqual({
      status: 'current',
      version: V16,
      sha256: SHA16,
    });
    // The conflict was detected before any backup: no leftover pin backup.
    expect(fs.existsSync(path.join(stateDir, 'descriptor-backups'))).toBe(false);
    expect(readInstalledAntigravityRuntime(ccsDir, home)?.nativeSha256).toBe(SHA16);
  });

  it('opens the release gate through the injected refresh verdict', async () => {
    const gate = (status: RuntimeRefreshResult['status']) =>
      createInstalledAntigravityComponents(ccsDir, home, {
        refreshDescriptor: async () =>
          (status === 'refreshed'
            ? { status, version: V16, sha256: SHA16, previousSha256: SHA14 }
            : status === 'current'
              ? { status, version: V16, sha256: SHA16 }
              : status === 'unreviewed'
                ? { status, installedVersion: '1.2.99' }
                : { status }) as RuntimeRefreshResult,
      })?.driver.canRestartSupportedNative();
    expect(await gate('current')).toBe(true);
    expect(await gate('refreshed')).toBe(true);
    expect(await gate('unreviewed')).toBe(false);
    expect(await gate('no-installation')).toBe(false);
    expect(await gate('gate-closed')).toBe(false);
    expect(await gate('failed')).toBe(false);
  });

  it('refuses the gate on fixture directories without writing or spawning', async () => {
    const before = fs.readFileSync(descriptorFile);
    const components = createInstalledAntigravityComponents(ccsDir, home);
    expect(components).not.toBeNull();
    expect(await components?.driver.canRestartSupportedNative()).toBe(false);
    expect(fs.readFileSync(descriptorFile).equals(before)).toBe(true);
    expect(fs.existsSync(path.join(stateDir, 'descriptor-backups'))).toBe(false);
    fs.rmSync(descriptorFile);
    expect(createInstalledAntigravityComponents(ccsDir, home)).toBeNull();
  });

  it('repairs a settled hostile pin when the binary is reviewed', async () => {
    const hostilePin = 'e'.repeat(64);
    let flips = 0;
    const pinMatches = (binary: string, sha: string) => {
      if (flips === 0) {
        flips++;
        writeDescriptor(hostilePin);
      }
      return sha === SHA16;
    };
    const result = await refresh(pinMatches);
    expect(result).toEqual({
      status: 'refreshed',
      version: V16,
      sha256: SHA16,
      previousSha256: hostilePin,
    });
    expect(readInstalledAntigravityRuntime(ccsDir, home)?.nativeSha256).toBe(SHA16);
  });
});
