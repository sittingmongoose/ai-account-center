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

if (require.main === module) {
  if (!checkInstalledLineage()) {
    process.exitCode = 1;
  } else {
    try {
      initializeAccountDirectory();
      console.log('[OK] AI Account Center storage is ready.');
    } catch (error) {
      console.error(`[X] AI Account Center storage could not be prepared: ${error.message}`);
      process.exitCode = 1;
    }
  }
}

module.exports = { initializeAccountDirectory, checkInstalledLineage };
