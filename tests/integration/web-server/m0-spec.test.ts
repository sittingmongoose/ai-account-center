/** Offline server parity (slow bucket: starts HTTP and uses the same runtime as dist/). */
import { test } from 'bun:test';
import { fileURLToPath } from 'node:url';
import { runCaptured } from '../../spec/run-captured.mjs';

test('M0 routes, role contracts, parser tables and shared identities stay byte-equal', () => {
  // Output streams to a file: a large corpus diff must not surface ENOBUFS or truncate away the failing
  // step (TOOL-ISSUES catalog-astra-T1); on failure the step is named first, then a bounded tail.
  runCaptured('node', ['tests/spec/run.mjs'], {
    cwd: fileURLToPath(new URL('../../..', import.meta.url)),
    timeout: 120_000,
    env: { ...process.env, AAC_NO_LOCAL_RUST_BUILD: '1' },
  });
}, 125_000);
