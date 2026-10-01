#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { spawnSync } = require('child_process');
const { parse } = require('smol-toml');

const SLINT_VERSION = '1.18.1';
const MINIMUM_RUST_MINOR = 92;
const WASM_NAME = 'ccs_account_dashboard';

function assertSlintPin(manifest) {
  for (const [section, dependency] of [
    ['dependencies', 'slint'],
    ['build-dependencies', 'slint-build'],
  ]) {
    const entry = manifest[section]?.[dependency];
    const version = typeof entry === 'string' ? entry : entry?.version;
    if (version !== `=${SLINT_VERSION}`) {
      throw new Error(`${dependency} must be pinned to =${SLINT_VERSION}.`);
    }
  }
}

function commandRunner(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd,
    env: options.env,
    encoding: 'utf8',
    stdio: options.capture ? 'pipe' : 'inherit',
    windowsHide: true,
  });
  if (result.error || result.status !== 0) {
    throw new Error(
      `${path.basename(command)} failed${result.status === null ? '' : ` with exit ${result.status}`}.`
    );
  }
  return options.capture ? result.stdout : '';
}

function toolPath(name, cargoBin) {
  const candidate = path.join(cargoBin, process.platform === 'win32' ? `${name}.exe` : name);
  return fs.existsSync(candidate) ? candidate : name;
}

function assertToolchain(run, env, cargoBin) {
  const rust = run(toolPath('rustc', cargoBin), ['--version'], { env, capture: true });
  const version = /^rustc (\d+)\.(\d+)\./.exec(rust);
  if (
    !version ||
    Number(version[1]) < 1 ||
    (Number(version[1]) === 1 && Number(version[2]) < MINIMUM_RUST_MINOR)
  ) {
    throw new Error('The Slint dashboard requires Rust 1.92 or newer. Run rustup update stable.');
  }
  const targets = run(toolPath('rustup', cargoBin), ['target', 'list', '--installed'], {
    env,
    capture: true,
  });
  if (!targets.split(/\s+/).includes('wasm32-unknown-unknown')) {
    throw new Error('Install the browser target with rustup target add wasm32-unknown-unknown.');
  }
  run(toolPath('wasm-pack', cargoBin), ['--version'], { env, capture: true });
}

function assertLockedSlint(lock) {
  const packages = lock.package ?? [];
  for (const dependency of ['slint', 'slint-build']) {
    const entries = packages.filter((entry) => entry.name === dependency);
    if (entries.length !== 1 || entries[0].version !== SLINT_VERSION) {
      throw new Error(`Cargo.lock must resolve ${dependency} to ${SLINT_VERSION}.`);
    }
  }
}

/** Fingerprint build inputs, including the same-origin login/control bridge. */
function sourceFingerprint(crate) {
  const files = [];
  function collect(relative) {
    const absolute = path.join(crate, relative);
    if (!fs.existsSync(absolute)) return;
    const stat = fs.lstatSync(absolute);
    if (stat.isSymbolicLink()) throw new Error('Dashboard build inputs must not be symlinks.');
    if (stat.isDirectory()) {
      for (const entry of fs.readdirSync(absolute).sort()) collect(path.join(relative, entry));
    } else if (stat.isFile()) files.push(relative);
  }
  for (const input of ['Cargo.toml', 'Cargo.lock', 'build.rs', 'src', 'ui', 'public'])
    collect(input);
  const hash = crypto.createHash('sha256');
  for (const file of files.sort()) {
    hash.update(file.split(path.sep).join('/'));
    hash.update('\0');
    hash.update(fs.readFileSync(path.join(crate, file)));
    hash.update('\0');
  }
  return { sha256: hash.digest('hex'), files: files.length };
}

function buildUi(options = {}) {
  const repoRoot = options.repoRoot ?? path.resolve(__dirname, '..');
  const run = options.run ?? commandRunner;
  const cargoBin = path.join(os.homedir(), '.cargo', 'bin');
  const env = { ...process.env, PATH: `${cargoBin}${path.delimiter}${process.env.PATH ?? ''}` };
  const crate = path.join(repoRoot, 'web-dashboard');
  const publicDir = path.join(crate, 'public');
  const pkg = path.join(crate, 'pkg');
  const packagedUi = path.join(repoRoot, 'dist', 'ui');

  assertSlintPin(parse(fs.readFileSync(path.join(crate, 'Cargo.toml'), 'utf8')));
  assertLockedSlint(parse(fs.readFileSync(path.join(crate, 'Cargo.lock'), 'utf8')));
  assertToolchain(run, env, cargoBin);
  run(
    toolPath('wasm-pack', cargoBin),
    [
      'build',
      crate,
      '--release',
      '--target',
      'web',
      '--out-dir',
      'pkg',
      '--out-name',
      WASM_NAME,
      '--',
      '--locked',
    ],
    { cwd: repoRoot, env }
  );

  const requiredFiles = [
    path.join(publicDir, 'index.html'),
    path.join(publicDir, 'bridge.js'),
    path.join(pkg, `${WASM_NAME}.js`),
    path.join(pkg, `${WASM_NAME}_bg.wasm`),
  ];
  for (const file of requiredFiles) {
    if (!fs.existsSync(file))
      throw new Error(`Missing Slint browser artifact: ${path.relative(repoRoot, file)}.`);
  }
  const wasm = fs.readFileSync(path.join(pkg, `${WASM_NAME}_bg.wasm`));
  if (!wasm.subarray(0, 8).equals(Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]))) {
    throw new Error('Slint browser output is not a valid WebAssembly module.');
  }
  fs.rmSync(packagedUi, { recursive: true, force: true });
  fs.mkdirSync(packagedUi, { recursive: true });
  fs.cpSync(publicDir, packagedUi, { recursive: true });
  // wasm-pack's generated '*' ignore rule would hide the runtime from npm pack.
  fs.cpSync(pkg, path.join(packagedUi, 'pkg'), {
    recursive: true,
    filter: (source) => !['.gitignore', '.npmignore'].includes(path.basename(source)),
  });
  const manifest = {
    schemaVersion: 1,
    framework: 'slint',
    version: SLINT_VERSION,
    target: 'wasm32-unknown-unknown',
    entry: 'index.html',
    source: sourceFingerprint(crate),
    wasm: {
      path: `pkg/${WASM_NAME}_bg.wasm`,
      bytes: wasm.length,
      sha256: crypto.createHash('sha256').update(wasm).digest('hex'),
    },
  };
  fs.writeFileSync(
    path.join(packagedUi, 'ui-build-manifest.json'),
    `${JSON.stringify(manifest, null, 2)}\n`
  );
  return manifest;
}

if (require.main === module) {
  try {
    const manifest = buildUi();
    console.log(
      `[OK] Slint ${manifest.version} dashboard: ${manifest.wasm.bytes} bytes of WebAssembly.`
    );
  } catch (error) {
    console.error(
      `[X] ${error instanceof Error ? error.message : 'Slint dashboard build failed.'}`
    );
    process.exitCode = 1;
  }
}

module.exports = {
  buildUi,
  assertSlintPin,
  assertLockedSlint,
  sourceFingerprint,
  commandRunner,
  toolPath,
};
