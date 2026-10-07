#!/usr/bin/env node
'use strict';

/**
 * Fails when a known personal detail (an owner's account name, computer name,
 * email local part or home-network prefix) appears in a tracked text file.
 *
 * The blocked values are never written here in plain text: the guard keeps the
 * SHA-256 of each lowercased word and of each blocked IPv4 /24 prefix
 * ("a.b.c"), and hashes every candidate it reads. Words are split on anything
 * that is not a letter or digit, on lower-to-upper case changes and on
 * letter/digit changes, so "workMac2" yields "workmac2", "work", "mac" and "2".
 *
 * To block another value, add sha256(lowercased value) to BLOCKED_WORDS or
 * BLOCKED_IPV4_PREFIXES. Product code that must keep a value (its behaviour
 * depends on it) is listed in ALLOWED with the exact number of occurrences, so
 * any new occurrence still fails.
 *
 * Usage: node scripts/personal-detail-guard.js [--root <dir>]
 */

const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const BLOCKED_WORDS = new Set([
  '0fbcceaae1977bab1d7be30549247fecd78a75b3574d015eb097bcce71fd9f08',
  '237e1cae99b6c84038bd60299fc641a6859e603f3a7f7729042a8a5786d5bfe7',
  '27d300fe53b3b94f115cfd63be02d868bcb8f755e56893709418084c1bfab1cd',
  '8137bce2aa40deb8ef397346bb798ac40b16f98138b1e9a0205860653e7980ba',
  '49449cc1bb8fdaff47febe7ad8762087e7feaccbc5321935f39204ba1c496db8',
]);

const BLOCKED_IPV4_PREFIXES = new Set([
  '4946358a3a79887b1390685855327cd17e56a9eab6979d852830aa1b678e8439',
]);

/** path -> { hash -> exact allowed occurrences } (product code, kept on purpose). */
const ALLOWED = {
  'scripts/claude-history/history-index-sync.cjs': {
    '0fbcceaae1977bab1d7be30549247fecd78a75b3574d015eb097bcce71fd9f08': 3,
  },
  'scripts/claude-history/history_index_node_bridge_v2.cjs': {
    '0fbcceaae1977bab1d7be30549247fecd78a75b3574d015eb097bcce71fd9f08': 1,
  },
  'scripts/claude-history/history_index_remote_v2.py': {
    '0fbcceaae1977bab1d7be30549247fecd78a75b3574d015eb097bcce71fd9f08': 5,
  },
  'src/web-server/services/app-update-hosts.ts': {
    '27d300fe53b3b94f115cfd63be02d868bcb8f755e56893709418084c1bfab1cd': 2,
  },
};

const MAX_FILE_BYTES = 8 * 1024 * 1024;
const sha256 = (value) => crypto.createHash('sha256').update(value).digest('hex');

function splitWords(line) {
  const words = new Set();
  for (const raw of line.split(/[^A-Za-z0-9]+/)) {
    if (!raw) continue;
    words.add(raw.toLowerCase());
    const parts = raw
      .replace(/([a-z])([A-Z])/g, '$1 $2')
      .replace(/([A-Za-z])([0-9])/g, '$1 $2')
      .replace(/([0-9])([A-Za-z])/g, '$1 $2')
      .split(' ');
    if (parts.length > 1) for (const part of parts) words.add(part.toLowerCase());
  }
  return words;
}

function ipv4Prefixes(line) {
  const prefixes = [];
  for (const match of line.matchAll(/(?<![0-9.])(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.\d{1,3}/g)) {
    prefixes.push(`${Number(match[1])}.${Number(match[2])}.${Number(match[3])}`);
  }
  // IPv4-mapped IPv6 in hex form (::ffff:c0a8:0a14 is 192.168.10.20).
  for (const match of line.matchAll(/::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})\b/gi)) {
    const high = parseInt(match[1], 16);
    const low = parseInt(match[2], 16);
    prefixes.push(`${high >> 8}.${high & 0xff}.${low >> 8}`);
  }
  return prefixes;
}

/**
 * Returns [{ line, hash, kind }] for every blocked value in `text`.
 * `blocked` lets tests pass their own hash sets.
 */
function scanText(
  text,
  blocked = { words: BLOCKED_WORDS, ipv4: BLOCKED_IPV4_PREFIXES },
  cache = new Map()
) {
  const hits = [];
  const lookup = (value, set) => {
    const key = `${set === blocked.words ? 'w' : 'i'}:${value}`;
    let hash = cache.get(key);
    if (hash === undefined) {
      hash = sha256(value);
      cache.set(key, hash);
    }
    return set.has(hash) ? hash : null;
  };
  const lines = text.split('\n');
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    for (const word of splitWords(line)) {
      const hash = lookup(word, blocked.words);
      if (hash) hits.push({ line: index + 1, hash, kind: 'word' });
    }
    if (line.includes('.') || line.includes(':')) {
      for (const prefix of ipv4Prefixes(line)) {
        const hash = lookup(prefix, blocked.ipv4);
        if (hash) hits.push({ line: index + 1, hash, kind: 'ipv4' });
      }
    }
  }
  return hits;
}

function trackedFiles(root) {
  return execFileSync('git', ['ls-files', '-z'], { cwd: root, maxBuffer: 64 * 1024 * 1024 })
    .toString('utf8')
    .split('\0')
    .filter((file) => file && !file.split('/').includes('node_modules'));
}

function isBinary(buffer) {
  return buffer.subarray(0, Math.min(buffer.length, 8000)).includes(0);
}

function scanRepository(root) {
  const cache = new Map();
  const problems = [];
  for (const file of trackedFiles(root)) {
    let buffer;
    try {
      const stat = fs.lstatSync(path.join(root, file));
      if (!stat.isFile() || stat.size > MAX_FILE_BYTES) continue;
      buffer = fs.readFileSync(path.join(root, file));
    } catch {
      continue; // deleted in the working tree
    }
    if (isBinary(buffer)) continue;
    const hits = scanText(buffer.toString('utf8'), undefined, cache);
    if (hits.length === 0) continue;
    const allowed = ALLOWED[file] || {};
    const counts = new Map();
    for (const hit of hits) counts.set(hit.hash, (counts.get(hit.hash) || 0) + 1);
    for (const [hash, count] of counts) {
      if (count <= (allowed[hash] || 0)) continue;
      const lines = hits.filter((hit) => hit.hash === hash).map((hit) => hit.line);
      problems.push({ file, kind: hits.find((hit) => hit.hash === hash).kind, lines, count });
    }
  }
  return problems;
}

function main() {
  const rootIndex = process.argv.indexOf('--root');
  const root =
    rootIndex > 0 ? path.resolve(process.argv[rootIndex + 1]) : path.resolve(__dirname, '..');
  const problems = scanRepository(root);
  if (problems.length === 0) {
    console.log('[OK] No blocked personal details in tracked files.');
    return;
  }
  console.error('[X] Blocked personal details found in tracked files.');
  console.error('    Replace them with neutral placeholders (see docs/code-standards.md).');
  for (const problem of problems) {
    console.error(
      `    ${problem.file}: ${problem.kind} on line(s) ${problem.lines.slice(0, 10).join(', ')}`
    );
  }
  process.exit(1);
}

if (require.main === module) main();

module.exports = { scanText, splitWords, ipv4Prefixes, scanRepository, sha256 };
