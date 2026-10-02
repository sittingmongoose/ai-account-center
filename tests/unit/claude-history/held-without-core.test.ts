import { afterEach, expect, test } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { claudeHistoryOpenHeldWithoutCore } from '../../../src/web-server/services/claude-history-sync-service';

const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});
function ccsDir(markers?: string[]): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-held-'));
  directories.push(root);
  if (markers) {
    fs.mkdirSync(path.join(root, 'claude-history-pending'), { mode: 0o700 });
    for (const name of markers)
      fs.writeFileSync(path.join(root, 'claude-history-pending', name), '{}');
  }
  return root;
}
const nonce = 'a'.repeat(32);

test('without the copy core, no marker folder holds nothing', () => {
  expect(claudeHistoryOpenHeldWithoutCore(ccsDir(), 'gmail', 'mac')).toBe(false);
});

test('without the copy core, only the profile and platform a marker names are held', () => {
  const root = ccsDir([`gmail-windows-${nonce}.json`]);
  expect(claudeHistoryOpenHeldWithoutCore(root, 'gmail', 'windows')).toBe(true);
  expect(claudeHistoryOpenHeldWithoutCore(root, 'gmail', 'mac')).toBe(false);
  // Launchers outside the history set keep their ordinary Open.
  expect(claudeHistoryOpenHeldWithoutCore(root, 'work-launcher', 'windows')).toBe(false);
  // A prefix of another profile's name is not that profile.
  expect(
    claudeHistoryOpenHeldWithoutCore(ccsDir([`me-x-windows-${nonce}.json`]), 'me', 'windows')
  ).toBe(false);
});

test('without the copy core, an unlistable marker folder or an unsafe id holds', () => {
  const root = ccsDir();
  fs.writeFileSync(path.join(root, 'claude-history-pending'), 'not a folder');
  expect(claudeHistoryOpenHeldWithoutCore(root, 'gmail', 'mac')).toBe(true);
  expect(claudeHistoryOpenHeldWithoutCore(ccsDir(), '../gmail', 'mac')).toBe(true);
});
