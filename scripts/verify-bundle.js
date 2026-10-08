#!/usr/bin/env node

'use strict';

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const zlib = require('zlib');

const UI_DIR = path.join(__dirname, '../dist/ui');
const MAX_SIZE = 8 * 1024 * 1024;
const WASM_NAME = 'ccs_account_dashboard';
const BUILD_ID = /^[a-f0-9]{12}$/;
const SHA256 = /^[a-f0-9]{64}$/;
const VARIANT_SUFFIX = new Map([
  ['br', '.br'],
  ['gzip', '.gz'],
]);

function sha256(buffer) {
  return crypto.createHash('sha256').update(buffer).digest('hex');
}

function isVariantFile(name) {
  return name.endsWith('.br') || name.endsWith('.gz');
}

/** Gzipped size of the shipped files; .br/.gz copies are alternates of files already counted. */
function walkDir(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).reduce((total, file) => {
    const filePath = path.join(dir, file.name);
    if (file.isDirectory()) return total + walkDir(filePath);
    if (isVariantFile(file.name)) return total;
    return total + zlib.gzipSync(fs.readFileSync(filePath)).length;
  }, 0);
}

function listFiles(dir, relative = '') {
  const files = [];
  for (const entry of fs.readdirSync(path.join(dir, relative), { withFileTypes: true })) {
    const child = relative ? `${relative}/${entry.name}` : entry.name;
    if (entry.isDirectory()) files.push(...listFiles(dir, child));
    else files.push(child);
  }
  return files;
}

function countLiteral(text, literal) {
  let count = 0;
  for (let index = text.indexOf(literal); index !== -1; index = text.indexOf(literal, index + 1))
    count++;
  return count;
}

function isSafeRelativePath(value) {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.length <= 512 &&
    !value.startsWith('/') &&
    !value.includes('\\') &&
    !value.includes('\0') &&
    value.split('/').every((segment) => segment !== '' && segment !== '.' && segment !== '..')
  );
}

/** Every listed .br/.gz copy matches its manifest row and decompresses to its original. */
function verifyPrecompressed(uiDir, manifest) {
  const mismatch = 'A precompressed dashboard file does not match its build manifest.';
  if (!Array.isArray(manifest.precompressed)) throw new Error(mismatch);
  const listed = new Set();
  for (const entry of manifest.precompressed) {
    const suffix = VARIANT_SUFFIX.get(entry?.encoding);
    if (
      !suffix ||
      !isSafeRelativePath(entry.path) ||
      entry.file !== `${entry.path}${suffix}` ||
      !Number.isSafeInteger(entry.bytes) ||
      entry.bytes < 1 ||
      !SHA256.test(entry.sha256 ?? '') ||
      listed.has(entry.file)
    )
      throw new Error(mismatch);
    listed.add(entry.file);
    const variant = fs.readFileSync(path.join(uiDir, entry.file));
    const original = fs.readFileSync(path.join(uiDir, entry.path));
    const decoded =
      entry.encoding === 'br' ? zlib.brotliDecompressSync(variant) : zlib.gunzipSync(variant);
    if (
      variant.length !== entry.bytes ||
      sha256(variant) !== entry.sha256 ||
      variant.length >= original.length ||
      !decoded.equals(original)
    )
      throw new Error(mismatch);
  }
  for (const file of listFiles(uiDir)) {
    if (isVariantFile(file) && !listed.has(file)) throw new Error(mismatch);
  }
}

/** The installable shell: a stamped worker, a valid manifest and every icon it names. */
function verifyPwaShell(uiDir, buildId) {
  for (const file of ['sw.js', 'sw-route.js']) {
    if (!fs.existsSync(path.join(uiDir, file))) throw new Error(`Missing browser asset: ${file}.`);
  }
  const sw = fs.readFileSync(path.join(uiDir, 'sw.js'), 'utf8');
  if (countLiteral(sw, `'${buildId}'`) !== 1 || sw.includes('__AAC_BUILD_ID__')) {
    throw new Error('The packaged sw.js must carry the stamped build id.');
  }
  let webmanifest;
  try {
    webmanifest = JSON.parse(fs.readFileSync(path.join(uiDir, 'manifest.webmanifest'), 'utf8'));
  } catch {
    throw new Error('The packaged manifest.webmanifest is not installable.');
  }
  const icons = webmanifest?.icons;
  if (
    webmanifest?.name !== 'AI Account Center' ||
    webmanifest?.short_name !== 'AAC' ||
    webmanifest?.start_url !== '/' ||
    webmanifest?.display !== 'standalone' ||
    !Array.isArray(icons) ||
    icons.length < 1
  ) {
    throw new Error('The packaged manifest.webmanifest is not installable.');
  }
  for (const icon of icons) {
    const relative = typeof icon?.src === 'string' ? icon.src.replace(/^\/+/, '') : '';
    if (!icon?.sizes || !isSafeRelativePath(relative)) {
      throw new Error('The packaged manifest.webmanifest names an icon that is not served.');
    }
    if (!fs.existsSync(path.join(uiDir, relative))) {
      throw new Error(`Missing browser asset: ${icon.src}.`);
    }
  }
}

/** Packaged folders are 0755 and files 0644, whatever the builder's umask was. */
function verifyModes(dir) {
  if (process.platform === 'win32') return;
  const message = 'Packaged dashboard folders must be 0755 and files 0644.';
  if ((fs.statSync(dir).mode & 0o777) !== 0o755) throw new Error(message);
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) verifyModes(entryPath);
    else if ((fs.lstatSync(entryPath).mode & 0o777) !== 0o644) throw new Error(message);
  }
}

/** Verify the pinned browser runtime and the exact binary that will be packaged. */
function verifyBundle(uiDir = UI_DIR) {
  const manifest = JSON.parse(fs.readFileSync(path.join(uiDir, 'ui-build-manifest.json'), 'utf8'));
  const buildId = manifest.buildId;
  if (
    manifest.schemaVersion !== 1 ||
    manifest.framework !== 'slint' ||
    manifest.version !== '1.18.1' ||
    manifest.target !== 'wasm32-unknown-unknown' ||
    manifest.entry !== 'index.html' ||
    !/^[a-f0-9]{64}$/.test(manifest.source?.sha256 ?? '') ||
    !Number.isSafeInteger(manifest.source?.files) ||
    manifest.source.files < 1 ||
    typeof buildId !== 'string' ||
    !BUILD_ID.test(buildId) ||
    manifest.wasm?.path !== `pkg/${buildId}/${WASM_NAME}_bg.wasm`
  ) {
    throw new Error('The packaged dashboard must be Slint 1.18.1 for WebAssembly.');
  }
  for (const file of ['index.html', 'bridge.js', `pkg/${buildId}/${WASM_NAME}.js`]) {
    if (!fs.existsSync(path.join(uiDir, file))) throw new Error(`Missing browser asset: ${file}.`);
  }
  for (const file of [`pkg/${WASM_NAME}.js`, `pkg/${WASM_NAME}_bg.wasm`]) {
    if (fs.existsSync(path.join(uiDir, file)))
      throw new Error('The packaged dashboard must not keep an unversioned WebAssembly runtime.');
  }
  const wasm = fs.readFileSync(path.join(uiDir, manifest.wasm.path));
  const hash = sha256(wasm);
  if (
    !wasm.subarray(0, 8).equals(Buffer.from([0, 97, 115, 109, 1, 0, 0, 0])) ||
    manifest.wasm.bytes !== wasm.length ||
    manifest.wasm.sha256 !== hash ||
    hash.slice(0, 12) !== buildId
  ) {
    throw new Error('The packaged WebAssembly binary does not match its build manifest.');
  }
  const bridge = fs.readFileSync(path.join(uiDir, 'bridge.js'), 'utf8');
  if (
    countLiteral(bridge, `'./pkg/${buildId}/${WASM_NAME}.js'`) !== 1 ||
    countLiteral(bridge, `'./pkg/${WASM_NAME}.js'`) !== 0
  ) {
    throw new Error('The packaged bridge.js must import the versioned WebAssembly runtime.');
  }
  verifyPwaShell(uiDir, buildId);
  verifyPrecompressed(uiDir, manifest);
  verifyModes(uiDir);
  const totalSize = walkDir(uiDir);
  if (totalSize > MAX_SIZE)
    throw new Error(
      `Bundle too large: ${(totalSize / 1024).toFixed(1)}KB gzipped (max: ${MAX_SIZE / 1024}KB).`
    );
  return { totalSize, manifest };
}

const CODEX_RUNTIME_FILE = path.join(__dirname, '../dist/app-updates/app_update_codex_runtime.cjs');
const CODEX_RUNTIME_EXPORTS = ['createCodexActivationRuntime', 'lockfile'];
// The only first-party modules the runtime may carry. Account activation
// (activate-codex-profile, auth.json replacement) is not among them.
const CODEX_RUNTIME_SOURCES = new Set([
  'src/codex-auth/codex-update-runtime-entry.ts',
  'src/codex-auth/codex-activation-runtime.ts',
  'src/codex-auth/codex-activation-confirmation.ts',
  'src/utils/app-launcher.ts',
]);
const CODEX_RUNTIME_PACKAGES = new Set([
  'ws',
  'proper-lockfile',
  'graceful-fs',
  'retry',
  'signal-exit',
]);
const CODEX_RUNTIME_FORBIDDEN = ['activate-codex-profile', 'activateCodexProfile', 'auth.json'];

/**
 * The Codex stop/start runtime shipped with the update helpers: self-contained,
 * exporting exactly { createCodexActivationRuntime, lockfile }, with no
 * account activation code and no path of the checkout that built it.
 */
function verifyCodexUpdateRuntime(file = CODEX_RUNTIME_FILE) {
  if (!fs.existsSync(file)) {
    throw new Error('Missing the Codex update runtime; run bun run build:server.');
  }
  const text = fs.readFileSync(file, 'utf8');
  const fail = (reason) => {
    throw new Error(`The Codex update runtime ${reason}.`);
  };
  // Bun marks each bundled module with a "// <path>" line.
  const modules = [...text.matchAll(/^\/\/ ((?:src|node_modules)\/\S+)$/gm)].map(
    (match) => match[1]
  );
  const sources = new Set(modules.filter((module) => module.startsWith('src/')));
  if (
    !sources.has('src/codex-auth/codex-update-runtime-entry.ts') ||
    !sources.has('src/codex-auth/codex-activation-runtime.ts')
  )
    fail('does not list its bundled modules');
  for (const source of sources) {
    if (!CODEX_RUNTIME_SOURCES.has(source)) fail(`bundles an unexpected module (${source})`);
  }
  for (const module of modules.filter((value) => value.startsWith('node_modules/'))) {
    const parts = module.split('/');
    const name = parts[1].startsWith('@') ? `${parts[1]}/${parts[2]}` : parts[1];
    if (!CODEX_RUNTIME_PACKAGES.has(name)) fail(`bundles an unexpected package (${name})`);
  }
  for (const marker of CODEX_RUNTIME_FORBIDDEN) {
    if (text.includes(marker)) fail('contains account activation code');
  }
  if (text.includes(path.resolve(__dirname, '..'))) fail('embeds the path of its build checkout');
  // Node core modules only: Bun's list adds its own and the npm names it serves.
  const builtins = new Set(
    require('module').builtinModules.filter((name) => !/^(?:bun(?::.*)?|ws|undici)$/.test(name))
  );
  for (const [, , name] of text.matchAll(/\brequire\((["'])([^"']+)\1\)/g)) {
    if (!builtins.has(name.replace(/^node:/, ''))) fail(`requires an external module (${name})`);
  }
  delete require.cache[require.resolve(file)];
  const loaded = require(file);
  if (
    JSON.stringify(Object.keys(loaded).sort()) !== JSON.stringify(CODEX_RUNTIME_EXPORTS) ||
    typeof loaded.createCodexActivationRuntime !== 'function' ||
    typeof loaded.lockfile?.lock !== 'function' ||
    typeof loaded.lockfile?.unlock !== 'function'
  )
    fail('must export exactly createCodexActivationRuntime and lockfile');
  return { bytes: Buffer.byteLength(text), modules: new Set(modules).size };
}

if (require.main === module) {
  try {
    const result = verifyBundle();
    console.log(
      `[OK] Slint ${result.manifest.version} bundle: ${(result.totalSize / 1024).toFixed(1)}KB gzipped.`
    );
  } catch (error) {
    console.error(
      `[X] ${error instanceof Error ? error.message : 'Dashboard bundle verification failed.'}`
    );
    process.exitCode = 1;
  }
  try {
    const result = verifyCodexUpdateRuntime();
    console.log(
      `[OK] Codex update runtime: ${(result.bytes / 1024).toFixed(1)}KB, ${result.modules} modules, no account activation code.`
    );
  } catch (error) {
    console.error(
      `[X] ${error instanceof Error ? error.message : 'Codex update runtime verification failed.'}`
    );
    process.exitCode = 1;
  }
}

module.exports = { CODEX_RUNTIME_FILE, verifyBundle, verifyCodexUpdateRuntime };
