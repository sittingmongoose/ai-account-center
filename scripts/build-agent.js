#!/usr/bin/env node
'use strict';
/**
 * The host agent binary (crates/aac-agent; TARGET-DESIGN 5.2, DESIGN-M3-COMPLETE 4.5, lane E):
 * one self-contained `aac-agent` per target, hash-pinned in the trusted catalog
 * (config/agent-artifacts.json). M3D4 answered: ship in stages, VM first;
 * M3D5 answered: the VM's current key with AAC-pinned known_hosts. The service admits only an
 * already installed agent; the lead places it on each remote computer with scripts/agent-install.sh
 * or scripts/agent-install.ps1. This script never installs on a live computer.
 *
 *   node scripts/build-agent.js            (bun run agent:build)
 *     Compiles the agent for this computer (cargo build --release --locked -p aac-agent) and keeps
 *     it in crates/aac-agent/prebuilt/<target>/aac-agent[.exe] with a note of the sources it was
 *     built from. Builds happen on Nas1 (Linux), the Mac and Windows (aac-winbuild), never on the
 *     dashboard VM; the landing collects each machine's prebuilt/<target> folder. Linux builds are
 *     glibc until the musl target is authorized (discovery F11); both are admitted on Linux.
 *     On Nas1 the lead's memory rule applies: wait for 6 GB available (re-check 30 s), nice -n 19,
 *     -j 2, because the landing gate has priority there.
 *
 *   node scripts/build-agent.js --copy-only   (every `build` runs this)
 *     Copies every prebuilt target built from this checkout's sources into dist/agent/ and writes
 *     manifest.json there: {name, version, protocol, sourceSha256, targets: {<target>: {file,
 *     sha256, size}}}. A target built from other sources is left out; a computer whose target is
 *     missing keeps the labelled helper fallback.
 *
 *   node scripts/build-agent.js --catalog-entry
 *     Prints the trusted-catalog entry for this computer's prebuilt target. The lead reviews and
 *     commits it into config/agent-artifacts.json; nothing here edits the catalog.
 */
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { spawnSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const PREBUILT = path.join(ROOT, 'crates', 'aac-agent', 'prebuilt');
const DIST = path.join(ROOT, 'dist', 'agent');
const CATALOG = path.join(ROOT, 'config', 'agent-artifacts.json');
const TARGETS = {
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'win32-x64': 'x86_64-pc-windows-msvc',
};
const SUPPORTED = Object.values(TARGETS);
/** The agent's production dependency closure; dev-dependencies and tests do not enter the binary. */
const CRATES = [
  'crates/aac-agent',
  'crates/aac-core',
  'crates/aac-files',
  'crates/aac-num',
  'crates/aac-protocol',
  'crates/aac-providers',
  'crates/aac-status',
  'native/aac-usage',
];
/** Production inputs embedded from outside those crates (include_str!). */
const EMBEDDED = [
  'config/private-files.json',
  'config/tables/providers.json',
  'config/tables/tools.json',
  'config/tables/hosts.json',
  'config/tables/antigravity-columns.json',
  'web-dashboard/public/computers.json',
  'tests/spec/wire-words.json',
];
const NAS1 = os.hostname() === 'ubuntu-agent-nas1';

function binaryName(target) {
  return target.endsWith('windows-msvc') ? 'aac-agent.exe' : 'aac-agent';
}

function fileSha(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

/** SHA-256 over the agent's build inputs in a fixed order, line endings ignored.
 * `root` defaults to the checkout; tests pass a fixture tree. */
function sourceHash(root = ROOT) {
  const files = ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml'];
  const walk = (rel) => {
    for (const entry of fs.readdirSync(path.join(root, rel), { withFileTypes: true })) {
      const child = `${rel}/${entry.name}`;
      if (entry.isDirectory()) walk(child);
      else files.push(child);
    }
  };
  for (const crate of CRATES) {
    for (const entry of fs.readdirSync(path.join(root, crate), { withFileTypes: true })) {
      const child = `${crate}/${entry.name}`;
      if (entry.isDirectory()) {
        if (entry.name === 'src') walk(child);
      } else files.push(child);
    }
  }
  files.push(...EMBEDDED);
  const hash = crypto.createHash('sha256');
  for (const rel of files.sort()) {
    const file = path.join(root, rel);
    if (!fs.existsSync(file)) continue;
    hash.update(rel);
    hash.update('\0');
    hash.update(fs.readFileSync(file, 'utf8').replace(/\r\n/g, '\n'));
    hash.update('\0');
  }
  return hash.digest('hex');
}

function agentVersion() {
  const match = fs
    .readFileSync(path.join(ROOT, 'Cargo.toml'), 'utf8')
    .match(/\[workspace\.package\][\s\S]*?^version = "([^"]+)"/m);
  return match ? match[1] : null;
}

/** The agent's own answer to --version, or null when it does not run here. */
function versionOf(file) {
  const result = spawnSync(file, ['--version'], { encoding: 'utf8', timeout: 10_000 });
  if (result.status !== 0) return null;
  try {
    return JSON.parse(result.stdout);
  } catch {
    return null;
  }
}

function memAvailableGb() {
  const match = /MemAvailable:\s+(\d+) kB/.exec(fs.readFileSync('/proc/meminfo', 'utf8'));
  return match ? Number(match[1]) / 1048576 : Infinity;
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** The lead's Nas1 rule: the landing gate has priority; never start cargo under 6 GB available. */
async function waitForNas1Memory() {
  while (memAvailableGb() < 6) {
    console.log('[i] Nas1: waiting for 6 GB available memory before cargo (re-check in 30 s)...');
    await sleep(30_000);
  }
}

async function build() {
  const target = TARGETS[`${process.platform}-${process.arch}`];
  if (!target) {
    console.error(`[X] No agent target for ${process.platform}-${process.arch}.`);
    process.exit(1);
  }
  const cargoTarget = process.env.CARGO_TARGET_DIR
    ? path.resolve(process.env.CARGO_TARGET_DIR)
    : path.join(ROOT, 'target');
  if (NAS1) await waitForNas1Memory();
  console.log('[i] Building the host agent (cargo build --release --locked -p aac-agent)...');
  let command = 'cargo';
  let args = ['build', '--release', '--locked', '-p', 'aac-agent'];
  if (NAS1) {
    command = 'nice';
    args = ['-n', '19', 'cargo', ...args, '-j', '2'];
  }
  const result = spawnSync(command, args, {
    cwd: ROOT,
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
  if (!version || version.name !== 'aac-agent' || version.target !== target) {
    console.error('[X] The built agent did not answer --version for this target.');
    process.exit(1);
  }
  const rustc = spawnSync('rustc', ['--version'], { encoding: 'utf8', cwd: ROOT });
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
    `[OK] ${target} agent in crates/aac-agent/prebuilt/${target}/ (${note.sha256.slice(0, 12)})`
  );
  copyOnly();
  catalogEntry();
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
      console.warn(`[!] ${target} agent was built from other sources; it is not shipped.`);
      continue;
    }
    if (protocol !== null && note.protocol !== protocol) {
      console.warn(`[!] ${target} agent speaks protocol ${note.protocol}; it is not shipped.`);
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
    name: 'aac-agent',
    version: agentVersion(),
    protocol,
    sourceSha256: current,
    targets,
  };
  fs.writeFileSync(path.join(DIST, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  const count = Object.keys(targets).length;
  console.log(
    count
      ? `[OK] Host agent for ${Object.keys(targets).join(', ')} copied into dist/agent/`
      : '[i] No host agent built from these sources: every computer keeps the labelled helper.'
  );
}

/** The trusted-catalog entry for this computer's prebuilt target; the lead reviews and commits it. */
function catalogEntry() {
  const target = TARGETS[`${process.platform}-${process.arch}`];
  if (!target) return;
  const file = path.join(PREBUILT, target, binaryName(target));
  if (!fs.existsSync(file)) {
    console.error('[X] No prebuilt agent for this target; run the build first.');
    process.exit(1);
  }
  const note = JSON.parse(fs.readFileSync(`${file}.json`, 'utf8'));
  const entry = {
    target,
    version: note.version,
    sha256: note.sha256,
    file: `dist/agent/${target}/${binaryName(target)}`,
  };
  console.log('[i] Trusted-catalog entry (review, then commit into agent-artifacts.json):');
  console.log(JSON.stringify(entry, null, 2));
}

/** Every shipped agent matches its manifest; every catalog entry present matches its pin. */
function verifyDist(dir = DIST) {
  const manifestPath = path.join(dir, 'manifest.json');
  if (!fs.existsSync(manifestPath)) return { present: false, targets: [], missing: [] };
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  if (manifest.name !== 'aac-agent') throw new Error('Not an agent manifest.');
  for (const [target, entry] of Object.entries(manifest.targets || {})) {
    if (!SUPPORTED.includes(target)) throw new Error(`Unknown agent target ${target}.`);
    const file = path.join(dir, entry.file);
    if (
      !fs.existsSync(file) ||
      fileSha(file) !== entry.sha256 ||
      fs.statSync(file).size !== entry.size
    )
      throw new Error(`The ${target} agent does not match its manifest.`);
  }
  const missing = [];
  const catalog = JSON.parse(fs.readFileSync(CATALOG, 'utf8'));
  for (const artifact of catalog.artifacts || []) {
    if (!artifact.file.startsWith('dist/agent/')) continue;
    const rel = artifact.file.slice('dist/agent/'.length);
    const entry = (manifest.targets || {})[artifact.target];
    const file = path.join(dir, rel);
    if (!entry || entry.file !== rel || !fs.existsSync(file)) {
      missing.push(artifact.target);
      continue;
    }
    if (
      entry.sha256 !== artifact.sha256 ||
      fileSha(file) !== artifact.sha256 ||
      artifact.version !== manifest.version
    )
      throw new Error(`The ${artifact.target} agent does not match its trusted catalog pin.`);
  }
  return { present: true, targets: Object.keys(manifest.targets || {}), missing };
}

if (require.main === module) {
  if (process.argv.includes('--copy-only')) copyOnly();
  else if (process.argv.includes('--catalog-entry')) catalogEntry();
  else
    build().catch((error) => {
      console.error(`[X] ${error instanceof Error ? error.message : 'Agent build failed.'}`);
      process.exit(1);
    });
}

module.exports = { sourceHash, verifyDist, binaryName, SUPPORTED, TARGETS, CRATES, EMBEDDED };
