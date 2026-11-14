/** Offline server parity (slow bucket: starts HTTP and uses the same runtime as dist/). */
import { test } from 'bun:test';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

test('M0 routes, role contracts, parser tables and shared identities stay byte-equal', () => {
  execFileSync('node', ['tests/spec/run.mjs'], {
    cwd: fileURLToPath(new URL('../../..', import.meta.url)),
    stdio: 'pipe',
    timeout: 120_000,
    env: { ...process.env, AAC_NO_LOCAL_RUST_BUILD: '1' },
  });
}, 125_000);
