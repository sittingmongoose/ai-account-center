'use strict';

/**
 * One release wasm build at a time per computer.
 *
 * The release wasm build ends in a fat-LTO link that needs several GB of memory, and every
 * push runs it through the pre-push gate. Several worktrees pushing at once exhausted the
 * VM's memory, so the build takes an exclusive lock first and waits for it only a bounded time.
 *
 * Linux with the `flock` tool: the wasm-pack command runs under `flock -n`, so the kernel
 * frees the lock the moment the build ends or dies. Elsewhere (macOS, Windows, Linux without
 * flock): an exclusive-create owner file holds the PID and start time, and a holder that is
 * gone (or older than STALE_AFTER_MS) is cleared.
 */

const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const DEFAULT_WAIT_S = 600;
const POLL_MS = 3000;
const NOTICE_EVERY_MS = 60_000;
/** A release wasm build never takes this long; an older owner file is a leftover. */
const STALE_AFTER_MS = 3 * 60 * 60 * 1000;
/** Exit code flock returns when the lock is busy; chosen so wasm-pack never uses it. */
const FLOCK_BUSY_EXIT = 197;
const BUSY_MESSAGE = 'UI build lane busy (another release wasm build is running); retry later';

function cacheRoot(env = process.env) {
  const base = env.XDG_CACHE_HOME ? env.XDG_CACHE_HOME : path.join(os.homedir(), '.cache');
  return path.join(base, 'ai-account-center');
}

function lockPath(env = process.env) {
  return path.join(cacheRoot(env), 'ui-build.lock');
}

/** The shared incremental target for the wasm build; an explicit CARGO_TARGET_DIR wins. */
function wasmTargetDir(env = process.env) {
  if (env.CARGO_TARGET_DIR) return env.CARGO_TARGET_DIR;
  return path.join(cacheRoot(env), 'cargo-target-wasm');
}

function waitSeconds(env = process.env) {
  const raw = env.AAC_UI_BUILD_LOCK_WAIT_S;
  if (raw === undefined || raw === '') return DEFAULT_WAIT_S;
  const value = Number(raw);
  return Number.isFinite(value) && value >= 0 ? value : DEFAULT_WAIT_S;
}

function sleepSync(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function hasFlockTool() {
  if (process.platform !== 'linux') return false;
  const result = spawnSync('flock', ['--version'], { stdio: 'ignore', windowsHide: true });
  return !result.error && result.status === 0;
}

function processAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error && error.code === 'EPERM';
  }
}

/** Linux process start time in clock ticks since boot, or null when it cannot be read. */
function linuxStartTicks(pid) {
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ');
    return fields[19] ?? null;
  } catch {
    return null;
  }
}

function ownerRecord(now) {
  return {
    pid: process.pid,
    startTicks: linuxStartTicks(process.pid),
    acquiredAt: now,
    host: os.hostname(),
  };
}

function readOwner(file) {
  try {
    return fs.readFileSync(file, 'utf8');
  } catch {
    return null;
  }
}

/** True when the recorded holder is gone, was replaced by another process, or is too old. */
function isStale(text, now, alive = processAlive, startTicks = linuxStartTicks) {
  let owner;
  try {
    owner = JSON.parse(text);
  } catch {
    return true;
  }
  if (!owner || !Number.isInteger(owner.pid) || owner.pid <= 0) return true;
  if (typeof owner.acquiredAt !== 'number' || now - owner.acquiredAt > STALE_AFTER_MS) return true;
  if (!alive(owner.pid)) return true;
  if (owner.startTicks) {
    const current = startTicks(owner.pid);
    if (current !== null && current !== owner.startTicks) return true;
  }
  return false;
}

/** One attempt at the owner-file lock; returns a release function or null when busy. */
function tryOwnerFile(file, now, staleCheck = isStale) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const record = JSON.stringify(ownerRecord(now));
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      fs.writeFileSync(file, record, { flag: 'wx', mode: 0o644 });
      return () => {
        if (readOwner(file) === record) fs.rmSync(file, { force: true });
      };
    } catch (error) {
      if (!error || error.code !== 'EEXIST') throw error;
    }
    const existing = readOwner(file);
    if (existing === null) continue;
    if (!staleCheck(existing, now)) return null;
    // Move the stale file aside first, so two waiters never both delete a fresh lock.
    const aside = `${file}.stale-${process.pid}-${now}`;
    try {
      fs.renameSync(file, aside);
    } catch {
      continue;
    }
    if (readOwner(aside) === existing) fs.rmSync(aside, { force: true });
    else {
      try {
        fs.linkSync(aside, file);
      } catch {
        // Someone else holds the lock now; theirs stands.
      }
      fs.rmSync(aside, { force: true });
    }
  }
  return null;
}

/**
 * Run `command args` while holding the computer-wide UI build lock.
 * options: env, run (the build's command runner), lockFile, waitS, useFlock,
 * sleep, now, log (a line each minute while waiting).
 */
function runWithUiBuildLock(command, args, runOptions, options = {}) {
  const env = options.env ?? process.env;
  const run = options.run;
  const file = options.lockFile ?? lockPath(env);
  const waitMs = (options.waitS ?? waitSeconds(env)) * 1000;
  const useFlock = options.useFlock ?? hasFlockTool();
  const sleep = options.sleep ?? sleepSync;
  const now = options.now ?? Date.now;
  const log = options.log ?? ((line) => console.log(line));
  fs.mkdirSync(path.dirname(file), { recursive: true });

  const started = now();
  let nextNotice = started + NOTICE_EVERY_MS;
  for (;;) {
    if (useFlock) {
      try {
        return run(
          'flock',
          ['-n', '-E', String(FLOCK_BUSY_EXIT), file, command, ...args],
          runOptions
        );
      } catch (error) {
        if (!error || error.status !== FLOCK_BUSY_EXIT) throw error;
      }
    } else {
      const release = tryOwnerFile(file, now());
      if (release) {
        try {
          return run(command, args, runOptions);
        } finally {
          release();
        }
      }
    }
    const elapsed = now() - started;
    if (elapsed >= waitMs) throw new Error(BUSY_MESSAGE);
    if (now() >= nextNotice) {
      log(
        `[i] Waiting for the UI build lane: another release wasm build is running (${Math.round(elapsed / 1000)}s of ${Math.round(waitMs / 1000)}s).`
      );
      nextNotice += NOTICE_EVERY_MS;
    }
    sleep(Math.min(POLL_MS, Math.max(0, waitMs - elapsed)));
  }
}

module.exports = {
  runWithUiBuildLock,
  tryOwnerFile,
  isStale,
  cacheRoot,
  lockPath,
  wasmTargetDir,
  waitSeconds,
  hasFlockTool,
  BUSY_MESSAGE,
  FLOCK_BUSY_EXIT,
  DEFAULT_WAIT_S,
  STALE_AFTER_MS,
};
