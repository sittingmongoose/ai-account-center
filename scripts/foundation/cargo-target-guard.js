'use strict';
/**
 * One fresh CARGO_TARGET_DIR per checkout (lows-batch-T1, cargo-target-per-checkout-T1):
 * a target folder created by another checkout silently reuses that checkout's builds and
 * gives stale passes. The wrapper records the checkout that created the folder and refuses
 * a different one. With CARGO_TARGET_DIR unset, cargo uses the workspace's own target/,
 * which is per-checkout by construction, so there is nothing to guard.
 *
 *   const { guardCargoTarget } = require('./foundation/cargo-target-guard.js');
 *   guardCargoTarget(path.resolve(__dirname, '..'));
 */
const fs = require('fs');
const path = require('path');

const MARKER = '.aac-cargo-checkout';

function guardCargoTarget(repoRoot) {
  const target = process.env.CARGO_TARGET_DIR;
  if (!target) return;
  const dir = path.resolve(target);
  const marker = path.join(dir, MARKER);
  if (fs.existsSync(marker)) {
    const owner = fs.readFileSync(marker, 'utf8').trim();
    if (owner !== repoRoot) {
      console.error(`[X] CARGO_TARGET_DIR ${dir} was created by ${owner}; this checkout is ${repoRoot}.`);
      console.error(
        '    Use one fresh target folder per checkout: a shared folder silently reuses the other checkout\'s build and gives stale passes.'
      );
      process.exit(1);
    }
    return;
  }
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(marker, `${repoRoot}\n`);
}

module.exports = { guardCargoTarget, MARKER };