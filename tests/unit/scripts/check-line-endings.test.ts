import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';

const { crlfFiles } = require('../../../scripts/check-line-endings.js') as {
  crlfFiles: (root: string) => string[];
};

let root = '';
beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-line-endings-'));
});
afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

function write(relative: string, text: string): void {
  const file = path.join(root, relative);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

describe('packaged line endings', () => {
  it('finds CRLF helpers and leaves LF files, Windows wrappers and dependencies alone', () => {
    write('scripts/account-usage/plan_usage.py', 'a = 1\r\nb = 2\r\n');
    write('scripts/agent-usage/aac-usage-native.sh', '#!/bin/sh\r\n');
    write('scripts/agent-usage/aac-usage.cmd', '@echo off\r\n');
    write('browser-bridge/opencode-muse/native-host/host.py', 'print(1)\n');
    write('browser-bridge/qwen/native-host/node_modules/dep/index.js', 'x\r\n');
    write('browser-bridge/qwen/extension/popup.mjs', 'export {};\r\n');
    expect(crlfFiles(root)).toEqual([
      'browser-bridge/qwen/extension/popup.mjs',
      'scripts/agent-usage/aac-usage-native.sh',
      'scripts/account-usage/plan_usage.py',
    ]);
  });

  it('passes this checkout', () => {
    expect(crlfFiles(path.resolve(__dirname, '../../..'))).toEqual([]);
  });
});
