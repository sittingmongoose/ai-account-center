/**
 * Builds the real Codex stop/start runtime into a temporary folder (it spawns
 * bun, so this file runs in the slow bucket) and loads it the way a remote
 * Linux host does: flat, beside app_update_codex.cjs, without AAC installed.
 * Never starts, stops or signals a Codex process.
 */
import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';

const { buildCodexUpdateRuntime } = require('../../../scripts/build-codex-update-runtime.js');
const BRIDGE = path.resolve(__dirname, '../../../scripts/app-updates/app_update_codex.cjs');
let folder = '';
let bundle = '';
let built: { bytes: number; modules: number };

beforeAll(() => {
  folder = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-codex-runtime-bundle-'));
  bundle = path.join(folder, 'app_update_codex_runtime.cjs');
  built = buildCodexUpdateRuntime(bundle);
  fs.copyFileSync(BRIDGE, path.join(folder, 'app_update_codex.cjs'));
});
afterAll(() => fs.rmSync(folder, { recursive: true, force: true }));

describe('Codex update runtime bundle', () => {
  it('passes the packaging check and carries no account activation code', () => {
    expect(built.bytes).toBeGreaterThan(10_000);
    const text = fs.readFileSync(bundle, 'utf8');
    expect(text).not.toContain('activate-codex-profile');
    expect(text).not.toContain('auth.json');
    expect(text).not.toContain(path.resolve(__dirname, '../../..'));
  });

  it('exports exactly the stop/start runtime and the lock library', () => {
    const loaded = require(bundle);
    expect(Object.keys(loaded).sort()).toEqual(['createCodexActivationRuntime', 'lockfile']);
    expect(typeof loaded.lockfile.lock).toBe('function');
    const runtime = loaded.createCodexActivationRuntime(path.join(folder, 'codex-home'));
    expect(Object.keys(runtime).sort()).toEqual(['dispose', 'start', 'stop']);
  });

  it('is what the bridge loads from a flat helper folder, with its own lock library', async () => {
    const bridge = require(path.join(folder, 'app_update_codex.cjs'));
    const loaded = bridge.loadRuntime();
    expect(loaded.selected).toBe(bundle);
    const home = path.join(folder, 'codex-lock');
    fs.mkdirSync(home);
    const release = await loaded.lockfile.lock(home, {
      realpath: false,
      lockfilePath: path.join(home, '.ccs-activation.lock'),
    });
    expect(fs.existsSync(path.join(home, '.ccs-activation.lock'))).toBe(true);
    await release();
    expect(fs.existsSync(path.join(home, '.ccs-activation.lock'))).toBe(false);
  });
});
