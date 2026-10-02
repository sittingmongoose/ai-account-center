import { afterEach, expect, test } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AntigravityProfileRegistry,
  PrivateStorageError,
  pruneSupersededCredentials,
} from '../../../src/antigravity';
import type { NativeCredential, VerifiedIdentity } from '../../../src/antigravity';

const temporary: string[] = [];
afterEach(() => {
  for (const directory of temporary.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});

function generation(seed: number): string {
  return String(seed).padStart(64, '0');
}

function hostDirectory(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-retention-'));
  temporary.push(root);
  const directory = path.join(root, 'ubuntu');
  fs.mkdirSync(directory, { mode: 0o700 });
  return directory;
}

function writeGeneration(
  directory: string,
  fingerprint: string,
  ageMs: number,
  mode = 0o600
): string {
  const name = `credential-${fingerprint}.bin`;
  const target = path.join(directory, name);
  fs.writeFileSync(target, Buffer.from(`fixture-generation-${fingerprint}`));
  fs.chmodSync(target, mode);
  const stamped = new Date(Date.now() - ageMs);
  fs.utimesSync(target, stamped, stamped);
  return name;
}

test('five generations keep only the current file plus one previous copy', () => {
  const directory = hostDirectory();
  const names = [1, 2, 3, 4, 5].map((seed) =>
    writeGeneration(directory, generation(seed), (6 - seed) * 60_000)
  );
  const report = pruneSupersededCredentials(directory, generation(5));
  expect(fs.readdirSync(directory).sort()).toEqual([names[3], names[4]].sort());
  expect(report.deleted.sort()).toEqual([names[0], names[1], names[2]].sort());
  expect(report.skipped).toEqual([]);
});

test('the recorded previous generation survives even when a newer orphan exists', () => {
  const directory = hostDirectory();
  // A crashed save can leave an unpublished generation newer than the recorded previous.
  const recorded = writeGeneration(directory, generation(1), 120_000);
  const orphan = writeGeneration(directory, generation(2), 60_000);
  const current = writeGeneration(directory, generation(3), 0);
  const report = pruneSupersededCredentials(directory, generation(3), generation(1));
  expect(fs.readdirSync(directory).sort()).toEqual([recorded, current].sort());
  expect(report.deleted).toEqual([orphan]);
  expect(report.skipped).toEqual([]);
});

test('a symlinked generation is never followed, never deleted and its target survives', () => {
  const directory = hostDirectory();
  const outside = path.join(path.dirname(directory), 'outside-target.bin');
  fs.writeFileSync(outside, Buffer.from('outside'), { mode: 0o600 });
  const linkName = `credential-${generation(9)}.bin`;
  fs.symlinkSync(outside, path.join(directory, linkName));
  const current = writeGeneration(directory, generation(5), 0);
  const previous = writeGeneration(directory, generation(4), 60_000);
  const report = pruneSupersededCredentials(directory, generation(5));
  expect(fs.lstatSync(path.join(directory, linkName)).isSymbolicLink()).toBe(true);
  expect(fs.readFileSync(outside, 'utf8')).toBe('outside');
  expect(report.skipped).toEqual([linkName]);
  expect(report.deleted).toEqual([]);
  expect(fs.readdirSync(directory).sort()).toEqual([linkName, current, previous].sort());
});

test('a generation with foreign permissions is left alone and reported while safe ones go', () => {
  const directory = hostDirectory();
  const current = writeGeneration(directory, generation(5), 0);
  const previous = writeGeneration(directory, generation(4), 60_000);
  const loose = writeGeneration(directory, generation(3), 120_000, 0o644);
  const oldest = writeGeneration(directory, generation(2), 180_000);
  const report = pruneSupersededCredentials(directory, generation(5));
  expect(fs.existsSync(path.join(directory, loose))).toBe(true);
  expect(fs.existsSync(path.join(directory, oldest))).toBe(false);
  expect(report.skipped).toEqual([loose]);
  expect(report.deleted).toEqual([oldest]);
  expect(fs.readdirSync(directory).sort()).toEqual([current, previous, loose].sort());
});

test('non-generation files are never touched', () => {
  const directory = hostDirectory();
  const current = writeGeneration(directory, generation(5), 0);
  fs.writeFileSync(path.join(directory, 'notes.txt'), 'keep me', { mode: 0o600 });
  fs.writeFileSync(path.join(directory, `credential-${'z'.repeat(64)}.bin`), 'bad name', {
    mode: 0o600,
  });
  const report = pruneSupersededCredentials(directory, generation(5));
  expect(report.deleted).toEqual([]);
  expect(report.skipped).toEqual([]);
  expect(fs.readdirSync(directory).sort()).toEqual(
    [current, 'notes.txt', `credential-${'z'.repeat(64)}.bin`].sort()
  );
});

test('prune refuses a directory that is not privately owned', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-retention-'));
  temporary.push(root);
  const open = path.join(root, 'open');
  fs.mkdirSync(open, { mode: 0o755 });
  writeGeneration(open, generation(1), 60_000);
  expect(() => pruneSupersededCredentials(open, generation(2))).toThrow(PrivateStorageError);
  expect(fs.existsSync(path.join(open, `credential-${generation(1)}.bin`))).toBe(true);
});

test('repeated registry saves retain exactly the current and one previous credential', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-retention-'));
  temporary.push(root);
  const registry = new AntigravityProfileRegistry(path.join(root, '.ccs'));
  const directory = path.join(root, '.ccs', 'antigravity-instances', 'first', 'ubuntu');
  const credential = (revision: string): NativeCredential => ({
    format: 'fixture-native-v1',
    bytes: Buffer.from(JSON.stringify({ revision, secret: `fixture-only-${revision}` })),
  });
  const identity = (): VerifiedIdentity => ({
    email: 'first@example.com',
    subject: 'fixture-subject',
    plan: 'fixture-plan',
    verifiedAt: new Date().toISOString(),
    source: 'provider-userinfo',
  });
  for (let revision = 1; revision <= 5; revision++) {
    await registry.withLock(async () => {
      registry.saveCredential(
        'first',
        'ubuntu',
        credential(String(revision)),
        identity(),
        Date.now()
      );
    });
    const generations = fs.readdirSync(directory).filter((name) => name.startsWith('credential-'));
    expect(generations.length).toBeLessThanOrEqual(2);
  }
  const generations = fs.readdirSync(directory).filter((name) => name.startsWith('credential-'));
  expect(generations.length).toBe(2);
  const saved = registry.readCredential('first', 'ubuntu');
  expect(saved.credential.bytes.equals(credential('5').bytes)).toBe(true);
  expect(generations).toContain(`credential-${saved.credentialRevision}.bin`);
});
