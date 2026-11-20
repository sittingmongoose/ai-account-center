#!/usr/bin/env node
'use strict';

const path = require('node:path');
const { spawn } = require('node:child_process');

const binary = path.join(
  __dirname,
  '..',
  'dist',
  'native',
  process.platform === 'win32' ? 'ai-account-center.exe' : 'ai-account-center'
);
const child = spawn(binary, ['dashboard', ...process.argv.slice(2)], { stdio: 'inherit' });
child.once('error', (error) => {
  console.error('[X] ' + error.message);
  process.exitCode = 1;
});
child.once('exit', (code) => {
  process.exitCode = code ?? 1;
});
