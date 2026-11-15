import { readFileSync } from 'fs';
import { describe, expect, it } from 'bun:test';

/** A packed tree must still resolve native status's path crates (L3 review L4). */
describe('npm files for shipped Rust sources', () => {
  it('ships the workspace manifest and the crates native status compiles against', () => {
    const files = JSON.parse(readFileSync('package.json', 'utf8')).files as string[];
    for (const needed of ['Cargo.toml', 'crates/aac-core/', 'crates/aac-files/', 'crates/aac-num/'])
      expect(files).toContain(needed);
  });
});
