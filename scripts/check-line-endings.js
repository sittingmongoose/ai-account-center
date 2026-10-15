#!/usr/bin/env node
/**
 * Refuses to package helpers with CRLF line endings. The package runs scripts on the Mac, Windows,
 * Nas1 and the VM and pins several of them by SHA-256 (the analytics helper, the Claude history
 * writer, the Antigravity runtime manifest), so a package built from a checkout that converted
 * line endings broke them silently (2026-10-13). `.gitattributes` keeps new checkouts LF; this
 * catches an older working tree. Windows `.cmd` wrappers keep CRLF on purpose.
 *
 * Usage: node scripts/check-line-endings.js [root]   (exit 1 lists the files to re-check out)
 */
'use strict';

const fs = require('fs');
const path = require('path');

const FOLDERS = ['scripts', 'browser-bridge', 'lib', 'config'];
const SKIP_DIRECTORIES = new Set(['node_modules', '__pycache__', '.git', 'target', 'bin', 'obj']);
const TEXT = /\.(py|js|cjs|mjs|ts|json|md|sh|ps1|cs|txt|toml|yaml|yml|html|css|svg)$|^[^.]+$/;
const KEEP_CRLF = /\.cmd$/i;

function walk(directory, found) {
  let entries;
  try {
    entries = fs.readdirSync(directory, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const full = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      if (!SKIP_DIRECTORIES.has(entry.name)) walk(full, found);
    } else if (entry.isFile() && TEXT.test(entry.name) && !KEEP_CRLF.test(entry.name)) {
      const bytes = fs.readFileSync(full);
      if (bytes.length <= 16 * 1024 * 1024 && bytes.includes(13) && bytes.includes('\r\n'))
        found.push(full);
    }
  }
}

function crlfFiles(root) {
  const found = [];
  for (const folder of FOLDERS) walk(path.join(root, folder), found);
  return found.map((file) => path.relative(root, file).split(path.sep).join('/')).sort();
}

if (require.main === module) {
  const root = path.resolve(process.argv[2] || path.join(__dirname, '..'));
  const found = crlfFiles(root);
  if (found.length > 0) {
    process.stderr.write(
      `[X] ${found.length} packaged file(s) have CRLF line endings, which break the helpers' SHA-256 pins:\n` +
        found
          .slice(0, 20)
          .map((file) => `    ${file}\n`)
          .join('') +
        (found.length > 20 ? `    ... and ${found.length - 20} more\n` : '') +
        '    Re-check out with LF (.gitattributes): git rm -r -q --cached . && git reset -q --hard\n'
    );
    process.exit(1);
  }
  process.stdout.write('[OK] Packaged scripts use LF line endings.\n');
}

module.exports = { crlfFiles };
