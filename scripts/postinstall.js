#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');

/** Installation prepares private storage; account imports remain explicit CLI actions. */
function initializeAccountDirectory(env = process.env) {
  const directory = env.CCS_DIR
    ? path.resolve(env.CCS_DIR)
    : path.join(path.resolve(env.CCS_HOME || os.homedir()), '.ccs');

  // Preserve existing configuration, permissions, symlinks and authentication.
  // The current config loader supplies defaults until a setting is saved.
  try {
    if (!fs.statSync(directory).isDirectory()) {
      throw new Error('Account configuration path is not a directory.');
    }
    return directory;
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }

  // Never repair or replace a dangling link during installation.
  try {
    fs.lstatSync(directory);
    throw new Error('Account configuration path is unavailable.');
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }

  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  return directory;
}

/**
 * Refuse a package that would remove the build this computer's dashboard runs. The failed
 * lifecycle script makes npm keep the previous installation (see deploy-lineage.js).
 */
function checkInstalledLineage(packageRoot = path.resolve(__dirname, '..'), env = process.env) {
  const { checkDeployLineage, isSourceCheckout } = require('./deploy-lineage');
  if (isSourceCheckout(packageRoot)) return true;
  const result = checkDeployLineage({ packageRoot, env });
  if (result.message) {
    console[result.ok ? 'log' : 'error'](`[${result.ok ? '!' : 'X'}] ${result.message}`);
  }
  return result.ok;
}

function checkInstalledDashboard(packageRoot, platform = process.platform) {
  const { isSourceCheckout } = require('./deploy-lineage');
  if (isSourceCheckout(packageRoot)) return;
  const { bin } = JSON.parse(fs.readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  const names = ['ai-account-center', 'ccs', 'ccs-droid', 'ccsd', 'ccs-codex', 'ccsx', 'ccsxp'];
  const suffix = platform === 'win32' ? '.exe' : '';
  for (const name of names) {
    if (bin?.[name] !== 'dist/native/' + name + suffix) {
      throw new Error('Dashboard command metadata is invalid for this platform: ' + name);
    }
  }
  for (const relative of [
    ...Object.values(bin),
    'dist/native/aac-http' + suffix,
    'dist/web-server/sidecar.js',
    'dist/ui/app/index.html',
  ]) {
    const file = fs.statSync(path.join(packageRoot, relative));
    if (!file.isFile() || file.size === 0)
      throw new Error('Incomplete dashboard package: ' + relative);
  }
}

if (require.main === module) {
  if (!checkInstalledLineage()) {
    process.exitCode = 1;
  } else {
    try {
      checkInstalledDashboard(path.resolve(__dirname, '..'));
      initializeAccountDirectory();
      console.log('[OK] AI Account Center storage is ready.');
    } catch (error) {
      console.error(`[X] AI Account Center storage could not be prepared: ${error.message}`);
      process.exitCode = 1;
    }
  }
}

module.exports = { initializeAccountDirectory, checkInstalledLineage, checkInstalledDashboard };
