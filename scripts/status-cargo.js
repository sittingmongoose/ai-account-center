#!/usr/bin/env node
'use strict';
/**
 * Cargo checks for the status readers crate, run inside crates/aac-status so rustup uses the
 * crate's pinned toolchain (rust-toolchain.toml is read from the working folder, not from
 * --manifest-path). Named after scripts/store-cargo.js on the aac-store branch.
 *
 *   node scripts/status-cargo.js test    (bun run status:test)   the Rust suites
 *   node scripts/status-cargo.js check   (bun run status:check)  fmt, clippy, then the suites
 */
const path = require('path');
const { spawnSync } = require('child_process');

const CRATE = path.resolve(__dirname, '..', 'crates', 'aac-status');

// One fresh CARGO_TARGET_DIR per checkout: refuse a folder another checkout created.
require('./foundation/cargo-target-guard.js').guardCargoTarget(path.resolve(__dirname, '..'));

function cargo(args) {
  const result = spawnSync('cargo', args, { cwd: CRATE, stdio: 'inherit' });
  if (result.error) {
    console.error(`[X] cargo could not start: ${result.error.message}`);
    process.exit(1);
  }
  if (result.status !== 0) process.exit(result.status || 1);
}

const TEST = ['test', '-p', 'aac-status', '--release', '--locked', '--no-default-features'];

const mode = process.argv[2];
if (mode === 'test') {
  cargo(TEST);
} else if (mode === 'check') {
  cargo(['fmt', '-p', 'aac-status', '--check']);
  cargo(['clippy', '-p', 'aac-status', '--release', '--all-targets', '--locked', '--', '-D', 'warnings']);
  cargo(TEST);
} else {
  console.error('Usage: node scripts/status-cargo.js test|check');
  process.exit(2);
}
