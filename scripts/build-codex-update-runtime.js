#!/usr/bin/env node
'use strict';

/**
 * Bundles the Codex stop/start runtime that scripts/app-updates/app_update_codex.cjs
 * loads on a Linux host without an AAC install. The helper sync copies the
 * bundle from dist/app-updates into ~/.ccs/app-updates beside the source
 * helpers. Generated code stays in dist/, never in scripts/.
 * Run after: tsc (bun run build:server / bun run build).
 */

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');
const { CODEX_RUNTIME_FILE, verifyCodexUpdateRuntime } = require('./verify-bundle');

const ROOT = path.join(__dirname, '..');
const ENTRY = 'src/codex-auth/codex-update-runtime-entry.ts';

function buildCodexUpdateRuntime(outfile = CODEX_RUNTIME_FILE) {
  const result = spawnSync(
    'bun',
    [
      'build',
      ENTRY,
      '--target=node',
      '--format=cjs',
      // Bun would inline this checkout's source folder. Keep it the bundle's own
      // folder at run time instead: dist/app-updates sits at the same depth as
      // dist/codex-auth, so package-relative paths resolve as in the tsc output.
      '--define',
      '__dirname=module.path',
      `--outfile=${outfile}`,
    ],
    { cwd: ROOT, encoding: 'utf8', shell: process.platform === 'win32' }
  );
  if (result.error || result.status !== 0) {
    process.stderr.write(`${result.stdout ?? ''}${result.stderr ?? ''}`);
    throw new Error('bun build failed for the Codex update runtime.');
  }
  fs.chmodSync(outfile, 0o644);
  return verifyCodexUpdateRuntime(outfile);
}

if (require.main === module) {
  try {
    const { bytes } = buildCodexUpdateRuntime();
    console.log(
      `[OK] Codex update runtime: ${path.relative(ROOT, CODEX_RUNTIME_FILE)} (${(bytes / 1024).toFixed(1)}KB).`
    );
  } catch (error) {
    console.error(
      `[X] ${error instanceof Error ? error.message : 'Codex update runtime build failed.'}`
    );
    process.exitCode = 1;
  }
}

module.exports = { buildCodexUpdateRuntime };
