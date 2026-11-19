/**
 * The paused-until-reviewed probe over temporary folders: the pin decision
 * and the version read are both injected; nothing native ever runs.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  isNativeVersion,
  readInstalledNativeVersion,
  readNativeRelease,
  readNativeUpdatePaused,
} from '../../../src/antigravity/native-version';

let root: string;
let home: string;
let ccsDir: string;
let releaseFile: string;

beforeEach(() => {
  root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-native-version-')));
  home = path.join(root, 'home');
  ccsDir = path.join(root, '.ccs');
  fs.mkdirSync(path.join(home, '.local', 'bin'), { recursive: true });
  fs.mkdirSync(ccsDir, { recursive: true });
  releaseFile = path.join(root, 'release.json');
  fs.writeFileSync(
    releaseFile,
    JSON.stringify({ schemaVersion: 1, nativeVersion: '1.2.16', nativeSha256: 'a'.repeat(64) })
  );
  const binary = path.join(home, '.local', 'bin', 'agy');
  fs.writeFileSync(binary, '#!/bin/sh\nexit 0\n');
  fs.chmodSync(binary, 0o755);
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

describe('native version review state', () => {
  it('accepts dotted releases with an optional tail only', () => {
    expect(isNativeVersion('1.2.16')).toBe(true);
    expect(isNativeVersion('1.2.16-rc.1')).toBe(true);
    expect(isNativeVersion('1.2')).toBe(false);
    expect(isNativeVersion('v1.2.16')).toBe(false);
    expect(isNativeVersion('1.2.16\nrm -rf /')).toBe(false);
    expect(isNativeVersion('x'.repeat(129))).toBe(false);
    expect(isNativeVersion(null)).toBe(false);
  });

  it('stays silent while the pin matches and never reads the version', async () => {
    let reads = 0;
    const paused = await readNativeUpdatePaused({
      home,
      ccsDir,
      releaseFile,
      pinMatches: () => true,
      readVersion: async () => {
        reads++;
        return '1.2.16';
      },
    });
    expect(paused).toBeNull();
    expect(reads).toBe(0);
  });

  it('reports the installed version once the pin stops matching', async () => {
    const paused = await readNativeUpdatePaused({
      home,
      ccsDir,
      releaseFile,
      pinMatches: () => false,
      readVersion: async () => '1.2.17',
    });
    expect(paused).toEqual({ installedVersion: '1.2.17' });
  });

  it('reports an unreadable version as null instead of failing', async () => {
    const paused = await readNativeUpdatePaused({
      home,
      ccsDir,
      releaseFile,
      pinMatches: () => false,
      readVersion: async () => {
        throw new Error('agy --version failed');
      },
    });
    expect(paused).toEqual({ installedVersion: null });
  });

  it('rejects a hostile version string from the reader', async () => {
    const paused = await readNativeUpdatePaused({
      home,
      ccsDir,
      releaseFile,
      pinMatches: () => false,
      readVersion: async () => '1.2.17; rm -rf /',
    });
    expect(paused).toEqual({ installedVersion: null });
    expect(await readInstalledNativeVersion(home, ccsDir, async () => '1.2')).toBeNull();
  });

  it('re-reads only when the binary changes', async () => {
    let reads = 0;
    const probe = {
      home,
      ccsDir,
      releaseFile,
      pinMatches: () => false,
      readVersion: async () => {
        reads++;
        return `1.2.${16 + reads}`;
      },
    };
    expect(await readNativeUpdatePaused(probe)).toEqual({ installedVersion: '1.2.17' });
    expect(await readNativeUpdatePaused(probe)).toEqual({ installedVersion: '1.2.17' });
    expect(reads).toBe(1);
    const binary = path.join(home, '.local', 'bin', 'agy');
    fs.writeFileSync(binary, '#!/bin/sh\n# updated build\nexit 1\n');
    fs.chmodSync(binary, 0o755);
    expect(await readNativeUpdatePaused(probe)).toEqual({ installedVersion: '1.2.18' });
    expect(reads).toBe(2);
  });

  it('stays silent without a binary, without a pin, or when the probe throws', async () => {
    fs.rmSync(path.join(home, '.local', 'bin', 'agy'));
    expect(
      await readNativeUpdatePaused({
        home,
        ccsDir,
        releaseFile,
        pinMatches: () => false,
        readVersion: async () => '1.2.17',
      })
    ).toBeNull();
    const binary = path.join(home, '.local', 'bin', 'agy');
    fs.writeFileSync(binary, '#!/bin/sh\nexit 0\n');
    fs.chmodSync(binary, 0o755);
    expect(
      await readNativeUpdatePaused({
        home,
        ccsDir,
        releaseFile,
        pinMatches: () => {
          throw new Error('pin reader failed');
        },
      })
    ).toBeNull();
    fs.writeFileSync(releaseFile, JSON.stringify({ schemaVersion: 1 }));
    expect(
      await readNativeUpdatePaused({
        home,
        ccsDir,
        releaseFile,
        pinMatches: () => false,
        readVersion: async () => '1.2.17',
      })
    ).toBeNull();
  });

  it('stays silent when the binary matches any reviewed entry, pausing otherwise', async () => {
    fs.writeFileSync(
      releaseFile,
      JSON.stringify({
        schemaVersion: 1,
        nativeActivationReleased: true,
        nativeProofReceiptSha256: 'b'.repeat(64),
        reviewedNatives: [
          { nativeVersion: '1.2.14', nativeSha256: 'a'.repeat(64) },
          { nativeVersion: '1.2.16', nativeSha256: 'c'.repeat(64) },
        ],
      })
    );
    let reads = 0;
    const probe = (match: string) => ({
      home,
      ccsDir,
      releaseFile,
      pinMatches: (binary: string, sha: string) => sha === match,
      readVersion: async () => {
        reads++;
        return '1.2.16';
      },
    });
    expect(await readNativeUpdatePaused(probe('c'.repeat(64)))).toBeNull();
    expect(reads).toBe(0);
    // A fresh binary identity re-proves instead of reusing the cached verdict.
    const binary = path.join(home, '.local', 'bin', 'agy');
    fs.writeFileSync(binary, '#!/bin/sh\n# replaced build\nexit 0\n');
    fs.chmodSync(binary, 0o755);
    expect(await readNativeUpdatePaused(probe('d'.repeat(64)))).toEqual({
      installedVersion: '1.2.16',
    });
    expect(reads).toBe(1);
  });

  it('caches a matched verdict without re-proving the binary', async () => {
    let proofs = 0;
    const probe = {
      home,
      ccsDir,
      releaseFile,
      pinMatches: () => {
        proofs++;
        return true;
      },
    };
    expect(await readNativeUpdatePaused(probe)).toBeNull();
    expect(await readNativeUpdatePaused(probe)).toBeNull();
    expect(proofs).toBe(1);
  });

  it('reads the gate and the reviewed set strictly', () => {
    const gate = { nativeActivationReleased: true, nativeProofReceiptSha256: 'b'.repeat(64) };
    const entries = [
      { nativeVersion: '1.2.14', nativeSha256: 'a'.repeat(64) },
      { nativeVersion: '1.2.16', nativeSha256: 'c'.repeat(64) },
    ];
    fs.writeFileSync(releaseFile, JSON.stringify({ schemaVersion: 1, ...gate, reviewedNatives: entries }));
    expect(readNativeRelease(releaseFile)).toEqual({
      gateOpen: true,
      reviewed: [
        { version: '1.2.14', sha256: 'a'.repeat(64) },
        { version: '1.2.16', sha256: 'c'.repeat(64) },
      ],
    });
    fs.writeFileSync(
      releaseFile,
      JSON.stringify({ schemaVersion: 1, ...gate, nativeVersion: '1.2.16', nativeSha256: 'c'.repeat(64) })
    );
    expect(readNativeRelease(releaseFile)).toEqual({
      gateOpen: true,
      reviewed: [{ version: '1.2.16', sha256: 'c'.repeat(64) }],
    });
    for (const reviewedNatives of [
      [],
      [{ nativeVersion: '1.2.16', nativeSha256: 'c'.repeat(64), extra: 1 }],
      [
        { nativeVersion: '1.2.16', nativeSha256: 'c'.repeat(64) },
        { nativeVersion: '1.2.16', nativeSha256: 'a'.repeat(64) },
      ],
      [
        { nativeVersion: '1.2.16', nativeSha256: 'c'.repeat(64) },
        { nativeVersion: '1.2.14', nativeSha256: 'c'.repeat(64) },
      ],
      [{ nativeVersion: '1.2', nativeSha256: 'c'.repeat(64) }],
      '1.2.16',
      Array.from({ length: 17 }, (_, index) => ({
        nativeVersion: `1.2.${index}`,
        nativeSha256: `${index}`.padStart(64, '0'),
      })),
    ]) {
      fs.writeFileSync(releaseFile, JSON.stringify({ schemaVersion: 1, ...gate, reviewedNatives }));
      expect(readNativeRelease(releaseFile)).toEqual({ gateOpen: true, reviewed: [] });
    }
    fs.writeFileSync(releaseFile, JSON.stringify({ schemaVersion: 1 }));
    expect(readNativeRelease(releaseFile)).toEqual({ gateOpen: false, reviewed: [] });
    expect(readNativeRelease(path.join(root, 'missing.json'))).toEqual({
      gateOpen: false,
      reviewed: [],
    });
  });
});
