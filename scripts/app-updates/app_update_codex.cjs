#!/usr/bin/env node
'use strict';

// Fixed Codex family updates. Runtime snapshots/auth environments remain in memory.
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFile, spawn } = require('child_process');
const home = os.homedir();
const executable = path.join(home, '.local/bin/codex');
// A remote host without AAC runs the self-contained bundle synced beside this
// helper; the VM loads the runtime of its installed AAC package.
const modules = [path.join(__dirname, 'app_update_codex_runtime.cjs'),
  path.resolve(__dirname, '../../dist/codex-auth/codex-activation-runtime.js'),
  path.join(home, '.local/lib/node_modules/@sittingmongoose/ai-account-center/dist/codex-auth/codex-activation-runtime.js'),
  path.join(home, '.local/lib/node_modules/@kaitranntt/ccs/dist/codex-auth/codex-activation-runtime.js')];
const flags = Object.fromEntries(process.argv.slice(2).reduce((pairs, arg, index, args) => {
  if (arg.startsWith('--')) pairs.push([arg, args[index + 1]]); return pairs;
}, []));
const operation = flags['--operation'] === 'desktop' ? 'desktop' : 'cli';
const seconds = /^\d+$/.test(flags['--timeout-seconds'] || '') ? Math.max(30, Math.min(900, Number(flags['--timeout-seconds']))) : 900;
// Shared Codex work is often busy for hours (long goals, other projects'
// stdio servers). Waiting longer than this for idle only stalls Update apps;
// a busy Codex becomes an honest "Codex is busy" row instead.
const IDLE_WAIT_SECONDS = 60;
const LOCK_WAIT_SECONDS = 30;
const RESTART_SETTLE_MS = 10000;
const output = { appId: operation === 'desktop' ? 'codex-desktop' : 'codex-cli', platform: 'ubuntu',
  status: 'failed', previousVersion: null, version: null, manager: operation === 'desktop' ? 'apt' : 'native',
  messageCode: 'update_failed', updateAttempted: false, restartedProcesses: 0, forcedStops: 0 };
const cleanEnvironment = (env) => Object.fromEntries(Object.entries(env).filter(([key]) => !/^CCS_SESSION_SECRET$|^CCS_DASHBOARD_|_PASSWORD_HASH$/i.test(key)));
const version = (text) => typeof text === 'string' ? text.match(/\d+\.\d+(?:\.\d+){0,3}/)?.[0] ?? null : null;
// A process in a systemd user service's cgroup (the dashboard) charges whatever it starts to that
// service: a relaunched Codex and everything it runs would count against the dashboard's MemoryMax,
// and with KillMode=process would keep its cgroup, and so its memory.peak, alive across restarts.
// An ssh session or a plain shell has no such cgroup and needs no transient unit.
function insideServiceCgroup(read = () => fs.readFileSync('/proc/self/cgroup', 'utf8')) {
  try { return read().split('\n').some((line) => /\/user@\d+\.service\/(?:.*\/)?[^/]+\.service(?:\/|$)/.test(line.trim())); } catch { return false; }
}
/** Restart one captured process from the updated install, outside the dashboard's cgroup. */
async function relaunch(target, desktop, outside, deps = {}) {
  const actual = desktop ? target.exe : (deps.executable ?? executable);
  const env = cleanEnvironment(target.env);
  if ((deps.inside ?? insideServiceCgroup)()) {
    // An older runtime offers no launcher. Starting here would cage the app in the dashboard.
    if (!outside) throw new Error('Launch failed.');
    const started = await outside({ ...target, exe: actual, env }, desktop);
    output.restartedProcesses++;
    return started;
  } else {
    const child = (deps.spawn ?? spawn)(actual, target.args.slice(1), { cwd: target.cwd, env, detached: true,
      argv0: desktop ? target.args[0] : (deps.executable ?? executable), stdio: ['ignore', 'ignore', 'ignore'] });
    await new Promise((resolve, reject) => { child.once('spawn', resolve); child.once('error', () => reject(new Error('Launch failed.'))); });
    child.unref();
    output.restartedProcesses++;
    // The pid it spawned and that process's start time, so start() can know the process it started
    // from any other, and never from a later one given the same pid. Without the start time the pid
    // names nothing start() will count.
    const startTime = (deps.startTimeOf ?? startTimeOf)(child.pid);
    return { pid: child.pid, ...(startTime ? { startTime } : {}) };
  }
}
/** The update bridge's process runtime: its launches stay outside the dashboard cgroup. */
function createUpdateRuntime(createRuntime, deps = {}, hooks = {}) {
  return createRuntime(path.join(home, '.codex'), { scan: async () => privateScan(),
    launch: (target, desktop, outside) => relaunch(target, desktop, outside, deps), ...hooks });
}
/**
 * What a marker says is owed: the daemon, and how many desktops. A desktop an earlier run closed stays
 * owed whatever runs now: only AAC's own relaunch of it, seen coming up, clears it, and that run's
 * launch snapshot is gone. An older marker listed desktops one entry each; one that names a desktop but
 * no count is one desktop.
 */
function markerObligations(marker, legacyDesktop) {
  const listed = Array.isArray(marker?.desktops) ? marker.desktops.filter((item) => typeof item === 'string').length
    : Number.isInteger(marker?.desktops) && marker.desktops > 0 ? marker.desktops : 0;
  return { daemon: marker?.daemon === true, desktops: listed || (marker?.desktop === true || legacyDesktop ? 1 : 0) };
}
const addObligations = (prior, plan) => ({ daemon: prior.daemon || plan.daemon, desktops: prior.desktops + plan.desktops });
const markerFields = (owed) => ({ desktop: owed.desktops > 0, daemon: owed.daemon, desktops: owed.desktops });
/** Still owed: a daemon `live` doesn't show ready, or any desktop, since none is cleared by looking. */
const stillOwed = (owed, live) => (owed.daemon && !live.daemon) || owed.desktops > 0;
function run(binary, args, timeout = 10000) {
  return new Promise((resolve, reject) => execFile(binary, args, { timeout, maxBuffer: 65536, encoding: 'utf8',
    env: { ...cleanEnvironment(process.env), CODEX_NON_INTERACTIVE: '1', NO_UPDATE_NOTIFIER: '1', DEBIAN_FRONTEND: 'noninteractive' } },
    (error, stdout) => error ? reject(new Error('Fixed app command failed.')) : resolve(stdout)));
}
function privateScan() {
  const values = [];
  for (const name of fs.readdirSync('/proc')) {
    if (!/^\d+$/.test(name)) continue;
    const directory = path.join('/proc', name);
    try {
      if (fs.statSync(directory).uid !== process.getuid()) continue;
      const stat = fs.readFileSync(path.join(directory, 'stat'), 'utf8');
      const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ');
      const args = fs.readFileSync(path.join(directory, 'cmdline'), 'utf8').split('\0').filter(Boolean);
      let exe;
      try { exe = fs.readlinkSync(path.join(directory, 'exe')).replace(/ \(deleted\)$/, ''); }
      catch (error) { if (error.code !== 'EACCES') throw error; exe = args[0] || ''; }
      const codex = path.basename(exe) === 'codex';
      // Fresh terminal CLI restarts are handled separately by the Python helper.
      // They are never mistaken for shared-daemon clients or replayed here.
      if (codex && !args.includes('app-server')) continue;
      const relevant = codex || exe === '/usr/lib/chatgpt/ChatGPT';
      const env = {};
      if (relevant) {
        const raw = fs.readFileSync(path.join(directory, 'environ'), 'utf8');
        if (raw.length > 1024 * 1024) throw new Error('Oversized context.');
        for (const item of raw.split('\0')) { const split = item.indexOf('='); if (split > 0) env[item.slice(0, split)] = item.slice(split + 1); }
      }
      values.push({ pid: Number(name), ppid: Number(fields[1]), state: fields[0], startTime: fields[19], args, exe,
        env, cwd: relevant ? fs.readlinkSync(path.join(directory, 'cwd')) : '', ...(relevant ? { unit: processUnit(directory) } : {}) });
    } catch (error) { if (!['ENOENT', 'ESRCH'].includes(error.code)) throw new Error('Codex inventory failed.'); }
  }
  return values;
}
/** The systemd service named by a process's cgroup text, so start() can know a desktop it launched there. */
function cgroupUnit(text) {
  const line = String(text).split('\n').find((entry) => entry.startsWith('0::'));
  const last = line?.slice(3).split('/').pop();
  return last && last.endsWith('.service') ? last : undefined;
}
function processUnit(directory) {
  try { return cgroupUnit(fs.readFileSync(path.join(directory, 'cgroup'), 'utf8')); } catch { return undefined; }
}
/** A process's start time, as /proc/<pid>/stat gives it and the scans record it; else undefined. */
function startTimeOf(pid) {
  try {
    const stat = fs.readFileSync(path.join('/proc', String(pid), 'stat'), 'utf8');
    return stat.slice(stat.lastIndexOf(')') + 2).split(' ')[19];
  } catch { return undefined; }
}
function markerFile() { return path.join(home, '.ccs/app-updates', output.appId + '-pending-restart.json'); }
function readMarker() { try { return JSON.parse(fs.readFileSync(markerFile(), 'utf8')); } catch { return null; } }
/**
 * `closed`: what a run's stop() has closed and not seen back: the `daemon`, and how many `desktops`
 * (`desktop` says whether any). It holds no path or profile. A marker without them, written before
 * stop(), says only that a restart is owed. `version` is what was installed
 * when it was written, and `target` the version a desktop install was about to put in place.
 */
const markerMatches = (marker, installed) => Boolean(installed) && [marker?.version, marker?.target].some((item) => version(item) === installed);
function writeMarker(text, file = markerFile()) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const temporary = file + '.' + process.pid + '.tmp';
  fs.writeFileSync(temporary, text, { mode: 0o600 });
  fs.renameSync(temporary, file);
}
function markPending(closed = {}) { writeMarker(JSON.stringify({ version: output.version, ...closed })); }
/** The marker exactly as stored, to put back later byte for byte; null when there is none. */
function markerSnapshot(file = markerFile()) {
  try { return fs.readFileSync(file, 'utf8'); } catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}
function restoreMarker(text, file = markerFile()) { writeMarker(text, file); }
// An older runtime can't say what it closed, so it is taken to have closed a daemon and a desktop.
const UNKNOWN_PLAN = { daemon: true, desktops: 1 };
const counted = (plan) => ({ daemon: plan.daemon === true,
  desktops: Array.isArray(plan.desktops) ? plan.desktops.length : Number.isInteger(plan.desktops) ? plan.desktops : 1 });
const runtimePlan = (runtime) => counted(runtime.stopPlan?.() ?? UNKNOWN_PLAN);
const runtimeRetired = async (runtime) => counted((await runtime.retiredPlan?.()) ?? UNKNOWN_PLAN);
const runtimeState = async (runtime) => (await runtime.restartState?.()) ?? { daemon: false, proxy: false };
/**
 * What runs now, with a daemon that owes readiness held to start()'s own bounded check. That check can
 * take about 40 seconds, so the daemon is judged on a scan taken after it. The daemon in that scan must be the very one the check vouched for, by pid and
 * start time. One that replaced it during the check gets a check of its own, once; a daemon that
 * keeps being replaced is not ready.
 */
const sameDaemon = (left, right) => Boolean(left && right && left.pid === right.pid && left.startTime === right.startTime);
const verifiedState = async (runtime, deps, owed) => {
  let after = await runtimeState(runtime, deps);
  if (!owed.daemon || !after.daemon) return after;
  for (let attempt = 0; attempt < 2; attempt++) {
    const checked = await runtime.verifyRunningDaemon?.();
    after = await runtimeState(runtime, deps);
    if (!checked || typeof checked !== 'object' || !after.daemon) return { ...after, daemon: false };
    if (sameDaemon(checked, after.daemonIdentity)) return after;
  }
  return { ...after, daemon: false };
};
/**
 * What is owed once start() has run: what earlier runs closed, which only their own relaunches could
 * have cleared, and what this run closed and did not relaunch and see come up. An older runtime that
 * can't say rests on start()'s own checks, which fail when a desktop is not back.
 */
async function owedAfterStart(runtime, deps, prior, closed) {
  const left = typeof runtime.retiredPlan === 'function' ? (await runtimeRetired(runtime, deps)).desktops : 0;
  return { daemon: closed.daemon, desktops: prior.desktops + left };
}
async function execute(deps) {
  const idle = Math.max(1, Math.min(seconds, deps.idleSeconds ?? IDLE_WAIT_SECONDS));
  const deadline = deps.now() + idle * 1000;
  const mode = deps.operation ?? operation;
  const expected = deps.expectedCandidate ?? flags['--expected-version'];
  const restartOnly = deps.restartOnly ?? flags['--restart-only'] === 'true';
  let runtime, prior, closed, retried = false, stopped = false, installed = false, updating = false, nothingOwed = false, busy = false, stopFailed = false;
  let earlierAtStop, intentWritten = false;
  // Before the CLI updater: what was written, what was there before it, and what is installed now.
  let intended = false, earlierAtInstall = null, pendingBefore = false, installedNow = null;
  try {
    output.previousVersion = output.version = version(await deps.readVersion());
    if (!output.version) { output.messageCode = 'version_unknown'; return output; }
    // A retry restarts only what its own stop() finds, because the launch snapshot of a failed
    // earlier run is gone. The marker says what that run closed, and only a marker that names
    // something is a retry: a run stopped by a busy Codex, or a marker the Python path wrote,
    // closed nothing. What it names is owed whatever version is installed now, since a helper can be
    // killed after replacing the app. A desktop marker from before these fields counts as a closed
    // desktop if it is for this install: the old bridge wrote one after any stop(), even with no
    // desktop running, so a host with only a daemon can see one false restart_not_running before the
    // marker clears. Versions compare on their numeric part, since the Python path writes a
    // prerelease in full.
    const marker = deps.marked();
    const unnamed = marker && marker.desktop === undefined && marker.daemon === undefined;
    prior = markerObligations(marker, Boolean(mode === 'desktop' && unnamed && (restartOnly || markerMatches(marker, output.previousVersion))));
    retried = stillOwed(prior, { daemon: false });
    if (mode === 'cli') {
      output.updateAttempted = true;
      // The updater can replace Codex under a daemon still running the old binary, and the helper can
      // be killed before anything else is written. So a running daemon goes in the marker first, and the
      // earlier marker comes back exactly if the update turns out to change nothing. An older runtime,
      // one that can't be created, or a failed scan can't say, so there is taken to be one.
      pendingBefore = deps.pending(output.previousVersion);
      let running = true;
      try {
        const probe = deps.runtime();
        if (probe.restartState) running = (await probe.restartState()).daemon;
      } catch { /* Stays true. */ }
      const intent = { ...prior, daemon: prior.daemon || running };
      intended = stillOwed(intent, { daemon: false });
      earlierAtInstall = intended ? deps.markerSnapshot() : null;
      if (intended) deps.mark(markerFields(intent));
      updating = true;
      await deps.update();
      updating = false;
      output.version = installedNow = version(await deps.readVersion());
      if (!output.version) { output.messageCode = 'version_unknown'; return output; }
      if (expected && output.version !== version(expected)) throw new Error('Candidate not installed.');
      if (output.version === output.previousVersion && !retried && !pendingBefore) {
        if (intended) { if (earlierAtInstall === null) deps.clear(); else deps.restoreMarker(earlierAtInstall); }
        output.status = 'current'; output.messageCode = 'current'; return output;
      }
      // Until stop() hands over its plan, the marker keeps naming the daemon on the new version.
      deps.mark(intended ? markerFields(intent) : undefined);
    }
    const target = mode === 'desktop' && !restartOnly ? version(expected) : null;
    // The runtime hands over its plan under the startup lock, before it signals anything or waits on a
    // recovery. Writing it there means a helper killed anywhere inside stop() still leaves what is owed.
    // The marker as it was is kept, to put back once a failed stop() is shown to have recovered.
    const beforeStop = (plan) => {
      if (!intentWritten) { earlierAtStop = deps.markerSnapshot(); intentWritten = true; }
      const owed = addObligations(prior, counted(plan));
      if (stillOwed(owed, { daemon: false })) deps.mark({ ...markerFields(owed), ...(target ? { target } : {}) });
    };
    runtime = deps.runtime({ beforeStop });
    while (true) {
      try { await runtime.stop(); stopped = true; break; }
      catch (error) {
        if (error.code !== 'busy') { stopFailed = true; throw error; }
        if (deps.now() >= deadline) { busy = true; throw error; }
        await deps.sleep(Math.min(5000, deadline - deps.now()));
      }
    }
    // The runtime's own plan, captured under its lock: a process that appeared just before that lock
    // is closed with the rest, so an earlier scan here could miss it. It goes in the marker at once, so
    // a failed install or a killed helper still leaves a record of what is closed.
    closed = addObligations(prior, runtimePlan(runtime, deps));
    deps.mark({ ...markerFields(closed), ...(target ? { target } : {}) });
    if (mode === 'desktop') {
      output.updateAttempted = !restartOnly;
      updating = true;
      if (!restartOnly) await deps.update();
      updating = false;
      output.version = version(await deps.readVersion());
      if (!output.version || (!restartOnly && output.version === output.previousVersion) || (expected && output.version !== version(expected))) throw new Error('Candidate not installed.');
      installed = true;
      deps.mark(markerFields(closed));
    }
    await deps.restartProxies?.();
    await runtime.start(); stopped = false;
    if (await deps.hasOldProxies()) throw new Error('An old proxy has no verified restart supervisor.');
    // A desktop is back only once start() relaunched it and saw the process it started come up, so
    // what an earlier run closed stays owed: its launch snapshot is gone. The daemon must be ready on a
    // scan taken now. start() has just retired any daemon a proxy respawned too early, so while a live
    // proxy could bring the daemon back it gets a short while. A first update keeps what it still owes,
    // so the next click retries. A retry leaving owed what an earlier run closed says so once and clears
    // it, since nothing here could ever finish it. A first update on an older runtime that can't be
    // scanned rests on start()'s own checks.
    const nothing = { daemon: false };
    const owed = await owedAfterStart(runtime, deps, prior, closed);
    if ((retried || typeof runtime.restartState === 'function') && stillOwed(owed, nothing)) {
      const settleBy = deps.now() + (deps.settleMs ?? RESTART_SETTLE_MS);
      let live = await runtimeState(runtime, deps);
      while (owed.daemon && !live.daemon && live.proxy && deps.now() < settleBy) {
        await deps.sleep(250);
        live = await runtimeState(runtime, deps);
      }
      if (stillOwed(owed, await verifiedState(runtime, deps, owed))) {
        if (retried) { deps.clear(); output.status = 'restart_failed'; output.messageCode = 'restart_not_running'; }
        else { deps.mark(markerFields(owed)); output.status = 'restart_failed'; output.messageCode = 'restart_failed'; }
        return output;
      }
    }
    deps.clear(); output.status = 'updated'; output.messageCode = 'updated';
  } catch {
    // start() does nothing once it has released the lock, so a launch that failed there is not
    // retried here. The marker stays, and the next click reports what it finds (restart_not_running):
    // the updaters send a matching marker here even with no daemon running.
    // An installer that failed may still have replaced the app: what is installed now decides which
    // version the row and the marker name. A CLI update stops nothing first, so a daemon still running
    // the old binary is owed a restart too, or the next click would call it current.
    if (updating) {
      try {
        const now = version(await deps.readVersion());
        installedNow = now;
        if (now && now !== output.version) {
          output.version = now;
          if (mode === 'cli') {
            // An older runtime, one that can't be created, or a failed scan can't say, so there is
            // taken to be one.
            let running = true;
            try {
              const probe = deps.runtime();
              if (probe.restartState) running = (await probe.restartState()).daemon;
            } catch { /* Stays true. */ }
            const owed = { ...prior, daemon: prior.daemon || running };
            if (stillOwed(owed, { daemon: false })) deps.mark(markerFields(owed));
            else nothingOwed = true;
          }
        }
      } catch { /* The version stays as it was read before the install. */ }
    }
    // A CLI updater that failed, or installed a version other than expected, before anything stopped:
    // if what is installed is the version this run started from and nothing was owed before, nothing
    // changed, so the earlier marker comes back exactly and the row is a failed update. A version that
    // can't be read keeps the intent.
    if (mode === 'cli' && intended && !runtime && !retried && !pendingBefore && installedNow !== null && installedNow === output.previousVersion) {
      try {
        if (earlierAtInstall === null) deps.clear(); else deps.restoreMarker(earlierAtInstall);
        nothingOwed = true;
      } catch { /* The intent stays: the next click checks again. */ }
    }
    if (stopped) {
      try { await runtime.start(); } catch { /* Reported below as restart_failed. */ }
      // With everything back, a failed install owes no restart. Otherwise the marker keeps what is
      // still closed, against whichever version is installed now.
      if (mode === 'desktop' && !installed && closed) {
        try {
          const owed = await owedAfterStart(runtime, deps, prior, closed);
          if (!stillOwed(owed, await verifiedState(runtime, deps, owed))) { deps.clear(); nothingOwed = true; }
          else if (output.version && output.version !== output.previousVersion) deps.mark(markerFields(owed));
        } catch { /* The marker stays: the next click checks again. */ }
      }
    }
    // A stop() that failed partway owes only what it really retired and did not bring back. A planned
    // process it never reached, such as a proxy-only desktop still running, is not owed.
    if (stopFailed) {
      const nothing = { daemon: false };
      try {
        // Each desktop stop() closed and its recovery did not relaunch and see come up stays owed.
        const retired = await runtimeRetired(runtime, deps);
        const earlier = intentWritten ? earlierAtStop : deps.markerSnapshot();
        // The readiness check can take about 40 seconds and a parent timeout can kill this helper
        // during it, so what is owed goes in the marker first, and comes out if the daemon passes.
        if (retired.daemon) deps.mark(markerFields(addObligations(prior, retired)));
        // A replacement that fails the readiness check is not back, or the marker would name nothing.
        const live = await verifiedState(runtime, deps, retired);
        const lost = { daemon: retired.daemon && !live.daemon, desktops: retired.desktops };
        if (stillOwed(lost, nothing)) deps.mark(markerFields(addObligations(prior, lost)));
        else if (retired.daemon || intentWritten) {
          // Exactly as it was: its own version and target, so it applies where it did before.
          if (earlier === null) deps.clear();
          else deps.restoreMarker(earlier);
        }
      } catch {
        // A scan that fails can't say what came back. The plan is in memory, and the worst it costs is
        // one false restart_not_running, where naming nothing would let the next click call it current.
        try {
          const planned = addObligations(prior, runtimePlan(runtime, deps));
          if (stillOwed(planned, nothing)) deps.mark(markerFields(planned));
        } catch { /* The row below still says the restart failed. */ }
      }
    }
    if (busy) {
      // Nothing was stopped. The CLI package may already be new (the pending
      // marker stays, so the next click restarts the daemon once Codex is idle);
      // the desktop package was not touched.
      output.status = 'action_required'; output.messageCode = 'codex_busy';
    } else {
      output.status = !nothingOwed && (retried || output.version !== output.previousVersion || deps.pending(output.version)) ? 'restart_failed' : 'failed';
      output.messageCode = output.status === 'restart_failed' ? 'restart_failed' : 'update_failed';
    }
  } finally { await runtime?.dispose?.(); }
  return output;
}
// The bundle exports its own lock library; an installed package resolves it
// from its node_modules.
function loadRuntime(candidates = modules) {
  const selected = candidates.find((file) => fs.existsSync(file));
  if (!selected) return null;
  const { createCodexActivationRuntime, lockfile } = require(selected);
  return { selected, createCodexActivationRuntime, lockfile: lockfile ?? require('module').createRequire(selected)('proper-lockfile') };
}
/** Everything execute() needs on a real host, with the runtime wired through createUpdateRuntime. */
function bridgeDeps(createCodexActivationRuntime) {
  const initial = privateScan();
  const oldProxies = initial.filter((item) => item.args.includes('app-server') && item.args.includes('proxy') &&
    (item.exe === executable || item.exe.startsWith(path.join(home, '.codex/packages/standalone/releases') + path.sep)));
  const supervised = oldProxies.filter((item) => {
    const visited = new Set(); let parent = item.ppid;
    while (parent && !visited.has(parent)) {
      visited.add(parent); const process = initial.find((row) => row.pid === parent);
      if (!process) break;
      // OpenSSH's same-user session helper is deliberately non-dumpable.
      // /proc still supplies its kernel-owned argv0/ancestry; do not require
      // access to that helper's private executable/environment.
      if (/^sshd(?:-session)?(?::\s|$)/.test(path.basename(process.exe))) return true;
      parent = process.ppid;
    }
    return false;
  });
  return { now: Date.now, sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    readVersion: () => operation === 'desktop' ? run('/usr/bin/dpkg-query', ['-W', '-f=${Version}', 'chatgpt']) : run(executable, ['--version']),
    update: () => operation === 'desktop' ? run('/usr/bin/sudo', ['-n', '/usr/bin/apt-get', 'install', '--only-upgrade', '-y', 'chatgpt'], 180000) : run(executable, ['update'], 180000),
    pending: (expected) => markerMatches(readMarker(), expected),
    mark: markPending, clear: () => { try { fs.unlinkSync(markerFile()); } catch {} },
    restartProxies: async () => {
      if (operation !== 'cli') return;
      // The installed desktop SSH-WebSocket transport supports reconnect and
      // creates a new fixed `codex app-server proxy` whenever this one exits.
      for (const target of supervised) {
        if (privateScan().some((item) => item.pid === target.pid && item.startTime === target.startTime)) process.kill(target.pid, 'SIGTERM');
      }
      const limit = Date.now() + 5000;
      while (Date.now() < limit && supervised.some((target) => privateScan().some((item) => item.pid === target.pid && item.startTime === target.startTime))) await new Promise((resolve) => setTimeout(resolve, 100));
      for (const target of supervised) {
        if (privateScan().some((item) => item.pid === target.pid && item.startTime === target.startTime)) { process.kill(target.pid, 'SIGKILL'); output.forcedStops++; }
      }
    },
    hasOldProxies: async () => {
      if (operation !== 'cli') return false;
      const limit = Date.now() + 30000;
      do {
        const current = privateScan();
        const old = oldProxies.some((target) => current.some((item) => item.pid === target.pid && item.startTime === target.startTime));
        const fresh = current.filter((item) => item.exe === fs.realpathSync(executable) && item.args.includes('app-server') && item.args.includes('proxy'));
        if (!old && fresh.length >= supervised.length) { output.restartedProcesses += supervised.length; return false; }
        await new Promise((resolve) => setTimeout(resolve, 250));
      } while (Date.now() < limit);
      return true;
    },
    runtime: (hooks) => createUpdateRuntime(createCodexActivationRuntime, {}, hooks),
    marked: readMarker,
    markerSnapshot: () => markerSnapshot(),
    restoreMarker: (text) => restoreMarker(text),
    };
}
async function main() {
  const loaded = fs.existsSync(executable) ? loadRuntime() : null;
  if (!loaded) return output;
  const { createCodexActivationRuntime, lockfile } = loaded;
  let release;
  try {
    release = await lockfile.lock(path.join(home, '.codex'), { realpath: false,
      lockfilePath: path.join(home, '.codex/.ccs-activation.lock'), stale: 120000, update: 5000,
      retries: { retries: Math.max(1, Math.floor(Math.min(seconds, LOCK_WAIT_SECONDS) / 5)), factor: 1, minTimeout: 5000, maxTimeout: 5000 } });
  } catch {
    // An account switch holds the Codex lock; come back when it is done.
    output.status = 'action_required'; output.messageCode = 'codex_busy'; return output;
  }
  try {
    return await execute(bridgeDeps(createCodexActivationRuntime));
  } finally { await release(); }
}
module.exports = { bridgeDeps, cgroupUnit, createUpdateRuntime, execute, markerSnapshot, restoreMarker, insideServiceCgroup, loadRuntime, modules, output, relaunch, version };
if (require.main === module) main().catch(() => output).then(() => process.stdout.write(JSON.stringify(output) + '\n'));
