/**
 * The native version reads over temporary folders: the version read is
 * injected; nothing native ever runs.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  isNativeVersion,
  readInstalledNativeVersion,
  readNativeRelease,
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

  it('reports only a well-shaped installed version and never throws', async () => {
    expect(await readInstalledNativeVersion(home, ccsDir, async () => '1.2.17')).toBe('1.2.17');
    expect(await readInstalledNativeVersion(home, ccsDir, async () => '1.2')).toBeNull();
    expect(
      await readInstalledNativeVersion(home, ccsDir, async () => {
        throw new Error('agy --version failed');
      })
    ).toBeNull();
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
