import path from 'path';
import { describe, expect, it } from 'bun:test';
import { toPosix } from '../../../scripts/foundation/to-posix.mjs';

/** Wire tables and ship lists record POSIX paths, so Windows runs must not rewrite them (tool-issues 58/59). */
describe('toPosix path normalization', () => {
  it('maps the native-separator form path.relative and pathlib report to POSIX form', () => {
    expect(toPosix(['src', 'web', 'backend.ts'].join(path.sep))).toBe('src/web/backend.ts');
    expect(toPosix(['crates', 'aac-files', 'src', 'table.rs'].join(path.sep))).toBe('crates/aac-files/src/table.rs');
  });

  it('round-trips a real repo file into the POSIX spelling its spec tables use', () => {
    const rel = toPosix(path.relative('.', path.resolve('scripts/foundation/generate-wire-words.mjs')));
    expect(rel).toBe('scripts/foundation/generate-wire-words.mjs');
  });
});
