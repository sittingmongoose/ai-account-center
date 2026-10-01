#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { parse } = require('smol-toml');
const {
  assertSlintPin,
  assertLockedSlint,
  sourceFingerprint,
  commandRunner,
  toolPath,
} = require('./build-ui');
const { verifyBundle } = require('./verify-bundle');

/** Validate the production UI without rebuilding already verified artifacts. */
function validateUi(options = {}) {
  const repoRoot = options.repoRoot ?? path.resolve(__dirname, '..');
  const run = options.run ?? commandRunner;
  const crate = path.join(repoRoot, 'web-dashboard');
  const cargoBin = path.join(os.homedir(), '.cargo', 'bin');
  const env = { ...process.env, PATH: `${cargoBin}${path.delimiter}${process.env.PATH ?? ''}` };
  assertSlintPin(parse(fs.readFileSync(path.join(crate, 'Cargo.toml'), 'utf8')));
  assertLockedSlint(parse(fs.readFileSync(path.join(crate, 'Cargo.lock'), 'utf8')));
  run(
    toolPath('cargo', cargoBin),
    ['fmt', '--manifest-path', path.join(crate, 'Cargo.toml'), '--check'],
    {
      cwd: repoRoot,
      env,
    }
  );
  const testFiles = fs
    .readdirSync(path.join(crate, 'tests'))
    .filter((name) => name.endsWith('.test.mjs'))
    .sort()
    .map((name) => path.join(crate, 'tests', name));
  run(process.execPath, ['--test', ...testFiles], {
    cwd: repoRoot,
    env,
  });
  run(process.execPath, ['--check', path.join(crate, 'public', 'bridge.js')], {
    cwd: repoRoot,
    env,
  });
  const source = sourceFingerprint(crate);
  const { manifest } = verifyBundle(path.join(repoRoot, 'dist', 'ui'));
  if (manifest.source?.sha256 !== source.sha256 || manifest.source?.files !== source.files) {
    throw new Error('Dashboard source changed after the build. Run bun run ui:build.');
  }
  return source;
}

if (require.main === module) {
  try {
    const source = validateUi();
    console.log(`[OK] Slint 1.18.1 UI validated against ${source.files} current build inputs.`);
  } catch (error) {
    console.error(`[X] ${error instanceof Error ? error.message : 'Dashboard validation failed.'}`);
    process.exitCode = 1;
  }
}

module.exports = { validateUi };
