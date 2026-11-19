#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { stageManifest } = require('./build-dashboard-native');
const { recordBuild } = require('./deploy-lineage');

const root = path.resolve(__dirname, '..');

function stagePackage(destination) {
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
        if (relative === 'dist/ccs.js' || relative === 'dist/bin') return false;
        const match =
          /^dist\/native\/(?:aac-http|ai-account-center|ccs|ccs-droid|ccsd|ccs-codex|ccsx|ccsxp)(\.exe)?$/.exec(
            relative
          );
        return !match || Boolean(match[1]) === (process.platform === 'win32');
      },
    });
  }
  stageManifest(resolved);
  recordBuild(root, path.join(resolved, 'dist'));
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
