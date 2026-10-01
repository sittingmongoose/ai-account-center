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

if (require.main === module) {
  try {
    initializeAccountDirectory();
    console.log('[OK] AI Account Center storage is ready.');
  } catch (error) {
    console.error(`[X] AI Account Center storage could not be prepared: ${error.message}`);
    process.exitCode = 1;
  }
}

module.exports = { initializeAccountDirectory };
