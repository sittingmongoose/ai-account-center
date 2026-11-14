#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { parse } = require('smol-toml');
const {
  assertLockedLeptos,
  assertLockedWasmBindgen,
  leptosFingerprint,
  shellFingerprint,
  stagedAppFiles,
  commandRunner,
  toolPath,
  LEPTOS_VERSION,
} = require('./build-ui');
const { verifyBundle } = require('./verify-bundle');

/**
 * Validate the production UI without rebuilding already verified artifacts: the pins, the Leptos crate's formatting,
 * the dashboard's node tests, the packaged bundle, and that the build in dist/ui is the build of the current sources.
 */
function validateUi(options = {}) {
  const repoRoot = options.repoRoot ?? path.resolve(__dirname, '..');
  const run = options.run ?? commandRunner;
  const crate = path.join(repoRoot, 'web-dashboard');
  const leptosCrate = path.join(crate, 'leptos');
  const cargoBin = path.join(os.homedir(), '.cargo', 'bin');
  const env = { ...process.env, PATH: `${cargoBin}${path.delimiter}${process.env.PATH ?? ''}` };
  const lock = parse(fs.readFileSync(path.join(leptosCrate, 'Cargo.lock'), 'utf8'));
  assertLockedLeptos(lock);
  assertLockedWasmBindgen(lock, 'Leptos');
  run(
    toolPath('cargo', cargoBin),
    ['fmt', '--manifest-path', path.join(leptosCrate, 'Cargo.toml'), '--check'],
    { cwd: leptosCrate, env }
  );
  const testFiles = fs
    .readdirSync(path.join(crate, 'tests'))
    .filter((name) => name.endsWith('.test.mjs'))
    .sort()
    .map((name) => path.join(crate, 'tests', name));
  run(process.execPath, ['--test', ...testFiles], { cwd: repoRoot, env });
  const source = leptosFingerprint(leptosCrate);
  const { manifest } = verifyBundle(path.join(repoRoot, 'dist', 'ui'));
  if (manifest.source?.sha256 !== source.sha256 || manifest.source?.files !== source.files) {
    throw new Error('Dashboard source changed after the build. Run bun run ui:build.');
  }
  // The recorded shell stamp must still be the stamp of the current shell inputs.
  const shell = shellFingerprint(
    path.join(crate, 'public'),
    stagedAppFiles(leptosCrate, manifest.buildId)
  );
  if (
    manifest.shell?.sha256 !== shell.sha256 ||
    manifest.shell?.files !== shell.files ||
    manifest.shellBuildId !== manifest.shell.sha256.slice(0, 12)
  ) {
    throw new Error('Dashboard shell changed after the build. Run bun run ui:build.');
  }
  return source;
}

if (require.main === module) {
  try {
    const source = validateUi();
    console.log(
      `[OK] Leptos ${LEPTOS_VERSION} UI validated against ${source.files} current build inputs.`
    );
  } catch (error) {
    console.error(`[X] ${error instanceof Error ? error.message : 'Dashboard validation failed.'}`);
    process.exitCode = 1;
  }
}

module.exports = { validateUi };
