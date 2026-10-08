import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';

const { verifyCodexUpdateRuntime } = require('../../../scripts/verify-bundle.js');
const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});

const MODULES = [
  '// node_modules/proper-lockfile/index.js',
  '// node_modules/ws/lib/websocket.js',
  '// src/codex-auth/codex-update-runtime-entry.ts',
  '// src/codex-auth/codex-activation-runtime.ts',
  'var fs = require("fs");',
].join('\n');
const EXPORTS =
  'module.exports = { createCodexActivationRuntime() {}, lockfile: { lock() {}, unlock() {} } };';

/** Writes a runtime-shaped file the way Bun lays out the real bundle. */
function runtime(text: string): string {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-runtime-verify-'));
  directories.push(directory);
  const file = path.join(directory, 'app_update_codex_runtime.cjs');
  fs.writeFileSync(file, `${text}\n`);
  return file;
}

describe('Codex update runtime verification', () => {
  it('accepts the stop/start runtime with exactly its two exports', () => {
    expect(verifyCodexUpdateRuntime(runtime(`${MODULES}\n${EXPORTS}`)).modules).toBe(4);
  });

  it('requires a built runtime', () => {
    expect(() => verifyCodexUpdateRuntime(path.join(os.tmpdir(), 'aac-missing.cjs'))).toThrow(
      'Missing the Codex update runtime'
    );
  });

  it('rejects account activation code', () => {
    for (const text of [
      `${MODULES}\n// src/codex-auth/activate-codex-profile.ts\n${EXPORTS}`,
      `${MODULES}\nfs.writeFileSync(path.join(home, "auth.json"), data);\n${EXPORTS}`,
      `${MODULES}\nfunction activateCodexProfile() {}\n${EXPORTS}`,
    ]) {
      expect(() => verifyCodexUpdateRuntime(runtime(text))).toThrow(/activation|unexpected module/);
    }
  });

  it('rejects an unexpected package, an external require or the build checkout path', () => {
    expect(() =>
      verifyCodexUpdateRuntime(runtime(`${MODULES}\n// node_modules/undici/index.js\n${EXPORTS}`))
    ).toThrow('unexpected package (undici)');
    expect(() =>
      verifyCodexUpdateRuntime(runtime(`${MODULES}\nvar ws = require("ws");\n${EXPORTS}`))
    ).toThrow('external module (ws)');
    const checkout = JSON.stringify(path.resolve(__dirname, '../../..'));
    expect(() =>
      verifyCodexUpdateRuntime(runtime(`${MODULES}\nvar __dirname = ${checkout};\n${EXPORTS}`))
    ).toThrow('build checkout');
  });

  it('rejects other export shapes', () => {
    expect(() =>
      verifyCodexUpdateRuntime(
        runtime(`${MODULES}\nmodule.exports = { createCodexActivationRuntime() {} };`)
      )
    ).toThrow('export exactly');
    expect(() =>
      verifyCodexUpdateRuntime(
        runtime(
          `${MODULES}\nmodule.exports = { createCodexActivationRuntime() {}, lockfile: {}, activate() {} };`
        )
      )
    ).toThrow('export exactly');
  });
});
