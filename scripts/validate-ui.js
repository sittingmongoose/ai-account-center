#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { parse } = require('smol-toml');
const {
  assertSlintPin,
  assertLockedSlint,
  assertLockedWasmBindgen,
  sourceFingerprint,
  leptosFingerprint,
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
  const dashboardLock = parse(fs.readFileSync(path.join(crate, 'Cargo.lock'), 'utf8'));
  assertLockedSlint(dashboardLock);
  assertLockedWasmBindgen(dashboardLock, 'Dashboard');
  run(
    toolPath('cargo', cargoBin),
    ['fmt', '--manifest-path', path.join(crate, 'Cargo.toml'), '--check'],
    {
      cwd: crate,
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
  // The Leptos pages (Status, Scoreboard) are checked the same way when the crate is present.
  const leptosCrate = path.join(crate, 'leptos');
  // lstat, not existsSync: a dangling link where the crate should be is checked (and refused), never taken for "no
  // pages here"; inside a real crate folder the fingerprint walk refuses a linked or dangling manifest
  const present = (file) => {
    try {
      fs.lstatSync(file);
      return true;
    } catch {
      return false;
    }
  };
  if (present(leptosCrate)) {
    assertLockedWasmBindgen(
      parse(fs.readFileSync(path.join(leptosCrate, 'Cargo.lock'), 'utf8')),
      'Leptos'
    );
    run(
      toolPath('cargo', cargoBin),
      ['fmt', '--manifest-path', path.join(leptosCrate, 'Cargo.toml'), '--check'],
      {
        cwd: leptosCrate,
        env,
      }
    );
    const leptos = leptosFingerprint(leptosCrate);
    if (
      manifest.leptos?.source?.sha256 !== leptos.sha256 ||
      manifest.leptos?.source?.files !== leptos.files
    ) {
      throw new Error('Leptos page source changed after the build. Run bun run ui:build.');
    }
    // Every file the pages load is there, and the wasm is the one the build recorded.
    const ui = path.join(repoRoot, 'dist', 'ui');
    const pkg = `pkg/${manifest.leptos.buildId}`;
    for (const file of [
      manifest.leptos.entry,
      manifest.leptos.wasm?.path,
      `${pkg}/aac_web.js`,
      'app/app.css',
      'app/MartianMono.ttf',
    ]) {
      if (typeof file !== 'string' || !fs.existsSync(path.join(ui, file))) {
        throw new Error('The packaged Leptos pages are incomplete. Run bun run ui:build.');
      }
    }
    const wasm = fs.readFileSync(path.join(ui, manifest.leptos.wasm.path));
    const wasmSha = require('crypto').createHash('sha256').update(wasm).digest('hex');
    if (wasmSha !== manifest.leptos.wasm.sha256 || wasm.length !== manifest.leptos.wasm.bytes) {
      throw new Error('The packaged Leptos wasm is not the one the build recorded. Run bun run ui:build.');
    }
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
