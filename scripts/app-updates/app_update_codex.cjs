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
// The whole version, a prerelease or build included: what decides whether an install changed anything,
// as the Python helper compares it. The numeric part (version()) is for the row and marker matching.
const fullVersion = (text) => typeof text === 'string' ? text.match(/\d+\.\d+(?:\.\d+){0,3}(?:[-+][0-9A-Za-z.+-]+)?/)?.[0] ?? null : null;
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
 * How many desktops an earlier Codex Desktop run closed and never saw back. Only AAC's own relaunch of a
 * desktop, seen coming up, clears one, and that run's launch snapshot is gone, so a retry reports them
 * once. An older marker listed desktops one entry each; one that names a desktop but no count is one.
 */
function desktopsOwed(marker, legacyDesktop) {
  const listed = Array.isArray(marker?.desktops) ? marker.desktops.filter((item) => typeof item === 'string').length
    : Number.isInteger(marker?.desktops) && marker.desktops > 0 ? marker.desktops : 0;
  return listed || (marker?.desktop === true || legacyDesktop ? 1 : 0);
}
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
 * A Codex Desktop run's marker: how many `desktops` it closed and has not seen back (`desktop` says
 * whether any), with no path or profile. `version` is what was installed when it was written, and
 * `target` the version a desktop install was about to put in place. The CLI keeps no marker.
 */
const markerMatches = (marker, installed) => Boolean(installed) && [marker?.version, marker?.target].some((item) => version(item) === installed);
function writeMarker(text, file = markerFile()) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const temporary = file + '.' + process.pid + '.tmp';
  fs.writeFileSync(temporary, text, { mode: 0o600 });
  fs.renameSync(temporary, file);
}
function markPending(closed = {}) { writeMarker(JSON.stringify({ version: output.version, ...closed })); }
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
 * One Codex update. The CLI (owner decision 2026-11-08): each click stands alone. It runs the updater
 * and, only if the installed version changed, restarts the daemon now and checks it came up; nothing is
 * carried to a later click, and an update that fails or is cut off reports failed with the version
 * installed now while the old daemon keeps running. Codex Desktop: the desktops a run closed are owed
 * until AAC's own relaunch of each is seen coming up, kept in the marker for the next click.
 */
async function execute(deps) {
  const idle = Math.max(1, Math.min(seconds, deps.idleSeconds ?? IDLE_WAIT_SECONDS));
  const deadline = deps.now() + idle * 1000;
  const mode = deps.operation ?? operation;
  const expected = deps.expectedCandidate ?? flags['--expected-version'];
  const restartOnly = deps.restartOnly ?? flags['--restart-only'] === 'true';
  let runtime, closed = null, stopped = false, installed = false, updating = false, busy = false, stopFailed = false;
  // Desktops an earlier desktop run left owed, and whether anything is owed after this one's failure.
  let prior = 0, nothingOwed = false;
  // The whole versions this run began from and has now, a prerelease included: what decides whether an
  // install changed anything.
  let fullPrevious = null, fullNow = null;
  const changed = () => fullNow !== fullPrevious;
  const target = mode === 'desktop' && !restartOnly ? version(expected) : null;
  // The desktop marker: desktops owed, or none.
  const record = (desktops, withTarget = false) => {
    if (mode !== 'desktop') return;
    if (desktops > 0) deps.mark({ desktop: true, desktops, ...(withTarget && target ? { target } : {}) });
    else deps.clear();
  };
  try {
    const first = await deps.readVersion();
    output.previousVersion = output.version = version(first);
    fullPrevious = fullNow = fullVersion(first);
    if (!output.version) { output.messageCode = 'version_unknown'; return output; }
    if (mode === 'desktop') {
      // A desktop marker from before these fields counts as one desktop when it is for this install.
      const marker = deps.marked();
      const unnamed = marker && marker.desktop === undefined && marker.daemon === undefined;
      prior = desktopsOwed(marker, Boolean(unnamed && (restartOnly || markerMatches(marker, output.previousVersion))));
    } else {
      output.updateAttempted = true;
      updating = true;
      await deps.update();
      updating = false;
      const read = await deps.readVersion();
      output.version = version(read);
      fullNow = fullVersion(read);
      if (!output.version) { output.messageCode = 'version_unknown'; return output; }
      if (expected && output.version !== version(expected)) throw new Error('Candidate not installed.');
      if (!changed()) { output.status = 'current'; output.messageCode = 'current'; return output; }
    }
    // A desktop run records what stop() is about to close under its lock, before it signals anything,
    // so a helper killed inside stop() still leaves it.
    runtime = deps.runtime(mode === 'desktop' ? { beforeStop: (plan) => record(prior + counted(plan).desktops, true) } : {});
    while (true) {
      try { await runtime.stop(); stopped = true; break; }
      catch (error) {
        if (error.code !== 'busy') { stopFailed = true; throw error; }
        if (deps.now() >= deadline) { busy = true; throw error; }
        await deps.sleep(Math.min(5000, deadline - deps.now()));
      }
    }
    closed = runtimePlan(runtime);
    record(prior + closed.desktops, true);
    if (mode === 'desktop') {
      output.updateAttempted = !restartOnly;
      updating = true;
      if (!restartOnly) await deps.update();
      updating = false;
      const read = await deps.readVersion();
      output.version = version(read);
      fullNow = fullVersion(read);
      if (!output.version || (!restartOnly && !changed()) || (expected && output.version !== version(expected))) throw new Error('Candidate not installed.');
      installed = true;
      record(prior + closed.desktops);
    }
    await deps.restartProxies?.();
    await runtime.start(); stopped = false;
    if (await deps.hasOldProxies()) throw new Error('An old proxy has no verified restart supervisor.');
    // The daemon must be ready on a scan taken now: start() has just retired any daemon a proxy
    // respawned too early, so while a live proxy could bring it back it gets a short while. A desktop
    // is back only once start() relaunched it and saw the process it started come up, so what an
    // earlier desktop run closed stays owed: a retry says so once and clears it. An older runtime
    // that can't be scanned rests on start()'s own checks.
    const left = typeof runtime.retiredPlan === 'function' ? (await runtimeRetired(runtime)).desktops : 0;
    const owed = { daemon: closed.daemon, desktops: prior + left };
    if (typeof runtime.restartState === 'function' || prior > 0) {
      const settleBy = deps.now() + (deps.settleMs ?? RESTART_SETTLE_MS);
      let live = await runtimeState(runtime);
      while (owed.daemon && !live.daemon && live.proxy && deps.now() < settleBy) {
        await deps.sleep(250);
        live = await runtimeState(runtime);
      }
      const daemonDown = owed.daemon && !(await verifiedState(runtime, deps, owed)).daemon;
      if (daemonDown || owed.desktops > 0) {
        if (prior > 0) { record(0); output.messageCode = 'restart_not_running'; }
        else { record(left); output.messageCode = 'restart_failed'; }
        output.status = 'restart_failed';
        return output;
      }
    }
    record(0); output.status = 'updated'; output.messageCode = 'updated';
  } catch {
    // An installer that failed may still have replaced the app: the row names what is installed now.
    if (updating) {
      try {
        const read = await deps.readVersion();
        if (version(read)) { output.version = version(read); fullNow = fullVersion(read); }
      } catch { /* The version stays as it was read before the install. */ }
    }
    if (stopped) {
      try { await runtime.start(); } catch { /* Reported below. */ }
      // A desktop install that failed owes no restart once everything is back.
      if (mode === 'desktop' && !installed && closed) {
        try {
          const left = (await runtimeRetired(runtime)).desktops;
          const daemonDown = closed.daemon && !(await verifiedState(runtime, deps, closed)).daemon;
          if (!daemonDown && prior + left === 0) { record(0); nothingOwed = true; }
          else record(prior + left, true);
        } catch { /* The marker stays: the next click checks again. */ }
      }
    }
    // A desktop stop() that failed partway owes the desktops it closed and did not bring back.
    if (stopFailed && mode === 'desktop') {
      try { record(prior + (await runtimeRetired(runtime)).desktops, true); }
      catch { try { record(prior + runtimePlan(runtime).desktops, true); } catch { /* The row says the restart failed. */ } }
    }
    if (busy) {
      // Nothing was stopped. A CLI package may already be new; its daemon restarts on a later click.
      output.status = 'action_required'; output.messageCode = 'codex_busy';
    } else if (mode === 'cli') {
      // Installed but not restarted, or not installed: either way the old daemon keeps running.
      const restartPhase = changed() && (stopFailed || stopped || closed !== null);
      output.status = restartPhase ? 'restart_failed' : 'failed';
      output.messageCode = restartPhase ? 'restart_failed' : 'update_failed';
    } else {
      output.status = !nothingOwed && (prior > 0 || changed() || deps.pending(output.version)) ? 'restart_failed' : 'failed';
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
module.exports = { bridgeDeps, cgroupUnit, createUpdateRuntime, execute, insideServiceCgroup, loadRuntime, modules, output, relaunch, version };
if (require.main === module) main().catch(() => output).then(() => process.stdout.write(JSON.stringify(output) + '\n'));
