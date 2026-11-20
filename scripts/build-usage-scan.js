#!/usr/bin/env node
'use strict';
/**
 * The usage-log scanner binary (native/aac-usage, DESIGN 12 in the share folder
 * 2026-11-13-analytics-discovery): one self-contained `aac-usage-scan` per target, run on every
 * computer Analytics reads. AAC copies it, hash-checked, into its own cache folder on each remote
 * computer (src/web-server/services/usage-scan-binary.ts).
 *
 *   node scripts/build-usage-scan.js            (bun run usage-scan:build)
 *     Compiles the scanner for this computer (cargo build --release --locked) and keeps it in
 *     native/aac-usage/prebuilt/<target>/aac-usage-scan[.exe] with a note of the sources it was built
 *     from. Builds happen on Nas1 (Linux), the Mac and Windows, never on the dashboard VM; the landing
 *     collects each machine's prebuilt/<target> folder.
 *
 *   node scripts/build-usage-scan.js --stage   (every `build` runs this)
 *     Builds the scanner for this computer, then copies all matching prebuilt targets.
 *
 *   node scripts/build-usage-scan.js --copy-only
 *     Copies every prebuilt target built from this checkout's sources into dist/native/aac-usage/
 *     and writes manifest.json there: {name, version, protocol, sourceSha256, targets: {<target>:
 *     {file, sha256, size}}}. A target built from other sources is left out (its protocol could
 *     differ). The dashboard requires its local scanner in every package.
 */
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { spawnSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const CRATE = path.join(ROOT, 'native', 'aac-usage');
const PREBUILT = path.join(CRATE, 'prebuilt');
const DIST = path.join(ROOT, 'dist', 'native', 'aac-usage');
const TARGETS = {
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'win32-x64': 'x86_64-pc-windows-msvc',
};
const SUPPORTED = Object.values(TARGETS);
const LOCAL_TARGET = TARGETS[`${process.platform}-${process.arch}`];
const PROTOCOL = Number(
  fs
    .readFileSync(path.join(CRATE, 'src', 'protocol.rs'), 'utf8')
    .match(/^pub const PROTOCOL: u64 = (\d+);$/m)?.[1]
);

function binaryName(target) {
  return target.endsWith('windows-msvc') ? 'aac-usage-scan.exe' : 'aac-usage-scan';
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

/** The scanner's own answer to --version, or null when it does not run here. */
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
  const target = LOCAL_TARGET;
  if (!target) {
    console.error(`[X] No scanner target for ${process.platform}-${process.arch}.`);
    process.exit(1);
  }
  const cargoTarget = process.env.CARGO_TARGET_DIR
    ? path.resolve(process.env.CARGO_TARGET_DIR)
    : path.join(CRATE, 'target');
  console.log('[i] Building the usage-log scanner (cargo build --release --locked)...');
  const result = spawnSync(
    'cargo',
    ['build', '--release', '--locked', '-j', process.env.CARGO_BUILD_JOBS || '2'],
    {
      cwd: CRATE,
      stdio: 'inherit',
      env: { ...process.env, CARGO_TARGET_DIR: cargoTarget },
    }
  );
  if (result.status !== 0) process.exit(result.status || 1);
  const artifact = path.join(cargoTarget, 'release', binaryName(target));
  const folder = path.join(PREBUILT, target);
  fs.mkdirSync(folder, { recursive: true });
  const destination = path.join(folder, binaryName(target));
  fs.copyFileSync(artifact, destination);
  fs.chmodSync(destination, 0o755);
  const version = versionOf(destination);
  if (
    !version ||
    version.target !== target ||
    version.protocol !== PROTOCOL ||
    version.name !== 'aac-usage-scan' ||
    version.version !== crateVersion()
  ) {
    console.error('[X] The built scanner did not answer --version for this target.');
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
    `[OK] ${target} scanner in native/aac-usage/prebuilt/${target}/ (${note.sha256.slice(0, 12)})`
  );
}

function copyOnly() {
  const current = sourceHash();
  const targets = {};
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
      console.warn(`[!] ${target} scanner was built from other sources; it is not shipped.`);
      continue;
    }
    if (note.protocol !== PROTOCOL) {
      console.warn(`[!] ${target} scanner speaks protocol ${note.protocol}; it is not shipped.`);
      continue;
    }
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
    name: 'aac-usage-scan',
    version: crateVersion(),
    protocol: PROTOCOL,
    sourceSha256: current,
    targets,
  };
  fs.writeFileSync(path.join(DIST, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  const count = Object.keys(targets).length;
  console.log(
    count
      ? `[OK] Usage-log scanner for ${Object.keys(targets).join(', ')} copied into dist/native/aac-usage/`
      : '[i] No usage-log scanner built from these sources.'
  );
}

/** Every shipped scanner matches the manifest beside it (verify-bundle). */
function verifyDist(dir = DIST) {
  const manifestPath = path.join(dir, 'manifest.json');
  if (!fs.existsSync(manifestPath)) return { present: false, targets: [] };
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  for (const [target, entry] of Object.entries(manifest.targets || {})) {
    if (!SUPPORTED.includes(target)) throw new Error(`Unknown scanner target ${target}.`);
    const expected = `${target}/${binaryName(target)}`;
    const file = path.join(dir, expected);
    if (
      entry.file !== expected ||
      !fs.existsSync(file) ||
      fileSha(file) !== entry.sha256 ||
      fs.statSync(file).size !== entry.size
    )
      throw new Error(`The ${target} scanner does not match its manifest.`);
  }
  return { present: true, targets: Object.keys(manifest.targets || {}) };
}

function verifyLocalDist(dir = DIST) {
  const result = verifyDist(dir);
  if (!result.targets.includes(LOCAL_TARGET)) {
    throw new Error(
      `Missing usage-log scanner for this computer (${process.platform}-${process.arch}). Run build:server before staging.`
    );
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8'));
  const version = versionOf(path.join(dir, LOCAL_TARGET, binaryName(LOCAL_TARGET)));
  if (
    !version ||
    manifest.name !== 'aac-usage-scan' ||
    manifest.protocol !== PROTOCOL ||
    manifest.version !== crateVersion() ||
    manifest.sourceSha256 !== sourceHash() ||
    version.name !== manifest.name ||
    version.target !== LOCAL_TARGET ||
    version.protocol !== manifest.protocol ||
    version.version !== manifest.version
  ) {
    throw new Error(`The ${LOCAL_TARGET} scanner does not match the current source or protocol.`);
  }
  return result;
}

if (require.main === module) {
  if (process.argv.includes('--copy-only')) copyOnly();
  else {
    build();
    if (process.argv.includes('--stage')) {
      copyOnly();
      verifyLocalDist();
    }
  }
}

module.exports = { sourceHash, verifyDist, verifyLocalDist, binaryName, SUPPORTED, TARGETS };
