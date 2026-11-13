import { describe, expect, it, afterEach } from 'bun:test';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const helperPath = path.resolve(import.meta.dir, '../../shared/fixtures/test-environment.js');
// The helper is CommonJS; load it through createRequire or import default.
// Bun handles CJS interop, so we use require-equivalent via import.
const helper = await import(helperPath);
const mod = helper.default ?? helper;

const trackSet = mod._tracked;
const getShared = mod._getSharedCacheRoot;
const sweep = mod._sweepStaleTestHomes;

async function runChildThatCreatesHome(signal?: string) {
  // Child creates a test home via the helper and prints its path, then waits if signal is requested.
  const script = `
    const m = require(${JSON.stringify(helperPath)});
    const home = m._createIsolatedTestHome();
    process.stdout.write(home + "\\n");
    process.stdout.write("READY\\n");
    // Keep alive until we receive signal or explicit exit
    ${signal ? 'setInterval(() => {}, 50);' : ''}
  `;
  if (!signal) {
    // Normal exit: use spawnSync so we reliably capture stdout even when the child exits immediately.
    const res = spawnSync('node', ['-e', script], {
      encoding: 'utf8',
      env: { ...process.env },
      timeout: 5000,
    });
    if (res.error) throw res.error;
    const home = (res.stdout || '').trim().split('\n')[0];
    return home;
  }
  const child = Bun.spawn(['node', '-e', script], {
    stdout: 'pipe',
    stderr: 'pipe',
    env: { ...process.env },
  });
  // Read until READY
  const reader = child.stdout.getReader();
  let buf = '';
  const decoder = new TextDecoder();
  while (!buf.includes('READY')) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
  }
  const home = buf.trim().split('\n')[0];
  if (signal) {
    // Give child a tick to install handlers
    await new Promise((r) => setTimeout(r, 50));
    child.kill(signal as any);
  }
  await child.exited;
  return home;
}

describe('test-environment cleanup', () => {
  afterEach(() => {
    // Remove any tracked leftovers that manual mkdtemp created during the test.
    // Our wrapper tracks them, so clean explicitly for determinism.
    for (const dir of Array.from(trackSet)) {
      // Only remove dirs under os.tmpdir() that are not the bootstrapped home.
      // We keep the bootstrapped home alive (CCS_TEST_BOOTSTRAP_HOME).
      if (dir === process.env.CCS_TEST_BOOTSTRAP_HOME) continue;
      try {
        fs.rmSync(dir, { recursive: true, force: true });
      } catch {}
      trackSet.delete(dir);
    }
  });

  it('creates a tracked home with a pid marker', () => {
    const home = mod._createIsolatedTestHome('ccs-test-home-marker-');
    expect(fs.existsSync(home)).toBe(true);
    expect(trackSet.has(home)).toBe(true);
    const marker = path.join(home, '.aac-test-pid');
    expect(fs.existsSync(marker)).toBe(true);
    expect(fs.readFileSync(marker, 'utf8').trim()).toBe(String(process.pid));
    const cachePath = path.join(home, '.cache');
    expect(fs.existsSync(cachePath)).toBe(true);
    expect(fs.lstatSync(cachePath).isSymbolicLink()).toBe(false);
    // Cleanup manually for this test
    fs.rmSync(home, { recursive: true, force: true });
    trackSet.delete(home);
  });

  it('points package caches at one shared folder outside temp homes', () => {
    const shared = getShared();
    expect(process.env.BUN_INSTALL_CACHE_DIR).toBe(path.join(shared, 'bun-install'));
    expect(process.env.npm_config_cache).toBe(path.join(shared, 'npm-cache'));
    // XDG_CACHE_HOME stays per-home for isolation (see ensureSharedCache comment).
    expect(process.env.XDG_CACHE_HOME).not.toBe(path.join(shared, 'xdg-cache'));
    // Outside every test home (a CI run may itself use a temp HOME, so /tmp alone proves nothing).
    expect(shared).not.toMatch(/ccs-test(?:-home)?-/);
    // Shared root must contain only package caches, not test homes
    expect(fs.existsSync(shared)).toBe(true);
    expect(
      shared.endsWith(path.join('.cache', 'aac-test-shared-cache')) ||
        shared === process.env.AAC_TEST_SHARED_CACHE
    ).toBe(true);
  });

  it('double cleanup is harmless', () => {
    const env = mod.createTestEnvironment();
    expect(fs.existsSync(env.testHome)).toBe(true);
    env.cleanup();
    expect(fs.existsSync(env.testHome)).toBe(false);
    // Second call must not throw
    env.cleanup();
    expect(fs.existsSync(env.testHome)).toBe(false);
  });

  it('child process that exits normally leaves no folder behind', async () => {
    const home = await runChildThatCreatesHome();
    expect(home).toBeTruthy();
    expect(fs.existsSync(home)).toBe(false);
  });

  it('child killed with SIGINT leaves no folder behind', async () => {
    const home = await runChildThatCreatesHome('SIGINT');
    expect(fs.existsSync(home)).toBe(false);
  });

  it('child killed with SIGTERM leaves no folder behind', async () => {
    const home = await runChildThatCreatesHome('SIGTERM');
    expect(fs.existsSync(home)).toBe(false);
  });

  it('sweep removes only stale folders whose owner is dead', () => {
    const tmp = os.tmpdir();
    const deadPid = 999999;
    const alivePid = process.pid;
    function makeDir(prefix: string, pid: number, ageHours: number) {
      const dir = fs.mkdtempSync(path.join(tmp, prefix));
      // If create went via wrapper, it already has a marker. Overwrite with desired pid.
      fs.writeFileSync(path.join(dir, '.aac-test-pid'), String(pid));
      if (ageHours > 0) {
        const old = new Date(Date.now() - ageHours * 60 * 60 * 1000);
        fs.utimesSync(dir, old, old);
      }
      return dir;
    }
    const staleDead = makeDir('ccs-test-home-stale-', deadPid, 7);
    const staleAlive = makeDir('ccs-test-home-stale-alive-', alivePid, 7);
    const freshDead = makeDir('ccs-test-home-fresh-', deadPid, 0);
    const staleOtherPrefix = makeDir('ccs-test-stale-', deadPid, 7);
    const freshOther = makeDir('aac-should-not-sweep-', deadPid, 7);
    // The helper's wrapper may have added these to tracked set; keep them for sweep check
    try {
      const removed = sweep();
      expect(removed).toBeGreaterThanOrEqual(2); // staleDead + staleOtherPrefix at least
      expect(fs.existsSync(staleDead)).toBe(false);
      expect(fs.existsSync(staleOtherPrefix)).toBe(false);
      expect(fs.existsSync(staleAlive)).toBe(true);
      expect(fs.existsSync(freshDead)).toBe(true);
      // Prefix that sweep does not cover must stay
      expect(fs.existsSync(freshOther)).toBe(true);
    } finally {
      for (const d of [staleAlive, freshDead, freshOther]) {
        try {
          fs.rmSync(d, { recursive: true, force: true });
        } catch {}
        trackSet.delete(d);
      }
    }
  });

  it('sweep ignores fresh folders and alive owners', () => {
    const tmp = os.tmpdir();
    const dir = fs.mkdtempSync(path.join(tmp, 'ccs-test-home-fresh2-'));
    fs.writeFileSync(path.join(dir, '.aac-test-pid'), String(999999));
    // fresh: no utimes to old
    const before = fs.existsSync(dir);
    sweep();
    expect(fs.existsSync(dir)).toBe(before);
    fs.rmSync(dir, { recursive: true, force: true });
    trackSet.delete(dir);
  });
});
