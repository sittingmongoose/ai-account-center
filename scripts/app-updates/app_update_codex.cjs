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
    await outside({ ...target, exe: actual, env }, desktop);
  } else {
    const child = (deps.spawn ?? spawn)(actual, target.args.slice(1), { cwd: target.cwd, env, detached: true,
      argv0: desktop ? target.args[0] : (deps.executable ?? executable), stdio: ['ignore', 'ignore', 'ignore'] });
    await new Promise((resolve, reject) => { child.once('spawn', resolve); child.once('error', () => reject(new Error('Launch failed.'))); });
    child.unref();
  }
  output.restartedProcesses++;
}
/** The update bridge's process runtime: its launches stay outside the dashboard cgroup. */
function createUpdateRuntime(createRuntime, deps = {}) {
  return createRuntime(path.join(home, '.codex'), { scan: async () => privateScan(),
    launch: (target, desktop, outside) => relaunch(target, desktop, outside, deps) });
}
/** A Codex app server on this home. Proxies don't count, nor do servers on another home (T3's). */
function serverOnHome(item, codexHome) {
  const index = item.args.indexOf('app-server');
  if (path.basename(item.exe) !== 'codex' || index < 0 || item.args[index + 1] === 'proxy') return false;
  return path.resolve(item.env.CODEX_HOME || path.join(item.env.HOME || home, '.codex')) === path.resolve(codexHome);
}
/** The shared daemon on this home; a desktop's own bundled server is not it. */
function daemonRunning(processes, codexHome = path.join(home, '.codex')) {
  return processes.some((item) => serverOnHome(item, codexHome) && item.args[item.args.indexOf('--listen') + 1]?.startsWith('unix://') === true);
}
/** start()'s own test: a Codex Desktop main process with a live app server on this home under it. */
function desktopReady(processes, codexHome = path.join(home, '.codex')) {
  return processes.some((main) => {
    if (main.exe !== '/usr/lib/chatgpt/ChatGPT' || main.args.join(' ').includes('--type=')) return false;
    const under = new Set([main.pid]);
    for (let grew = true; grew;) {
      grew = false;
      for (const item of processes) if (under.has(item.ppid) && !under.has(item.pid)) { under.add(item.pid); grew = true; }
    }
    return processes.some((item) => item.pid !== main.pid && under.has(item.pid) && serverOnHome(item, codexHome));
  });
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
        env, cwd: relevant ? fs.readlinkSync(path.join(directory, 'cwd')) : '' });
    } catch (error) { if (!['ENOENT', 'ESRCH'].includes(error.code)) throw new Error('Codex inventory failed.'); }
  }
  return values;
}
function markerFile() { return path.join(home, '.ccs/app-updates', output.appId + '-pending-restart.json'); }
function readMarker() { try { return JSON.parse(fs.readFileSync(markerFile(), 'utf8')); } catch { return null; } }
/**
 * `closed`: the `desktop` and `daemon` a run's stop() has closed, which a retry must see running
 * again. A marker without them, written before stop(), says only that a restart is owed.
 */
function markPending(closed = {}) {
  fs.mkdirSync(path.dirname(markerFile()), { recursive: true, mode: 0o700 });
  const temporary = markerFile() + '.' + process.pid + '.tmp';
  fs.writeFileSync(temporary, JSON.stringify({ version: output.version, ...closed }), { mode: 0o600 });
  fs.renameSync(temporary, markerFile());
}
async function execute(deps) {
  const idle = Math.max(1, Math.min(seconds, deps.idleSeconds ?? IDLE_WAIT_SECONDS));
  const deadline = deps.now() + idle * 1000;
  const mode = deps.operation ?? operation;
  const expected = deps.expectedCandidate ?? flags['--expected-version'];
  const restartOnly = deps.restartOnly ?? flags['--restart-only'] === 'true';
  let runtime, stopped = false, busy = false;
  try {
    output.previousVersion = output.version = version(await deps.readVersion());
    if (!output.version) { output.messageCode = 'version_unknown'; return output; }
    // A retry restarts only what its own stop() finds, because the launch snapshot of a failed
    // earlier run is gone. The marker says what that run closed, and only a marker that names
    // something is a retry: a run stopped by a busy Codex, or a marker the Python path wrote,
    // closed nothing. A desktop marker from before these fields counts as a closed desktop,
    // because the old bridge wrote those only after stopping.
    const marker = deps.marked();
    const current = restartOnly || marker?.version === output.previousVersion;
    const unnamed = marker && marker.desktop === undefined && marker.daemon === undefined;
    const prior = {
      desktop: Boolean(current && (marker?.desktop === true || (mode === 'desktop' && unnamed))),
      daemon: Boolean(current && marker?.daemon === true),
    };
    const retried = prior.desktop || prior.daemon;
    if (mode === 'cli') {
      output.updateAttempted = true;
      await deps.update();
      output.version = version(await deps.readVersion());
      if (!output.version) { output.messageCode = 'version_unknown'; return output; }
      if (expected && output.version !== version(expected)) throw new Error('Candidate not installed.');
      if (output.version === output.previousVersion && !deps.pending(output.version)) {
        output.status = 'current'; output.messageCode = 'current'; return output;
      }
      deps.mark(retried ? prior : undefined);
    }
    runtime = deps.runtime();
    let seen;
    while (true) {
      try { seen = await deps.running(); await runtime.stop(); stopped = true; break; }
      catch (error) {
        if (error.code !== 'busy') throw error;
        if (deps.now() >= deadline) { busy = true; throw error; }
        await deps.sleep(Math.min(5000, deadline - deps.now()));
      }
    }
    const closed = { desktop: prior.desktop || seen.desktop, daemon: prior.daemon || seen.daemon };
    if (mode === 'cli') deps.mark(closed);
    if (mode === 'desktop') {
      output.updateAttempted = !restartOnly;
      if (!restartOnly) await deps.update();
      output.version = version(await deps.readVersion());
      if (!output.version || (!restartOnly && output.version === output.previousVersion) || (expected && output.version !== version(expected))) throw new Error('Candidate not installed.');
      deps.mark(closed);
    }
    await deps.restartProxies?.();
    await runtime.start(); stopped = false;
    if (await deps.hasOldProxies()) throw new Error('An old proxy has no verified restart supervisor.');
    // Leaving closed what an earlier run closed is not a restart: say so once. The marker goes too,
    // since nothing here could ever finish it.
    if (retried) {
      const live = await deps.running();
      if ((closed.desktop && !live.desktop) || (closed.daemon && !live.daemon)) {
        deps.clear(); output.status = 'restart_failed'; output.messageCode = 'restart_not_running';
        return output;
      }
    }
    deps.clear(); output.status = 'updated'; output.messageCode = 'updated';
  } catch {
    // start() does nothing once it has released the lock, so a launch that failed there is not
    // retried here. The marker stays, and the next click reports what it finds (restart_not_running):
    // the updaters send a matching marker here even with no daemon running.
    if (stopped) { try { await runtime.start(); } catch { /* Reported below as restart_failed. */ } }
    if (busy) {
      // Nothing was stopped. The CLI package may already be new (the pending
      // marker stays, so the next click restarts the daemon once Codex is idle);
      // the desktop package was not touched.
      output.status = 'action_required'; output.messageCode = 'codex_busy';
    } else {
      output.status = output.version !== output.previousVersion || deps.pending(output.version) ? 'restart_failed' : 'failed';
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
    pending: (expected) => readMarker()?.version === expected,
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
    runtime: () => createUpdateRuntime(createCodexActivationRuntime),
    marked: readMarker,
    running: () => { const processes = privateScan(); return { desktop: desktopReady(processes), daemon: daemonRunning(processes) }; },
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
module.exports = { bridgeDeps, createUpdateRuntime, daemonRunning, desktopReady, execute, insideServiceCgroup, loadRuntime, modules, output, relaunch, version };
if (require.main === module) main().catch(() => output).then(() => process.stdout.write(JSON.stringify(output) + '\n'));
