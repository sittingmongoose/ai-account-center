#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const zlib = require('zlib');
const { spawnSync } = require('child_process');
const { parse } = require('smol-toml');
const { runWithUiBuildLock, wasmTargetDir } = require('./ui-build-lock');

const SLINT_VERSION = '1.18.1';
const MINIMUM_RUST_MINOR = 92;
const WASM_NAME = 'ccs_account_dashboard';
/** The one import of the wasm-bindgen glue that the packaged bridge.js rewrites. */
const BRIDGE_IMPORT = `'./pkg/${WASM_NAME}.js'`;
const BRIDGE_IMPORT_ERROR = `bridge.js must import ./pkg/${WASM_NAME}.js exactly once.`;
const PRECOMPRESSED_EXTENSIONS = new Set([
  '.wasm',
  '.js',
  '.mjs',
  '.css',
  '.html',
  '.svg',
  '.json',
  '.txt',
]);
const PRECOMPRESS_MIN_BYTES = 1024;
/** A variant is kept only when it is smaller than this share of the original. */
const PRECOMPRESS_MAX_RATIO = 0.9;

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
    const error = new Error(
      `${path.basename(command)} failed${result.status === null ? '' : ` with exit ${result.status}`}.`
    );
    error.status = result.status;
    throw error;
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

function sha256(buffer) {
  return crypto.createHash('sha256').update(buffer).digest('hex');
}

function countLiteral(text, literal) {
  let count = 0;
  for (let index = text.indexOf(literal); index !== -1; index = text.indexOf(literal, index + 1))
    count++;
  return count;
}

/** The source bridge.js must name the unversioned glue import exactly once. */
function assertBridgeImport(source) {
  if (countLiteral(source, BRIDGE_IMPORT) !== 1) throw new Error(BRIDGE_IMPORT_ERROR);
}

/** Rewrite only the packaged copy; web-dashboard/public/bridge.js stays as written. */
function versionedBridgeSource(source, buildId) {
  assertBridgeImport(source);
  const index = source.indexOf(BRIDGE_IMPORT);
  return `${source.slice(0, index)}'./pkg/${buildId}/${WASM_NAME}.js'${source.slice(index + BRIDGE_IMPORT.length)}`;
}

/** Relative POSIX paths of every file below a directory, sorted for reproducible output. */
function listFiles(directory, relative = '') {
  const files = [];
  for (const entry of fs.readdirSync(path.join(directory, relative)).sort()) {
    const child = relative ? `${relative}/${entry}` : entry;
    const stat = fs.lstatSync(path.join(directory, child));
    if (stat.isSymbolicLink()) throw new Error('Dashboard build output must not contain symlinks.');
    if (stat.isDirectory()) files.push(...listFiles(directory, child));
    else if (stat.isFile()) files.push(child);
  }
  return files;
}

/**
 * Brotli (quality 11) and gzip (level 9) copies of the larger text and wasm files.
 * Node's gzip header carries mtime 0, so the same input gives the same bytes.
 */
function precompressUi(uiDir) {
  const entries = [];
  for (const relative of listFiles(uiDir)) {
    const extension = path.extname(relative).toLowerCase();
    if (!PRECOMPRESSED_EXTENSIONS.has(extension)) continue;
    const absolute = path.join(uiDir, ...relative.split('/'));
    const original = fs.readFileSync(absolute);
    if (original.length < PRECOMPRESS_MIN_BYTES) continue;
    const variants = [
      [
        'br',
        '.br',
        zlib.brotliCompressSync(original, {
          params: {
            [zlib.constants.BROTLI_PARAM_QUALITY]: 11,
            [zlib.constants.BROTLI_PARAM_SIZE_HINT]: original.length,
            [zlib.constants.BROTLI_PARAM_MODE]:
              extension === '.wasm'
                ? zlib.constants.BROTLI_MODE_GENERIC
                : zlib.constants.BROTLI_MODE_TEXT,
          },
        }),
      ],
      ['gzip', '.gz', zlib.gzipSync(original, { level: 9 })],
    ];
    for (const [encoding, suffix, bytes] of variants) {
      if (bytes.length >= original.length * PRECOMPRESS_MAX_RATIO) continue;
      fs.writeFileSync(`${absolute}${suffix}`, bytes);
      entries.push({
        path: relative,
        encoding,
        file: `${relative}${suffix}`,
        bytes: bytes.length,
        sha256: sha256(bytes),
      });
    }
  }
  return entries;
}

/** Packaged modes never depend on the builder's umask or on source file modes. */
function normalizeModes(directory) {
  fs.chmodSync(directory, 0o755);
  for (const entry of fs.readdirSync(directory)) {
    const absolute = path.join(directory, entry);
    const stat = fs.lstatSync(absolute);
    if (stat.isSymbolicLink()) throw new Error('Dashboard build output must not contain symlinks.');
    if (stat.isDirectory()) normalizeModes(absolute);
    else fs.chmodSync(absolute, 0o644);
  }
}

/**
 * The short commit of the packaged source, or null when `repoRoot` is not itself
 * the top of a git checkout (an unpacked copy inside some other repository must
 * not record that repository's commit).
 */
function readBuildCommit(repoRoot) {
  try {
    const result = spawnSync(
      'git',
      ['-C', repoRoot, 'rev-parse', '--show-toplevel', '--short=8', 'HEAD'],
      {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
        timeout: 5000,
        windowsHide: true,
      }
    );
    if (result.status !== 0) return null;
    const [topLevel = '', commit = ''] = String(result.stdout).trim().split(/\r?\n/);
    if (!topLevel || fs.realpathSync(path.resolve(topLevel)) !== fs.realpathSync(repoRoot)) {
      return null;
    }
    return /^[a-f0-9]{7,40}$/.test(commit.trim()) ? commit.trim() : null;
  } catch {
    return null;
  }
}

function buildUi(options = {}) {
  const repoRoot = options.repoRoot ?? path.resolve(__dirname, '..');
  const run = options.run ?? commandRunner;
  const cargoBin = path.join(os.homedir(), '.cargo', 'bin');
  const baseEnv = options.env ?? process.env;
  // Every worktree shares one incremental wasm target unless the caller names its own.
  const env = {
    ...baseEnv,
    PATH: `${cargoBin}${path.delimiter}${baseEnv.PATH ?? ''}`,
    CARGO_TARGET_DIR: wasmTargetDir(baseEnv),
  };
  const crate = path.join(repoRoot, 'web-dashboard');
  const publicDir = path.join(crate, 'public');
  const pkg = path.join(crate, 'pkg');
  const packagedUi = path.join(repoRoot, 'dist', 'ui');

  assertSlintPin(parse(fs.readFileSync(path.join(crate, 'Cargo.toml'), 'utf8')));
  assertLockedSlint(parse(fs.readFileSync(path.join(crate, 'Cargo.lock'), 'utf8')));
  assertToolchain(run, env, cargoBin);
  // wasm-pack writes into this worktree's own web-dashboard/pkg (--out-dir is crate-relative),
  // even though the cargo target folder is shared.
  // One release wasm build (a multi-GB fat-LTO link) at a time per computer.
  runWithUiBuildLock(
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
    { cwd: repoRoot, env },
    { env, run, ...options.lock }
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
  const wasmSha256 = sha256(wasm);
  // The pkg folder is named after the wasm content, so pkg/<buildId>/** can be cached forever.
  const buildId = wasmSha256.slice(0, 12);
  // Checked before the old output is removed, so a bad bridge never leaves dist/ui empty.
  const bridge = versionedBridgeSource(
    fs.readFileSync(path.join(publicDir, 'bridge.js'), 'utf8'),
    buildId
  );
  fs.rmSync(packagedUi, { recursive: true, force: true });
  fs.mkdirSync(packagedUi, { recursive: true });
  fs.cpSync(publicDir, packagedUi, { recursive: true });
  fs.writeFileSync(path.join(packagedUi, 'bridge.js'), bridge);
  // wasm-pack's generated '*' ignore rule would hide the runtime from npm pack.
  fs.cpSync(pkg, path.join(packagedUi, 'pkg', buildId), {
    recursive: true,
    filter: (source) => !['.gitignore', '.npmignore'].includes(path.basename(source)),
  });
  const precompressed = precompressUi(packagedUi);
  const commit = (options.readCommit ?? readBuildCommit)(repoRoot);
  const manifest = {
    schemaVersion: 1,
    framework: 'slint',
    version: SLINT_VERSION,
    target: 'wasm32-unknown-unknown',
    entry: 'index.html',
    source: sourceFingerprint(crate),
    buildId,
    ...(typeof commit === 'string' && /^[a-f0-9]{7,40}$/.test(commit) ? { commit } : {}),
    wasm: {
      path: `pkg/${buildId}/${WASM_NAME}_bg.wasm`,
      bytes: wasm.length,
      sha256: wasmSha256,
    },
    precompressed,
  };
  // Written last, then every mode is set explicitly.
  fs.writeFileSync(
    path.join(packagedUi, 'ui-build-manifest.json'),
    `${JSON.stringify(manifest, null, 2)}\n`
  );
  normalizeModes(packagedUi);
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
  readBuildCommit,
  assertSlintPin,
  assertLockedSlint,
  assertBridgeImport,
  versionedBridgeSource,
  sourceFingerprint,
  commandRunner,
  toolPath,
  BRIDGE_IMPORT_ERROR,
  PRECOMPRESSED_EXTENSIONS,
  WASM_NAME,
};
