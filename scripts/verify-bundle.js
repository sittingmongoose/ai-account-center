#!/usr/bin/env node

'use strict';

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const zlib = require('zlib');

const UI_DIR = path.join(__dirname, '../dist/ui');
const MAX_SIZE = 8 * 1024 * 1024;

function walkDir(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).reduce((total, file) => {
    const filePath = path.join(dir, file.name);
    return (
      total +
      (file.isDirectory() ? walkDir(filePath) : zlib.gzipSync(fs.readFileSync(filePath)).length)
    );
  }, 0);
}

/** Verify the pinned browser runtime and the exact binary that will be packaged. */
function verifyBundle(uiDir = UI_DIR) {
  const manifest = JSON.parse(fs.readFileSync(path.join(uiDir, 'ui-build-manifest.json'), 'utf8'));
  if (
    manifest.schemaVersion !== 1 ||
    manifest.framework !== 'slint' ||
    manifest.version !== '1.18.1' ||
    manifest.target !== 'wasm32-unknown-unknown' ||
    manifest.entry !== 'index.html' ||
    !/^[a-f0-9]{64}$/.test(manifest.source?.sha256 ?? '') ||
    !Number.isSafeInteger(manifest.source?.files) ||
    manifest.source.files < 1 ||
    manifest.wasm?.path !== 'pkg/ccs_account_dashboard_bg.wasm'
  ) {
    throw new Error('The packaged dashboard must be Slint 1.18.1 for WebAssembly.');
  }
  for (const file of ['index.html', 'bridge.js', 'pkg/ccs_account_dashboard.js']) {
    if (!fs.existsSync(path.join(uiDir, file))) throw new Error(`Missing browser asset: ${file}.`);
  }
  const wasm = fs.readFileSync(path.join(uiDir, manifest.wasm.path));
  const hash = crypto.createHash('sha256').update(wasm).digest('hex');
  if (
    !wasm.subarray(0, 8).equals(Buffer.from([0, 97, 115, 109, 1, 0, 0, 0])) ||
    manifest.wasm.bytes !== wasm.length ||
    manifest.wasm.sha256 !== hash
  ) {
    throw new Error('The packaged WebAssembly binary does not match its build manifest.');
  }
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
