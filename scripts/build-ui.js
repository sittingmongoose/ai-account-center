#!/usr/bin/env node
'use strict';

// Builds the web dashboard: the Leptos app (web-dashboard/leptos, Leptos 0.9 client-side rendered, compiled to
// WebAssembly) and the JavaScript data layer it loads (web-dashboard/public), into dist/ui. The runtime is
// content-addressed (pkg/<buildId>/), the frame page is app/index.html, the service worker is stamped with the shell
// build id, larger files get byte-identical .br and .gz copies, and ui-build-manifest.json records all of it.

const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const zlib = require('zlib');
const { spawnSync } = require('child_process');
const { parse } = require('smol-toml');
const { runWithUiBuildLock, wasmTargetDir } = require('./ui-build-lock');

const LEPTOS_NAME = 'aac_web';
const LEPTOS_VERSION = '0.9.0-rc1';
const WASM_PACK_VERSION = '0.15.1';
const MINIMUM_RUST_MINOR = 99;
/**
 * wasm-pack installs the wasm-bindgen CLI at the version in Cargo.lock. These pins are the
 * newest release of each crate (2026-11-08); the CLI and the lockfile must be the same version.
 */
const WASM_BINDGEN_PINS = {
  'wasm-bindgen': '0.2.130',
  'wasm-bindgen-futures': '0.4.80',
  'js-sys': '0.3.107',
  'web-sys': '0.3.107',
};
/** The app frame's page: the server answers every page route with it. */
const APP_ENTRY = 'app/index.html';
/** The mono font the pages load from app/, with its licence. */
const APP_FONTS = ['MartianMono.ttf', 'OFL-MartianMono.txt'];
/** The service worker's build placeholder, stamped with the shell build id. */
const SW_BUILD_ID = '__AAC_BUILD_ID__';
const SW_BUILD_ID_ERROR = 'sw.js must name the __AAC_BUILD_ID__ placeholder exactly once.';
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
/** The Leptos crate's build inputs, relative to the crate. */
const LEPTOS_INPUTS = [
  '.cargo/config.toml',
  'rust-toolchain.toml',
  'Cargo.toml',
  'Cargo.lock',
  'index.html',
  'build',
  'fonts',
  'src',
  'style',
];

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

function assertToolchain(run, env, cargoBin, crate) {
  const rust = run(toolPath('rustc', cargoBin), ['--version'], { cwd: crate, env, capture: true });
  const version = /^rustc (\d+)\.(\d+)\./.exec(rust);
  if (
    !version ||
    Number(version[1]) < 1 ||
    (Number(version[1]) === 1 && Number(version[2]) < MINIMUM_RUST_MINOR)
  ) {
    throw new Error(
      'The dashboard requires Rust 1.99.1 or newer. Install the pinned browser toolchain with rustup toolchain install 1.99.1.'
    );
  }
  const targets = run(toolPath('rustup', cargoBin), ['target', 'list', '--installed'], {
    cwd: crate,
    env,
    capture: true,
  });
  if (!targets.split(/\s+/).includes('wasm32-unknown-unknown')) {
    throw new Error('Install the browser target with rustup target add wasm32-unknown-unknown.');
  }
  const wasmPack = run(toolPath('wasm-pack', cargoBin), ['--version'], {
    cwd: crate,
    env,
    capture: true,
  });
  if (wasmPack.trim() !== `wasm-pack ${WASM_PACK_VERSION}`) {
    throw new Error(
      `The dashboard requires wasm-pack ${WASM_PACK_VERSION}. Run cargo install wasm-pack --version ${WASM_PACK_VERSION} --locked.`
    );
  }
}

/** One version of each wasm-bindgen crate, so the CLI wasm-pack downloads matches the lockfile. */
function assertLockedWasmBindgen(lock, label) {
  const packages = lock.package ?? [];
  for (const [name, version] of Object.entries(WASM_BINDGEN_PINS)) {
    const entries = packages.filter((entry) => entry.name === name);
    if (entries.length !== 1 || entries[0].version !== version) {
      const found = entries.map((entry) => entry.version).join(', ') || 'missing';
      throw new Error(
        `${label} Cargo.lock must resolve ${name} to ${version} so the wasm-bindgen CLI matches the lockfile (found ${found}).`
      );
    }
  }
}

/** The manifest's Leptos version must be what Cargo.lock resolves (the crate pins `=LEPTOS_VERSION`). */
function assertLockedLeptos(lock) {
  const entries = (lock.package ?? []).filter((entry) => entry.name === 'leptos');
  if (entries.length !== 1 || entries[0].version !== LEPTOS_VERSION) {
    throw new Error(`web-dashboard/leptos/Cargo.lock must resolve leptos to ${LEPTOS_VERSION}.`);
  }
}

/** lstat that returns null only when nothing is there (a dangling symlink is still there). */
function lstatOrNull(absolute) {
  try {
    return fs.lstatSync(absolute);
  } catch (error) {
    if (error && error.code === 'ENOENT') return null;
    throw error;
  }
}

function normalizedFingerprintPaths(files) {
  return files.map((file) => file.split(/[\\/]/).join('/')).sort();
}

/**
 * Hash a crate's build inputs: each input path relative to `root`, walked without following links. The root, every
 * folder on the way to an input and every input must be a real folder or file; any symlink (dangling or not) fails.
 */
function fingerprintInputs(root, inputs, label, relativePath = path) {
  const rootStat = lstatOrNull(root);
  if (!rootStat || rootStat.isSymbolicLink() || !rootStat.isDirectory())
    throw new Error(`${label} build root must be a real folder.`);
  const files = [];
  function collect(relative) {
    const absolute = path.join(root, ...relative.split(/[\\/]/));
    const stat = lstatOrNull(absolute);
    if (!stat) return;
    if (stat.isSymbolicLink()) throw new Error(`${label} build inputs must not be symlinks.`);
    if (stat.isDirectory()) {
      for (const entry of fs.readdirSync(absolute).sort())
        collect(relativePath.join(relative, entry));
    } else if (stat.isFile()) files.push(relative);
  }
  for (const input of inputs) {
    // a linked parent folder (such as .cargo) would be followed silently by path.join
    const parts = input.split(/[\\/]/);
    let missing = false;
    for (let i = 1; i < parts.length && !missing; i++) {
      const stat = lstatOrNull(path.join(root, ...parts.slice(0, i)));
      if (!stat) missing = true;
      else if (stat.isSymbolicLink() || !stat.isDirectory())
        throw new Error(`${label} build inputs must not be symlinks.`);
    }
    if (!missing) collect(input);
  }
  const hash = crypto.createHash('sha256');
  for (const file of normalizedFingerprintPaths(files)) {
    hash.update(file);
    hash.update('\0');
    hash.update(fs.readFileSync(path.join(root, ...file.split('/'))));
    hash.update('\0');
  }
  return { sha256: hash.digest('hex'), files: files.length };
}

/** Fingerprint the Leptos app and its shared computer registry (public/computers.json). */
function leptosFingerprint(leptosCrate) {
  // Validate the crate itself before widening the root to include the shared registry.
  fingerprintInputs(leptosCrate, LEPTOS_INPUTS, 'Leptos');
  return fingerprintInputs(
    path.dirname(leptosCrate),
    [
      ...LEPTOS_INPUTS.map((input) => `${path.basename(leptosCrate)}/${input}`),
      'public/computers.json',
    ],
    'Leptos'
  );
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

/**
 * The files the build writes under app/ for a runtime build id: the frame page (its template with the runtime's
 * address and the stylesheet's content stamp), the stylesheet and the mono font with its licence. Keyed by their
 * path in dist/ui, so the build and ui:validate hash the same bytes.
 */
function stagedAppFiles(leptosCrate, buildId) {
  const template = fs.readFileSync(path.join(leptosCrate, 'index.html'), 'utf8');
  if (!template.includes('{{PKG}}') || !template.includes('{{CSS}}'))
    throw new Error('The Leptos page template must reference {{PKG}} and {{CSS}}.');
  // The stylesheet keeps one path but its link names the content, so a phone that kept the old file reads the new one.
  const css = fs.readFileSync(path.join(leptosCrate, 'style', 'app.css'));
  const cssVersion = sha256(css).slice(0, 12);
  const files = new Map([
    [
      APP_ENTRY,
      Buffer.from(
        template
          .replaceAll('{{PKG}}', `/pkg/${buildId}`)
          .replaceAll('{{CSS}}', `/app/app.css?v=${cssVersion}`),
        'utf8'
      ),
    ],
    ['app/app.css', css],
  ]);
  for (const font of APP_FONTS) {
    files.set(`app/${font}`, fs.readFileSync(path.join(leptosCrate, 'fonts', font)));
  }
  return files;
}

/** Stamp the shell build id into the packaged sw.js only; the source keeps the placeholder. */
function stampedSwSource(source, buildId) {
  if (countLiteral(source, SW_BUILD_ID) !== 1) throw new Error(SW_BUILD_ID_ERROR);
  return source.replace(SW_BUILD_ID, buildId);
}

/**
 * Hash every packaged shell file (path + bytes, sorted): each file under `publicDir` and the staged app/ files
 * (`appFiles`, from stagedAppFiles). Any shell change, the frame page's runtime address included, then rolls the
 * service worker's stamp and cache, while pkg/<buildId>/ stays content-addressed by the wasm hash. Deterministic:
 * identical shell inputs give an identical hash.
 */
function shellFingerprint(publicDir, appFiles = new Map()) {
  const rows = listFiles(publicDir).map((relative) => [
    relative,
    () => fs.readFileSync(path.join(publicDir, ...relative.split('/'))),
  ]);
  for (const [relative, bytes] of appFiles) rows.push([relative, () => bytes]);
  rows.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const hash = crypto.createHash('sha256');
  for (const [relative, read] of rows) {
    hash.update(relative);
    hash.update('\0');
    hash.update(read());
    hash.update('\0');
  }
  return { sha256: hash.digest('hex'), files: rows.length };
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
  const leptosCrate = path.join(crate, 'leptos');
  const publicDir = path.join(crate, 'public');
  const pkg = path.join(leptosCrate, 'pkg');
  const packagedUi = path.join(repoRoot, 'dist', 'ui');

  // A linked or incomplete crate, and a lock that drifted from the pinned versions, are refused before anything runs.
  const source = leptosFingerprint(leptosCrate);
  if (!fs.existsSync(path.join(leptosCrate, 'Cargo.toml')))
    throw new Error('Missing the Leptos dashboard crate: web-dashboard/leptos/Cargo.toml.');
  const lock = parse(fs.readFileSync(path.join(leptosCrate, 'Cargo.lock'), 'utf8'));
  assertLockedLeptos(lock);
  assertLockedWasmBindgen(lock, 'Leptos');
  assertToolchain(run, env, cargoBin, leptosCrate);
  // wasm-pack writes into this worktree's own web-dashboard/leptos/pkg (--out-dir is crate-relative), even though the
  // cargo target folder is shared. One release wasm build at a time per computer.
  runWithUiBuildLock(
    toolPath('wasm-pack', cargoBin),
    [
      'build',
      leptosCrate,
      '--release',
      '--target',
      'web',
      '--out-dir',
      'pkg',
      '--out-name',
      LEPTOS_NAME,
      '--',
      '--locked',
    ],
    { cwd: repoRoot, env },
    { env, run, ...options.lock }
  );
  const wasmPath = path.join(pkg, `${LEPTOS_NAME}_bg.wasm`);
  for (const file of [
    path.join(pkg, `${LEPTOS_NAME}.js`),
    wasmPath,
    path.join(leptosCrate, 'index.html'),
  ]) {
    if (!fs.existsSync(file))
      throw new Error(`Missing dashboard build artifact: ${path.relative(repoRoot, file)}.`);
  }
  const { stripNameSection } = require(path.join(leptosCrate, 'build', 'strip-names.cjs'));
  const wasm = stripNameSection(fs.readFileSync(wasmPath));
  if (!wasm.subarray(0, 8).equals(Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]))) {
    throw new Error('The dashboard build output is not a valid WebAssembly module.');
  }
  fs.writeFileSync(wasmPath, wasm);
  const wasmSha256 = sha256(wasm);
  // The pkg folder is named after the wasm content, so pkg/<buildId>/** can be cached forever.
  const buildId = wasmSha256.slice(0, 12);
  // Everything is checked before the old output is removed, so a failed build never leaves dist/ui empty. The service
  // worker is stamped with the shell id, not the wasm id: any shell change (the frame page and its stylesheet, sw.js,
  // sw-route.js, every *.mjs and the other packaged files) rolls the worker and its cache, while pkg/<buildId>/ stays
  // content-addressed by the wasm hash.
  const appFiles = stagedAppFiles(leptosCrate, buildId);
  const shell = shellFingerprint(publicDir, appFiles);
  const shellBuildId = shell.sha256.slice(0, 12);
  // Fixtures without a worker are left alone.
  const swPath = path.join(publicDir, 'sw.js');
  const sw = fs.existsSync(swPath)
    ? stampedSwSource(fs.readFileSync(swPath, 'utf8'), shellBuildId)
    : null;
  fs.rmSync(packagedUi, { recursive: true, force: true });
  fs.mkdirSync(packagedUi, { recursive: true });
  fs.cpSync(publicDir, packagedUi, { recursive: true });
  if (sw !== null) fs.writeFileSync(path.join(packagedUi, 'sw.js'), sw);
  // wasm-pack's generated '*' ignore rule would hide the runtime from npm pack.
  fs.cpSync(pkg, path.join(packagedUi, 'pkg', buildId), {
    recursive: true,
    filter: (file) =>
      !['.gitignore', '.npmignore', 'package.json', 'README.md'].includes(path.basename(file)),
  });
  fs.mkdirSync(path.join(packagedUi, 'app'), { recursive: true });
  for (const [relative, bytes] of appFiles)
    fs.writeFileSync(path.join(packagedUi, ...relative.split('/')), bytes);
  const precompressed = precompressUi(packagedUi);
  const commit = (options.readCommit ?? readBuildCommit)(repoRoot);
  const manifest = {
    schemaVersion: 2,
    framework: 'leptos',
    version: LEPTOS_VERSION,
    target: 'wasm32-unknown-unknown',
    entry: APP_ENTRY,
    source,
    buildId,
    shellBuildId,
    shell,
    ...(typeof commit === 'string' && /^[a-f0-9]{7,40}$/.test(commit) ? { commit } : {}),
    wasm: {
      path: `pkg/${buildId}/${LEPTOS_NAME}_bg.wasm`,
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

/**
 * Opt-in refusal for machines that must not compile Rust/WebAssembly (a shared VM
 * whose memory other work needs). Set AAC_NO_LOCAL_RUST_BUILD=1 there; nothing is
 * keyed to a host name.
 */
function localRustBuildRefusal(env = process.env) {
  if (env.AAC_NO_LOCAL_RUST_BUILD !== '1') return null;
  return (
    'This machine sets AAC_NO_LOCAL_RUST_BUILD=1, so the dashboard is not compiled here. ' +
    'Build on a build host (the landing gate or the Windows build recipe) instead.'
  );
}

if (require.main === module) {
  const refusal = localRustBuildRefusal();
  if (refusal) {
    console.error(`[X] ${refusal}`);
    process.exit(1);
  }
  try {
    const manifest = buildUi();
    console.log(
      `[OK] Leptos ${manifest.version} dashboard: ${manifest.wasm.bytes} bytes of WebAssembly.`
    );
  } catch (error) {
    console.error(`[X] ${error instanceof Error ? error.message : 'Dashboard build failed.'}`);
    process.exitCode = 1;
  }
}

module.exports = {
  buildUi,
  localRustBuildRefusal,
  readBuildCommit,
  assertLockedLeptos,
  assertLockedWasmBindgen,
  stampedSwSource,
  stagedAppFiles,
  shellFingerprint,
  fingerprintInputs,
  normalizedFingerprintPaths,
  leptosFingerprint,
  commandRunner,
  toolPath,
  SW_BUILD_ID_ERROR,
  PRECOMPRESSED_EXTENSIONS,
  APP_ENTRY,
  LEPTOS_NAME,
  LEPTOS_VERSION,
};
