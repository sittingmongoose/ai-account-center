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
  verifyPrecompressed(uiDir, manifest);
  verifyModes(uiDir);
  const totalSize = walkDir(uiDir);
  if (totalSize > MAX_SIZE)
    throw new Error(
      `Bundle too large: ${(totalSize / 1024).toFixed(1)}KB gzipped (max: ${MAX_SIZE / 1024}KB).`
    );
  return { totalSize, manifest };
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
}

module.exports = { verifyBundle };
