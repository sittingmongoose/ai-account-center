/**
 * The helper sync sends one flat archive: the source helpers plus the
 * generated Codex runtime. Fixture folders only: nothing here reaches ssh or
 * a real ~/.ccs.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import { spawnSync } from 'child_process';
import { createHash } from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  HELPER_FILES,
  MAC_EXTRACT,
  helperArchiveArgs,
  localHelperChecksums,
} from '../../../src/web-server/services/app-update-hosts';

const RUNTIME = 'app_update_codex_runtime.cjs';
const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});

function temporary(prefix: string): string {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  directories.push(directory);
  return directory;
}

/** A package root with every helper; the runtime is left out when built is false. */
function fixtureRoot(built = true): string {
  const root = temporary('aac-helper-sync-');
  const source = path.join(root, 'scripts', 'app-updates');
  const generated = path.join(root, 'dist', 'app-updates');
  fs.mkdirSync(source, { recursive: true });
  fs.mkdirSync(generated, { recursive: true });
  for (const name of HELPER_FILES) {
    if (name === RUNTIME && !built) continue;
    fs.writeFileSync(path.join(name === RUNTIME ? generated : source, name), `fixture ${name}\n`);
  }
  return root;
}

describe('remote helper sync', () => {
  it('ships the source helpers and the Codex runtime', () => {
    expect(HELPER_FILES).toHaveLength(13);
    expect(HELPER_FILES).toContain(RUNTIME);
    const source = path.resolve(__dirname, '../../../scripts/app-updates');
    for (const name of HELPER_FILES.filter((value) => value !== RUNTIME))
      expect(fs.existsSync(path.join(source, name))).toBe(true);
    // Generated code never lands in the source folder.
    expect(fs.existsSync(path.join(source, RUNTIME))).toBe(false);
  });

  it('archives every helper flat from both folders and extracts them flat', () => {
    const root = fixtureRoot();
    const args = helperArchiveArgs(root);
    expect(args.filter((value) => value === '-C')).toHaveLength(2);
    const archive = spawnSync('tar', args);
    expect(archive.status).toBe(0);
    const listing = spawnSync('tar', ['-t', '-f', '-'], {
      input: archive.stdout,
      encoding: 'utf8',
    });
    expect(listing.status).toBe(0);
    const names = listing.stdout.split('\n').filter(Boolean);
    expect([...names].sort()).toEqual([...HELPER_FILES].sort());

    const home = temporary('aac-helper-home-');
    const extracted = spawnSync('/bin/sh', ['-c', MAC_EXTRACT], {
      input: archive.stdout,
      env: { ...process.env, HOME: home },
    });
    expect(extracted.status).toBe(0);
    const target = path.join(home, '.ccs', 'app-updates');
    expect(fs.readdirSync(target).sort()).toEqual([...HELPER_FILES].sort());
    for (const name of HELPER_FILES)
      expect(fs.readFileSync(path.join(target, name), 'utf8')).toBe(`fixture ${name}\n`);
  });

  it('hashes the runtime from the build output beside the source helpers', () => {
    const root = fixtureRoot();
    const values = localHelperChecksums(root);
    expect(Object.keys(values).sort()).toEqual([...HELPER_FILES].sort());
    for (const name of HELPER_FILES)
      expect(values[name]).toBe(createHash('sha256').update(`fixture ${name}\n`).digest('hex'));
  });

  it('fails loudly when the runtime is not built instead of sending a partial set', () => {
    const root = fixtureRoot(false);
    const message = 'The Codex update runtime is not built; run bun run build:server.';
    expect(() => localHelperChecksums(root)).toThrow(message);
    expect(() => helperArchiveArgs(root)).toThrow(message);
  });

  it('fails when a source helper is missing', () => {
    const root = fixtureRoot();
    fs.rmSync(path.join(root, 'scripts', 'app-updates', 'app_update_codex.cjs'));
    expect(() => localHelperChecksums(root)).toThrow('app_update_codex.cjs is missing');
  });
});
