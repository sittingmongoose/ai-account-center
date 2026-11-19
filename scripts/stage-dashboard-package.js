#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { stageManifest } = require('./build-dashboard-native');
const { readSourceBuild, recordBuild } = require('./deploy-lineage');

const root = path.resolve(__dirname, '..');

function stagePackage(destination) {
  const source = readSourceBuild(root);
  if (source) {
    const binary = path.join(
      root,
      'dist/native',
      process.platform === 'win32' ? 'aac-http.exe' : 'aac-http'
    );
    const commit = execFileSync(binary, ['--build-commit'], {
      encoding: 'utf8',
      timeout: 5000,
      windowsHide: true,
    }).trim();
    if (commit !== source.commit) {
      throw new Error(
        'The native dashboard was built from another commit. Rebuild before staging.'
      );
    }
  }
  const directory = path.resolve(destination);
  const parent = fs.realpathSync(path.dirname(directory));
  const resolved = path.join(parent, path.basename(directory));
  const relative = path.relative(fs.realpathSync(root), resolved);
  if (!relative || (!relative.startsWith('..' + path.sep) && !path.isAbsolute(relative))) {
    throw new Error('Use a new package staging directory outside the source checkout.');
  }
  fs.mkdirSync(resolved);
  const manifest = require('../package.json');
  for (const item of new Set(['package.json', ...manifest.files])) {
    const from = path.join(root, item);
    if (!fs.existsSync(from)) continue;
    fs.cpSync(from, path.join(resolved, item), {
      recursive: true,
      dereference: true,
      filter: (file) => {
        const relative = path.relative(root, file).split(path.sep).join('/');
        if (/^dist\/ccs(?:\.js|\.d\.ts)(?:\.map)?$/.test(relative) || relative === 'dist/bin')
          return false;
        const match =
          /^dist\/native\/(?:aac-http|ai-account-center|ccs|ccs-droid|ccsd|ccs-codex|ccsx|ccsxp)(\.exe)?$/.exec(
            relative
          );
        if (match) return Boolean(match[1]) === (process.platform === 'win32');
        // The compiler can leave output for deleted sources in an incremental build.
        const emitted =
          /^dist\/(?!ui\/|native\/|app-updates\/)(.+?)(?:\.js|\.d\.ts)(?:\.map)?$/.exec(relative);
        return !emitted || fs.existsSync(path.join(root, 'src', emitted[1] + '.ts'));
      },
    });
  }
  stageManifest(resolved);
  const metadata = recordBuild(root, path.join(resolved, 'dist'));
  if (metadata.commit !== (source?.commit ?? null)) {
    throw new Error('The source checkout changed while the package was staged.');
  }
  return resolved;
}

if (require.main === module) {
  if (process.argv.length !== 3) {
    console.error('Usage: node scripts/stage-dashboard-package.js <new-package-directory>');
    process.exitCode = 1;
  } else {
    console.log(stagePackage(process.argv[2]));
  }
}
