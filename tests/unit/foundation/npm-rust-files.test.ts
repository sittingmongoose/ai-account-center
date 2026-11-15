import { readFileSync, readdirSync, statSync } from 'fs';
import path from 'path';
import { describe, expect, it } from 'bun:test';

const SHIPPED_CRATES = ['crates/aac-core/', 'crates/aac-files/', 'crates/aac-num/'];

function rustFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const full = path.join(dir, name);
    return statSync(full).isDirectory() ? rustFiles(full) : name.endsWith('.rs') ? [full] : [];
  });
}

/** A packed tree must still resolve native status's path crates (L3 review L4). */
describe('npm files for shipped Rust sources', () => {
  const files = JSON.parse(readFileSync('package.json', 'utf8')).files as string[];
  const shipped = (file: string) =>
    files.some((entry) => (entry.endsWith('/') ? file.startsWith(entry) : file === entry));

  it('ships the workspace manifest and the crates native status compiles against', () => {
    for (const needed of ['Cargo.toml', ...SHIPPED_CRATES]) expect(files).toContain(needed);
  });

  it('ships every file a shipped crate compiles in outside its tests (Lows batch 3 review L2)', () => {
    const missing: string[] = [];
    for (const crate of SHIPPED_CRATES)
      for (const file of rustFiles(path.join(crate, 'src'))) {
        const source = readFileSync(file, 'utf8');
        // Test modules sit at the end of a file by convention; what follows #[cfg(test)] is test-only.
        const production = source.split('#[cfg(test)]')[0] ?? '';
        for (const match of production.matchAll(/include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)/g)) {
          const target = path.relative('.', path.resolve(path.dirname(file), match[1] as string));
          if (!shipped(target)) missing.push(`${file} -> ${target}`);
        }
      }
    expect(missing).toEqual([]);
  });
});
