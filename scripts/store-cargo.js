#!/usr/bin/env node
'use strict';
/**
 * Cargo checks for the history store crate (docs/store.md 9.3, 9.4), run inside native/aac-store
 * so rustup uses the crate's pinned toolchain (rust-toolchain.toml is read from the working
 * folder, not from --manifest-path).
 *
 *   node scripts/store-cargo.js test    (bun run store:test)   the Rust suites
 *   node scripts/store-cargo.js check   (bun run store:check)  fmt, clippy, then the suites
 */
const path = require('path');
const { spawnSync } = require('child_process');

const CRATE = path.resolve(__dirname, '..', 'native', 'aac-store');

function cargo(args) {
  const result = spawnSync('cargo', args, { cwd: CRATE, stdio: 'inherit' });
  if (result.error) {
    console.error(`[X] cargo could not start: ${result.error.message}`);
    process.exit(1);
  }
  if (result.status !== 0) process.exit(result.status || 1);
}

const TEST = ['test', '--release', '--locked', '--no-default-features'];

const mode = process.argv[2];
if (mode === 'test') {
  cargo(TEST);
} else if (mode === 'check') {
  cargo(['fmt', '--check']);
  cargo(['clippy', '--release', '--all-targets', '--locked', '--', '-D', 'warnings']);
  cargo(TEST);
} else {
  console.error('Usage: node scripts/store-cargo.js test|check');
  process.exit(2);
}
