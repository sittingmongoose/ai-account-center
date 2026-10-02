/**
 * The runtime bundle (scripts/antigravity/runtime) is installed as its own
 * hashed, versioned copy, so it cannot import the package's dashboard helpers
 * in scripts/antigravity. Three modules therefore exist twice on purpose; they
 * must stay byte-identical, and the bundle manifest must hash every file as
 * shipped (install_runtime.py refuses a mismatch at install time).
 */
import { describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';

const ROOT = path.resolve(import.meta.dir, '../../../scripts/antigravity');
const RUNTIME = path.join(ROOT, 'runtime');
const sha = (file: string) => createHash('sha256').update(fs.readFileSync(file)).digest('hex');

describe('Antigravity runtime bundle', () => {
  it.each(['auth_platforms.py', 'native_credential_worker.py', 'native_status_userinfo.py'])(
    'keeps the dashboard and bundle copies of %s identical',
    (name) => {
      expect(sha(path.join(RUNTIME, name))).toBe(sha(path.join(ROOT, name)));
    }
  );

  it('hashes every shipped bundle file in its manifest', () => {
    const manifest = JSON.parse(
      fs.readFileSync(path.join(RUNTIME, 'runtime-manifest.json'), 'utf8')
    );
    const rows = manifest.files as Array<{ path: string; sha256: string }>;
    expect(rows.length).toBeGreaterThan(0);
    for (const row of rows)
      expect([row.path, sha(path.join(RUNTIME, row.path))]).toEqual([row.path, row.sha256]);
    const shipped = fs
      .readdirSync(RUNTIME)
      .filter(
        (name) => fs.statSync(path.join(RUNTIME, name)).isFile() && name !== 'runtime-manifest.json'
      )
      .sort();
    expect(rows.map((row) => row.path).sort()).toEqual(shipped);
  });
});
