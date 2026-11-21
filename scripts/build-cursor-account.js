#!/usr/bin/env node
'use strict';
/**
 * The Cursor account reader (native/aac-cursor-account, DESIGN 17 in the share folder
 * 2026-11-13-analytics-discovery): one self-contained `aac-cursor-account` per target, built like
 * the usage-log scanner (build-usage-scan.js). It runs on the dashboard's own computer only and is
 * never copied to another computer (it is the one AAC piece that calls Cursor with AAC's key).
 *
 *   node scripts/build-cursor-account.js            (bun run cursor-account:build)
 *     Compiles the reader for this computer (cargo build --release --locked) and keeps it in
 *     native/aac-cursor-account/prebuilt/<target>/aac-cursor-account[.exe] with a note of the
 *     sources it was built from. Builds happen on Nas1 (Linux), the Mac and Windows, never on the
 *     dashboard VM; the landing collects each machine's prebuilt/<target> folder.
 *
 *   node scripts/build-cursor-account.js --copy-only   (every `build` runs this)
 *     Copies every prebuilt target built from this checkout's sources into
 *     dist/native/aac-cursor-account/ and writes manifest.json there: {name, version, protocol,
 *     sourceSha256, targets: {<target>: {file, sha256, size}}}. Without a reader for the dashboard's
 *     target, the Cursor account is not read and Analytics says so.
 */
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { spawnSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const CRATE = path.join(ROOT, 'native', 'aac-cursor-account');
const PREBUILT = path.join(CRATE, 'prebuilt');
const DIST = path.join(ROOT, 'dist', 'native', 'aac-cursor-account');
const TARGETS = {
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'win32-x64': 'x86_64-pc-windows-msvc',
};
const SUPPORTED = Object.values(TARGETS);

function binaryName(target) {
  return target.endsWith('windows-msvc') ? 'aac-cursor-account.exe' : 'aac-cursor-account';
}

function fileSha(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

/** SHA-256 over the crate's build inputs in a fixed order, line endings ignored. */
function sourceHash() {
  const files = ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml'];
  const walk = (dir) => {
    for (const entry of fs.readdirSync(path.join(CRATE, dir), { withFileTypes: true })) {
      const rel = `${dir}/${entry.name}`;
      if (entry.isDirectory()) walk(rel);
      else files.push(rel);
    }
  };
  walk('src');
  // The shared file reader and its own path dependencies are build inputs too.
  for (const dep of ['aac-files', 'aac-core', 'aac-num']) {
    files.push(`../../crates/${dep}/Cargo.toml`);
    walk(`../../crates/${dep}/src`);
  }
  const hash = crypto.createHash('sha256');
  for (const rel of files.sort()) {
    hash.update(rel);
    hash.update('\0');
    hash.update(fs.readFileSync(path.join(CRATE, rel), 'utf8').replace(/\r\n/g, '\n'));
    hash.update('\0');
  }
  return hash.digest('hex');
}

function crateVersion() {
  const match = fs
    .readFileSync(path.join(CRATE, 'Cargo.toml'), 'utf8')
    .match(/^version = "([^"]+)"/m);
  return match ? match[1] : null;
}

/** The Cursor account reader's own answer to --version, or null when it does not run here. */
function versionOf(file) {
  const result = spawnSync(file, ['--version'], { encoding: 'utf8', timeout: 10_000 });
  if (result.status !== 0) return null;
  try {
    return JSON.parse(result.stdout);
  } catch {
    return null;
  }
}

function build() {
  const target = TARGETS[`${process.platform}-${process.arch}`];
  if (!target) {
    console.error(`[X] No Cursor account reader target for ${process.platform}-${process.arch}.`);
    process.exit(1);
  }
  const cargoTarget = process.env.CARGO_TARGET_DIR
    ? path.resolve(process.env.CARGO_TARGET_DIR)
    : path.join(CRATE, 'target');
  console.log('[i] Building the Cursor account reader (cargo build --release --locked)...');
  const result = spawnSync('cargo', ['build', '--release', '--locked'], {
    cwd: CRATE,
    stdio: 'inherit',
    env: { ...process.env, CARGO_TARGET_DIR: cargoTarget },
  });
  if (result.status !== 0) process.exit(result.status || 1);
  const artifact = path.join(cargoTarget, 'release', binaryName(target));
  const folder = path.join(PREBUILT, target);
  fs.mkdirSync(folder, { recursive: true });
  const destination = path.join(folder, binaryName(target));
  fs.copyFileSync(artifact, destination);
  fs.chmodSync(destination, 0o755);
  const version = versionOf(destination);
  if (!version || version.target !== target) {
    console.error('[X] The built Cursor account reader did not answer --version for this target.');
    process.exit(1);
  }
  const rustc = spawnSync('rustc', ['--version'], { encoding: 'utf8', cwd: CRATE });
  const note = {
    target,
    sha256: fileSha(destination),
    size: fs.statSync(destination).size,
    sourceSha256: sourceHash(),
    version: version.version,
    protocol: version.protocol,
    rustc: (rustc.stdout || '').trim() || null,
    builtAt: new Date().toISOString(),
  };
  fs.writeFileSync(`${destination}.json`, `${JSON.stringify(note, null, 2)}\n`);
  console.log(
    `[OK] ${target} Cursor account reader in native/aac-cursor-account/prebuilt/${target}/ (${note.sha256.slice(0, 12)})`
  );
}

function copyOnly() {
  const current = sourceHash();
  const targets = {};
  let protocol = null;
  for (const target of SUPPORTED) {
    const file = path.join(PREBUILT, target, binaryName(target));
    if (!fs.existsSync(file)) continue;
    let note = null;
    try {
      note = JSON.parse(fs.readFileSync(`${file}.json`, 'utf8'));
    } catch {
      /* A binary without its note is not shipped. */
    }
    if (!note || note.sourceSha256 !== current || note.sha256 !== fileSha(file)) {
      console.warn(`[!] ${target} Cursor account reader was built from other sources; it is not shipped.`);
      continue;
    }
    if (protocol !== null && note.protocol !== protocol) {
      console.warn(`[!] ${target} Cursor account reader speaks protocol ${note.protocol}; it is not shipped.`);
      continue;
    }
    protocol = note.protocol;
    const out = path.join(DIST, target);
    fs.mkdirSync(out, { recursive: true });
    fs.copyFileSync(file, path.join(out, binaryName(target)));
    fs.chmodSync(path.join(out, binaryName(target)), 0o755);
    targets[target] = {
      file: `${target}/${binaryName(target)}`,
      sha256: note.sha256,
      size: note.size,
    };
  }
  fs.mkdirSync(DIST, { recursive: true });
  const manifest = {
    name: 'aac-cursor-account',
    version: crateVersion(),
    protocol,
    sourceSha256: current,
    targets,
  };
  fs.writeFileSync(path.join(DIST, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  const count = Object.keys(targets).length;
  console.log(
    count
      ? `[OK] Cursor account reader for ${Object.keys(targets).join(', ')} copied into dist/native/aac-cursor-account/`
      : '[i] No Cursor account reader built from these sources: the Cursor account is not read (Analytics says so).'
  );
}

/** Every shipped Cursor account reader matches the manifest beside it (verify-bundle). */
function verifyDist(dir = DIST) {
  const manifestPath = path.join(dir, 'manifest.json');
  if (!fs.existsSync(manifestPath)) return { present: false, targets: [] };
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  for (const [target, entry] of Object.entries(manifest.targets || {})) {
    if (!SUPPORTED.includes(target)) throw new Error(`Unknown Cursor account reader target ${target}.`);
    const file = path.join(dir, entry.file);
    if (
      !fs.existsSync(file) ||
      fileSha(file) !== entry.sha256 ||
      fs.statSync(file).size !== entry.size
    )
      throw new Error(`The ${target} Cursor account reader does not match its manifest.`);
  }
  return { present: true, targets: Object.keys(manifest.targets || {}) };
}

if (require.main === module) {
  if (process.argv.includes('--copy-only')) copyOnly();
  else build();
}

module.exports = { sourceHash, verifyDist, binaryName, SUPPORTED, TARGETS };
