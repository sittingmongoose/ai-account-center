#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const root = path.resolve(__dirname, '..');
const tools = ['aac-http', 'ai-account-center', 'ccs'];
const retired = ['ccs-droid', 'ccsd', 'ccs-codex', 'ccsx', 'ccsxp'];
const extension = process.platform === 'win32' ? '.exe' : '';

function stageDashboard(destination, artifacts) {
  fs.mkdirSync(destination, { recursive: true });
  for (const name of [...tools, ...retired]) {
    const original = retired.includes(name) ? 'ccs' : name;
    const target = path.join(destination, name + extension);
    fs.copyFileSync(path.join(artifacts, original + extension), target);
    if (process.platform !== 'win32') fs.chmodSync(target, 0o755);
  }
}

function stageManifest(packageRoot) {
  const manifestPath = path.join(packageRoot, 'package.json');
  if (
    fs.existsSync(path.join(packageRoot, '.git')) ||
    fs.existsSync(path.join(packageRoot, 'src'))
  ) {
    throw new Error('Platform metadata must be written in a package staging directory.');
  }
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  for (const name of [...tools.slice(1), ...retired]) {
    const bin = 'dist/native/' + name + extension;
    if (!fs.statSync(path.join(packageRoot, bin)).isFile()) throw new Error('Missing ' + bin);
    manifest.bin[name] = bin;
  }
  fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');
}

if (require.main === module) {
  const [option, value] = process.argv.slice(2);
  if (option === '--manifest' && value) {
    stageManifest(path.resolve(value));
  } else {
    const target = path.resolve(root, process.env.CARGO_TARGET_DIR || 'target');
    if (option !== '--copy-only') {
      execFileSync(
        'cargo',
        [
          'build',
          '--release',
          '--locked',
          '-j',
          process.env.CARGO_BUILD_JOBS || '2',
          '-p',
          'aac-http',
          '--bins',
        ],
        { cwd: root, stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: target } }
      );
    }
    stageDashboard(path.join(root, 'dist/native'), path.join(target, 'release'));
  }
}

module.exports = { stageDashboard, stageManifest };
