#!/usr/bin/env node
'use strict';

/**
 * Test Environment Isolation
 *
 * Provides isolated ~/.ccs/ directory for tests to prevent
 * interference with the user's actual CCS configuration.
 *
 * Usage:
 *   const { createTestEnvironment } = require('../fixtures/test-environment');
 *
 *   describe('my tests', () => {
 *     let testEnv;
 *
 *     before(() => {
 *       testEnv = createTestEnvironment();
 *     });
 *
 *     after(() => {
 *       testEnv.cleanup();
 *     });
 *
 *     it('test something', () => {
 *       // Tests run with CCS_HOME pointing to temp directory
 *       // Your test code here
 *     });
 *   });
 */

const fs = require('fs');
const path = require('path');
const os = require('os');

// A server started by a test runs the Status collectors, which publish schemas and seeds into the
// reports share. Point them at a folder that does not exist (AAC never creates the root), so no
// test can read or write the real share.
process.env.AAC_AGENT_REPORTS_DIR ||= path.join(os.tmpdir(), `ccs-test-no-reports-${process.pid}`);
process.env.AAC_PM_MAIL_REGISTRY_DIR ||= path.join(os.tmpdir(), `ccs-test-no-registry-${process.pid}`);
process.env.AAC_STATUS_DOORBELL ||= 'off';

let bootstrappedTestHome;
const originalHomedir = os.homedir;
// Capture the real home before any test redirect so the shared cache can live outside temp homes.
// Prefer the OS userInfo home (stable across HOME redirects where a parent test process
// has already redirected HOME to a temp home). Fall back to os.homedir before redirect.
const REAL_HOME = (() => {
  try {
    const info = os.userInfo();
    if (info && typeof info.homedir === 'string' && info.homedir) return info.homedir;
  } catch {}
  try {
    return originalHomedir();
  } catch {
    return process.env.HOME || process.env.USERPROFILE || os.homedir();
  }
})();
let homedirPatched = false;

// --- Tracked temp-home cleanup -------------------------------------------------
const trackedTestHomes = new Set();
let cleanupRegistered = false;
const PID_MARKER_NAME = '.aac-test-pid';

function writePidMarker(dir) {
  try {
    fs.writeFileSync(path.join(dir, PID_MARKER_NAME), String(process.pid));
  } catch {
    // best effort: marker is used by the sweeper to detect dead owners
  }
}

function cleanupAllTrackedHomes() {
  for (const dir of Array.from(trackedTestHomes)) {
    try {
      fs.rmSync(dir, { recursive: true, force: true });
    } catch {
      // ignore: missing dir is fine, rmSync with force:true already handles it
    }
    trackedTestHomes.delete(dir);
  }
}

let _bunAfterAllRegistered = false;
function ensureCleanupRegistered() {
  if (cleanupRegistered) return;
  cleanupRegistered = true;
  process.on('exit', cleanupAllTrackedHomes);
  for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    process.on(sig, () => {
      cleanupAllTrackedHomes();
      // Re-raise the signal so the exit code stays right (not a plain exit 0).
      process.removeAllListeners(sig);
      process.kill(process.pid, sig);
    });
  }
  // Bun's test runner does not fire process.on('exit') when its test process
  // finishes (verified: no EXIT event in Bun's test context, while Node does fire it).
  // Register a bun:test afterAll hook so tracked temp homes are removed when the
  // test process finishes, not just on Node exit. This is best-effort: if bun:test
  // is unavailable (Node runtime), the hook is skipped and process.on('exit') applies.
  try {
    // eslint-disable-next-line global-require
    const bunTest = require('bun:test');
    if (bunTest && typeof bunTest.afterAll === 'function' && !_bunAfterAllRegistered) {
      bunTest.afterAll(() => {
        cleanupAllTrackedHomes();
      });
      _bunAfterAllRegistered = true;
    }
  } catch {}
}

function trackTestHome(dir) {
  if (!dir) return;
  trackedTestHomes.add(dir);
  ensureCleanupRegistered();
}

// --- Shared cache outside temp homes -----------------------------------------
let _sharedCacheRoot;
function getSharedCacheRoot() {
  if (_sharedCacheRoot) return _sharedCacheRoot;
  const envRoot = process.env.AAC_TEST_SHARED_CACHE;
  if (envRoot) {
    _sharedCacheRoot = envRoot;
  } else {
    _sharedCacheRoot = path.join(REAL_HOME, '.cache', 'aac-test-shared-cache');
  }
  return _sharedCacheRoot;
}

function ensureSharedCache() {
  const root = getSharedCacheRoot();
  try {
    fs.mkdirSync(root, { recursive: true });
  } catch {}
  // One shared root holds package caches only - never credentials or user config.
  const bunInstall = path.join(root, 'bun-install');
  const bunGlobal = path.join(root, 'bun-global');
  const npmCache = path.join(root, 'npm-cache');
  const yarnCache = path.join(root, 'yarn-cache');
  const pnpmStore = path.join(root, 'pnpm-store');
  const pnpmHome = path.join(root, 'pnpm-home');
  for (const dir of [bunInstall, bunGlobal, npmCache, yarnCache, pnpmStore, pnpmHome]) {
    try {
      fs.mkdirSync(dir, { recursive: true });
    } catch {}
  }
  // Point package caches at the shared root BEFORE HOME is redirected.
  // This prevents every temp home from downloading its own copy of the same packages
  // (the previous behavior copied ~230MB per home into /tmp).
  // Redirected: bun (BUN_INSTALL_CACHE_DIR), npm (npm_config_cache), yarn
  // (YARN_CACHE_FOLDER), pnpm store/home. XDG cache and vite-plus are NOT
  // redirected to the shared root because XDG_CACHE_HOME is intentionally left
  // per-home for test isolation: the UI build lock and other fixtures rely on a
  // per-test XDG cache to avoid cross-test contention. Their caches land in the
  // temp home's own .cache and are removed with the home. That is the tradeoff
  // noted in the task ("if a tool's cache can't be redirected safely, say so").
  process.env.BUN_INSTALL_CACHE_DIR = bunInstall;
  process.env.BUN_INSTALL_GLOBAL_DIR = bunGlobal;
  process.env.npm_config_cache = npmCache;
  process.env.YARN_CACHE_FOLDER = yarnCache;
  process.env.PNPM_HOME = pnpmHome;
  process.env.PNPM_STORE_DIR = pnpmStore;
  return root;
}

// --- Sweep stale leftovers ----------------------------------------------------
const SWEEP_PREFIXES = ['ccs-test-home-', 'ccs-test-'];
const SWEEP_AGE_MS = 6 * 60 * 60 * 1000;

function isOwnerAlive(pid) {
  if (!Number.isFinite(pid)) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    if (err && err.code === 'EPERM') return true;
    return false;
  }
}

function sweepStaleTestHomes() {
  const tmp = os.tmpdir();
  let entries;
  try {
    entries = fs.readdirSync(tmp);
  } catch {
    return 0;
  }
  const now = Date.now();
  let removed = 0;
  for (const name of entries) {
    if (!SWEEP_PREFIXES.some((p) => name.startsWith(p))) continue;
    const full = path.join(tmp, name);
    let stat;
    try {
      stat = fs.statSync(full);
    } catch {
      continue;
    }
    if (!stat.isDirectory()) continue;
    // Use mtime as the age signal; a directory that has not been touched in 6h is stale.
    const age = now - stat.mtimeMs;
    if (age < SWEEP_AGE_MS) continue;
    let ownerAlive = false;
    try {
      const marker = path.join(full, PID_MARKER_NAME);
      const content = fs.readFileSync(marker, 'utf8').trim();
      const pid = Number.parseInt(content, 10);
      if (Number.isFinite(pid)) {
        ownerAlive = isOwnerAlive(pid);
      } else {
        ownerAlive = false;
      }
    } catch {
      // No marker (legacy folder created before markers): treat as dead if old enough.
      ownerAlive = false;
    }
    if (!ownerAlive) {
      try {
        fs.rmSync(full, { recursive: true, force: true });
        removed++;
      } catch {}
    }
  }
  return removed;
}

// Sweep at test start (when this module is preloaded). Runs before new homes are created.
try {
  sweepStaleTestHomes();
} catch {}

// --- Auto-track other mkdtemp callers the same way ----------------------------
// Every temp dir created via fs.mkdtemp* under os.tmpdir() is recorded and removed
// on exit/signal. This fixes the "obvious leaks" where callers forget cleanup and
// matches createIsolatedTestHome behavior without editing every test file.
// Existing afterEach/try-finally cleanup stays valid; double-removal with force:true is harmless.
const _mkdtempSync = fs.mkdtempSync;
const _mkdtemp = fs.mkdtemp;
let _promisesMkdtemp = null;
try {
  if (fs.promises && typeof fs.promises.mkdtemp === 'function') {
    _promisesMkdtemp = fs.promises.mkdtemp;
  }
} catch {}

function maybeTrackMkdtempDir(dir, prefix) {
  try {
    if (!dir || typeof dir !== 'string') return;
    const tmpDir = os.tmpdir();
    const tmpNorm = path.normalize(tmpDir);
    const dirNorm = path.normalize(dir);
    if (!dirNorm.startsWith(tmpNorm)) return;
    // Write a pid marker only for the sweep-covered prefixes (ccs-test-*).
    // Other prefixes are still tracked for afterAll/exit cleanup, but get no
    // marker file so fixtures that assert an empty directory are not broken.
    const basePrefix = typeof prefix === 'string' ? path.basename(String(prefix)) : '';
    const isTestPrefix = basePrefix.startsWith('ccs-test-') || basePrefix.startsWith('ccs-test-home-');
    if (isTestPrefix) {
      writePidMarker(dir);
    }
    trackTestHome(dir);
  } catch {}
}

if (typeof _mkdtempSync === 'function') {
  fs.mkdtempSync = function(prefix, ...args) {
    const dir = _mkdtempSync.call(fs, prefix, ...args);
    maybeTrackMkdtempDir(dir, prefix);
    return dir;
  };
}
if (typeof _mkdtemp === 'function') {
  fs.mkdtemp = function(prefix, callback) {
    // Overload: mkdtemp(prefix, callback)  and  mkdtemp(prefix, options, callback)
    const cb = typeof arguments[arguments.length - 1] === 'function' ? arguments[arguments.length - 1] : null;
    const extraArgs = Array.prototype.slice.call(arguments, 1, cb ? -1 : undefined);
    if (!cb) {
      return _mkdtemp.call(fs, prefix, ...extraArgs);
    }
    return _mkdtemp.call(fs, prefix, ...extraArgs, (err, dir) => {
      if (!err && dir) maybeTrackMkdtempDir(dir, prefix);
      cb(err, dir);
    });
  };
}
if (_promisesMkdtemp) {
  const origPromisesMktemp = fs.promises.mkdtemp;
  fs.promises.mkdtemp = async function(prefix, ...args) {
    const dir = await origPromisesMktemp.call(fs.promises, prefix, ...args);
    maybeTrackMkdtempDir(dir, prefix);
    return dir;
  };
}

function getEffectiveTestHome() {
  return process.env.CCS_HOME || process.env.HOME || process.env.USERPROFILE || bootstrappedTestHome || originalHomedir();
}

function patchHomedirForTests() {
  if (homedirPatched) {
    return;
  }

  os.homedir = () => getEffectiveTestHome();
  homedirPatched = true;
}

function createIsolatedTestHome(prefix = 'ccs-test-home-') {
  ensureSharedCache();
  const testHome = _mkdtempSync.call(fs, path.join(os.tmpdir(), prefix));
  writePidMarker(testHome);
  trackTestHome(testHome);
  fs.mkdirSync(path.join(testHome, '.ccs'), { recursive: true });
  fs.mkdirSync(path.join(testHome, '.claude'), { recursive: true });
  fs.mkdirSync(path.join(testHome, '.config'), { recursive: true });
  fs.mkdirSync(path.join(testHome, '.cache'), { recursive: true });
  fs.mkdirSync(path.join(testHome, '.state'), { recursive: true });
  return testHome;
}

function ensureGlobalTestEnvironment() {
  if (bootstrappedTestHome) {
    return bootstrappedTestHome;
  }

  ensureSharedCache();
  const testHome = createIsolatedTestHome();
  // Inherited overrides must never route tests back to private user state or
  // pass live credentials to the provider collectors started by server tests.
  for (const key of Object.keys(process.env)) {
    if (/(?:TOKEN|API_KEY|SECRET|PASSWORD|ACCESS_KEY|COOKIE)/.test(key)) {
      delete process.env[key];
    }
  }
  for (const key of [
    'CCS_DIR', 'CCS_CONFIG', 'CCS_UNIFIED_CONFIG', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR',
    'CCS_CLAUDE_PATH', 'CCS_CODEX_PATH', 'CCS_DROID_PATH', 'CCS_PROXY_HOST',
    'CCS_PROXY_PORT', 'CCS_PROXY_PROTOCOL', 'CCS_TEST_DISABLE_GLOBAL_BOOTSTRAP'
  ]) {
    delete process.env[key];
  }
  process.env.HOME = testHome;
  process.env.USERPROFILE = testHome;
  process.env.CCS_HOME = testHome;
  process.env.XDG_CONFIG_HOME = path.join(testHome, '.config');
  process.env.XDG_CACHE_HOME = path.join(testHome, '.cache');
  process.env.XDG_STATE_HOME = path.join(testHome, '.state');
  process.env.CCS_TEST_BOOTSTRAP_HOME = testHome;

  bootstrappedTestHome = testHome;
  patchHomedirForTests();
  return bootstrappedTestHome;
}

/**
 * Create an isolated test environment
 * Sets CCS_HOME to a temporary directory and provides cleanup
 *
 * @returns {object} Test environment with paths and cleanup function
 */
function createTestEnvironment() {
  ensureSharedCache();
  // Create unique temp directory for this test run
  const testHome = createIsolatedTestHome('ccs-test-');
  const testCcsDir = path.join(testHome, '.ccs');

  // Store original environment
  const originalHome = process.env.HOME;
  const originalCcsHome = process.env.CCS_HOME;
  const originalCcsDir = process.env.CCS_DIR;
  const originalUserProfile = process.env.USERPROFILE;
  const originalXdgConfigHome = process.env.XDG_CONFIG_HOME;
  const originalXdgCacheHome = process.env.XDG_CACHE_HOME;
  const originalXdgStateHome = process.env.XDG_STATE_HOME;

  // Keep HOME-family env vars aligned so code that still consults os.homedir()
  // or XDG defaults stays inside the isolated test sandbox.
  process.env.HOME = testHome;
  process.env.USERPROFILE = testHome;
  process.env.CCS_HOME = testHome;
  delete process.env.CCS_DIR;
  process.env.XDG_CONFIG_HOME = path.join(testHome, '.config');
  process.env.XDG_CACHE_HOME = path.join(testHome, '.cache');
  process.env.XDG_STATE_HOME = path.join(testHome, '.state');
  patchHomedirForTests();

  // Return environment object
  return {
    /** Path to the test home directory (like ~) */
    testHome,

    /** Path to the test .ccs directory (like ~/.ccs) */
    ccsDir: testCcsDir,

    /** Original HOME value for reference */
    originalHome,

    /**
     * Get a path within the test CCS directory
     * @param {...string} parts - Path segments
     * @returns {string} Full path
     */
    getCcsPath(...parts) {
      return path.join(testCcsDir, ...parts);
    },

    /**
     * Create a file in the test CCS directory
     * @param {string} relativePath - Path relative to .ccs/
     * @param {string|object} content - File content (objects are JSON stringified)
     */
    createFile(relativePath, content) {
      const fullPath = path.join(testCcsDir, relativePath);
      const dir = path.dirname(fullPath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
      const data = typeof content === 'object' ? JSON.stringify(content, null, 2) : content;
      fs.writeFileSync(fullPath, data);
    },

    /**
     * Read a file from the test CCS directory
     * @param {string} relativePath - Path relative to .ccs/
     * @param {boolean} [asJson=false] - Parse as JSON
     * @returns {string|object} File content
     */
    readFile(relativePath, asJson = false) {
      const fullPath = path.join(testCcsDir, relativePath);
      const content = fs.readFileSync(fullPath, 'utf8');
      return asJson ? JSON.parse(content) : content;
    },

    /**
     * Check if a file exists in the test CCS directory
     * @param {string} relativePath - Path relative to .ccs/
     * @returns {boolean}
     */
    fileExists(relativePath) {
      return fs.existsSync(path.join(testCcsDir, relativePath));
    },

    /**
     * Clean up the test environment and restore original settings
     */
    cleanup() {
      // Restore original environment
      if (originalHome !== undefined) {
        process.env.HOME = originalHome;
      } else {
        delete process.env.HOME;
      }

      if (originalUserProfile !== undefined) {
        process.env.USERPROFILE = originalUserProfile;
      } else {
        delete process.env.USERPROFILE;
      }

      if (originalCcsHome !== undefined) {
        process.env.CCS_HOME = originalCcsHome;
      } else {
        delete process.env.CCS_HOME;
      }
      if (originalCcsDir !== undefined) {
        process.env.CCS_DIR = originalCcsDir;
      } else {
        delete process.env.CCS_DIR;
      }

      if (originalXdgConfigHome !== undefined) {
        process.env.XDG_CONFIG_HOME = originalXdgConfigHome;
      } else {
        delete process.env.XDG_CONFIG_HOME;
      }

      if (originalXdgCacheHome !== undefined) {
        process.env.XDG_CACHE_HOME = originalXdgCacheHome;
      } else {
        delete process.env.XDG_CACHE_HOME;
      }

      if (originalXdgStateHome !== undefined) {
        process.env.XDG_STATE_HOME = originalXdgStateHome;
      } else {
        delete process.env.XDG_STATE_HOME;
      }

      // Clean up temp directory (removing twice is harmless)
      try {
        fs.rmSync(testHome, { recursive: true, force: true });
      } catch (err) {
        // Ignore cleanup errors
        console.warn(`[test-environment] Cleanup warning: ${err.message}`);
      } finally {
        trackedTestHomes.delete(testHome);
      }
    }
  };
}

/**
 * Get the CCS home directory (respects CCS_HOME env var)
 * This should be used instead of os.homedir() for CCS paths
 *
 * @returns {string} Home directory path
 */
function getCcsHome() {
  return process.env.CCS_HOME || os.homedir();
}

/**
 * Get the CCS directory path
 *
 * @returns {string} Path to .ccs directory
 */
function getCcsDir() {
  return path.join(getCcsHome(), '.ccs');
}

module.exports = {
  createTestEnvironment,
  ensureGlobalTestEnvironment,
  getCcsHome,
  getCcsDir,
  // For tests and debugging
  _tracked: trackedTestHomes,
  _getSharedCacheRoot: getSharedCacheRoot,
  _sweepStaleTestHomes: sweepStaleTestHomes,
  _cleanupAllTrackedHomes: cleanupAllTrackedHomes,
  _trackTestHome: trackTestHome,
  _PID_MARKER_NAME: PID_MARKER_NAME,
  _createIsolatedTestHome: createIsolatedTestHome
};

ensureGlobalTestEnvironment();

