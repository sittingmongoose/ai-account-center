#!/usr/bin/env node
'use strict';
/**
 * The history store's native module (docs/store.md 9.2).
 *
 *   node scripts/build-native.js            (bun run store:build)
 *     Compiles native/aac-store with cargo for this computer, keeps the result in
 *     native/aac-store/prebuilt/aac-store.<platform>-<arch>.node (with a .json note of the
 *     sources it was built from), checks that Node loads it, and copies it into dist/native/.
 *
 *   node scripts/build-native.js --copy-only   (every `build` runs this)
 *     Copies an existing prebuilt module into dist/native/ without compiling. A machine with
 *     no prebuilt module builds a dashboard that runs without stored history.
 *
 * Needs for a compile: cargo with the toolchain in native/aac-store/rust-toolchain.toml
 * (rustup installs it), a C compiler (zstd is C), and on Linux glibc as on the target machine:
 * build on the machine that runs the dashboard, or one with the same or an older glibc.
 */
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { spawnSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const CRATE = path.join(ROOT, 'native', 'aac-store');
const PREBUILT = path.join(CRATE, 'prebuilt');
const DIST = path.join(ROOT, 'dist', 'native');
const BINDING_VERSION = 1;

function moduleName(platform = process.platform, arch = process.arch) {
  return `aac-store.${platform}-${arch}.node`;
}

function cdylibName() {
  if (process.platform === 'win32') return 'aac_store.dll';
  if (process.platform === 'darwin') return 'libaac_store.dylib';
  return 'libaac_store.so';
}

/** SHA-256 over the crate's build inputs, in a fixed order. */
function sourceHash() {
  const files = [
    'Cargo.toml',
    'Cargo.lock',
    'build.rs',
    'rust-toolchain.toml',
    '../../src/web-server/status/report-secret-policy.json',
  ];
  const walk = (dir) => {
    for (const entry of fs.readdirSync(path.join(CRATE, dir), { withFileTypes: true })) {
      const rel = `${dir}/${entry.name}`;
      if (entry.isDirectory()) walk(rel);
      else files.push(rel);
    }
  };
  walk('src');
  const hash = crypto.createHash('sha256');
  for (const rel of files.sort()) {
    hash.update(rel);
    hash.update('\0');
    // Line endings do not change the build.
    hash.update(fs.readFileSync(path.join(CRATE, rel), 'utf8').replace(/\r\n/g, '\n'));
    hash.update('\0');
  }
  return hash.digest('hex');
}

function fileSha(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function checkLoads(file) {
  const result = spawnSync(
    process.execPath,
    [
      '-e',
      `const m = require(${JSON.stringify(file)}); if (m.bindingVersion() !== ${BINDING_VERSION}) process.exit(3);`,
    ],
    { encoding: 'utf8' }
  );
  if (result.status !== 0) {
    throw new Error(`Node could not load ${path.basename(file)}: ${(result.stderr || '').trim()}`);
  }
}

function copyToDist() {
  const name = moduleName();
  const source = path.join(PREBUILT, name);
  if (!fs.existsSync(source)) {
    console.log(
      `[i] No history store module for ${process.platform}-${process.arch}: ` +
        'the dashboard runs without stored history. Build it with: bun run store:build'
    );
    return false;
  }
  const notePath = `${source}.json`;
  const note = fs.existsSync(notePath) ? JSON.parse(fs.readFileSync(notePath, 'utf8')) : null;
  const current = sourceHash();
  if (!note || note.sourceSha256 !== current) {
    console.warn(
      `[!] ${name} was built from other store sources than this checkout. ` +
        'Rebuild it with: bun run store:build'
    );
  }
  fs.mkdirSync(DIST, { recursive: true });
  fs.copyFileSync(source, path.join(DIST, name));
  if (note) fs.writeFileSync(path.join(DIST, `${name}.json`), `${JSON.stringify(note, null, 2)}\n`);
  console.log(`[OK] History store module copied into dist/native/${name}`);
  return true;
}

function build() {
  const cargo = spawnSync('cargo', ['--version'], { encoding: 'utf8', cwd: CRATE });
  if (cargo.status !== 0) {
    console.error('[X] cargo is not installed. Install Rust with rustup, then run this again.');
    process.exit(1);
  }
  const target = process.env.CARGO_TARGET_DIR
    ? path.resolve(process.env.CARGO_TARGET_DIR)
    : path.join(CRATE, 'target');
  console.log('[i] Building the history store module (cargo build --release --locked)...');
  const result = spawnSync('cargo', ['build', '--release', '--locked'], {
    cwd: CRATE,
    stdio: 'inherit',
    env: { ...process.env, CARGO_TARGET_DIR: target },
  });
  if (result.status !== 0) process.exit(result.status || 1);
  const artifact = path.join(target, 'release', cdylibName());
  fs.mkdirSync(PREBUILT, { recursive: true });
  const name = moduleName();
  const destination = path.join(PREBUILT, name);
  // Node chooses its loader by extension, so the temporary copy keeps `.node`.
  const temporary = destination.replace(/\.node$/, `.tmp-${process.pid}.node`);
  fs.copyFileSync(artifact, temporary);
  checkLoads(temporary);
  fs.renameSync(temporary, destination);
  const rustc = spawnSync('rustc', ['--version'], { encoding: 'utf8', cwd: CRATE });
  const note = {
    module: name,
    sha256: fileSha(destination),
    sourceSha256: sourceHash(),
    bindingVersion: BINDING_VERSION,
    rustc: (rustc.stdout || '').trim() || null,
    builtAt: new Date().toISOString(),
  };
  fs.writeFileSync(`${destination}.json`, `${JSON.stringify(note, null, 2)}\n`);
  console.log(`[OK] Built native/aac-store/prebuilt/${name}`);
  copyToDist();
}

if (require.main === module) {
  if (process.argv.includes('--copy-only')) copyToDist();
  else build();
}

module.exports = { moduleName, sourceHash, copyToDist };
