import { describe, expect, it } from 'bun:test';
import path from 'path';

const guard = require(path.resolve(import.meta.dir, '../../../scripts/personal-detail-guard.js'));

// Synthetic blocked values only: the real list lives in the guard as hashes.
const blocked = {
  words: new Set([guard.sha256('zebrafox'), guard.sha256('quokka')]),
  ipv4: new Set([guard.sha256('198.51.77')]),
};

describe('personal detail guard', () => {
  it('finds blocked words in any case and inside identifiers', () => {
    const text = [
      'profile: ZebraFox',
      'host = "quokka-mac"',
      'const quokkaProfile = 1;',
      'user zebrafox2@example.com',
      'zebrafoxes and notquokka are different words',
    ].join('\n');
    expect(guard.scanText(text, blocked).map((hit: { line: number }) => hit.line)).toEqual([
      1, 2, 3, 4,
    ]);
  });

  it('finds blocked /24 prefixes in dotted and IPv4-mapped hex form', () => {
    const text = [
      'peer 198.51.77.20',
      'trust 198.51.77.0/24',
      'mapped ::ffff:c633:4d14',
      'other 198.51.78.20 and 10.198.51.77',
    ].join('\n');
    expect(guard.scanText(text, blocked).map((hit: { line: number }) => hit.line)).toEqual([
      1, 2, 3,
    ]);
  });

  it('keeps the shipped list free of plain-text values', () => {
    const fs = require('fs');
    const source = fs.readFileSync(
      path.resolve(import.meta.dir, '../../../scripts/personal-detail-guard.js'),
      'utf8'
    );
    expect(guard.scanText(source)).toEqual([]);
  });
});
