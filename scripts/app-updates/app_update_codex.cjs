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
// Set only for an npm-global Codex (app_updates.npm_bridge_arguments passes them): npm itself then updates it under its own prefix.
const npmFlags = flags['--npm-prefix'] ? { node: flags['--npm-node'], cli: flags['--npm-cli'], prefix: flags['--npm-prefix'] } : null;
// Shared Codex work is often busy for hours (long goals, other projects'
// stdio servers). Waiting longer than this for idle only stalls Update apps;
// a busy Codex becomes an honest "Codex is busy" row instead.
const IDLE_WAIT_SECONDS = 60;
const LOCK_WAIT_SECONDS = 30;
const output = { appId: operation === 'desktop' ? 'codex-desktop' : 'codex-cli', platform: 'ubuntu',
  status: 'failed', previousVersion: null, version: null, manager: operation === 'desktop' ? 'apt' : npmFlags ? 'npm' : 'native',
  messageCode: 'update_failed', updateAttempted: false, restartedProcesses: 0, forcedStops: 0 };
const version = (text) => typeof text === 'string' ? text.match(/\d+\.\d+(?:\.\d+){0,3}/)?.[0] ?? null : null;
function run(binary, args, timeout = 10000) {
  return new Promise((resolve, reject) => execFile(binary, args, { timeout, maxBuffer: 65536, encoding: 'utf8',
    env: { ...process.env, CODEX_NON_INTERACTIVE: '1', NO_UPDATE_NOTIFIER: '1', DEBIAN_FRONTEND: 'noninteractive' } },
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
// The command that updates the CLI at `target`. An npm-global install is updated by npm itself, under the prefix
// that must own `target`: `codex update` would run whichever npm PATH finds. Anything else runs `codex update`.
function updateCommand(target = executable, npm = npmFlags) {
  if (!npm) return { file: target, args: ['update'] };
  if (![npm.node, npm.cli, npm.prefix].every((value) => typeof value === 'string' && path.isAbsolute(value))) throw new Error('npm install is unknown.');
  const packageRoot = fs.realpathSync(path.join(npm.prefix, 'lib/node_modules/@openai/codex'));
  if (!fs.realpathSync(target).startsWith(packageRoot + path.sep)) throw new Error('npm install does not own this Codex.');
  return { file: npm.node, args: [npm.cli, 'install', '--global', '--prefix', npm.prefix, '@openai/codex@latest'] };
}
function cliUpdate() {
  const { file, args } = updateCommand();
  return run(file, args, 180000);
}
function markerFile() { return path.join(home, '.ccs/app-updates', output.appId + '-pending-restart.json'); }
function markPending() {
  fs.mkdirSync(path.dirname(markerFile()), { recursive: true, mode: 0o700 });
  const temporary = markerFile() + '.' + process.pid + '.tmp';
  fs.writeFileSync(temporary, JSON.stringify({ version: output.version }), { mode: 0o600 });
  fs.renameSync(temporary, markerFile());
}
async function execute(deps) {
  const idle = Math.max(1, Math.min(seconds, deps.idleSeconds ?? IDLE_WAIT_SECONDS));
  const deadline = deps.now() + idle * 1000;
  let runtime, stopped = false, busy = false;
  try {
    output.previousVersion = output.version = version(await deps.readVersion());
    if (!output.version) { output.messageCode = 'version_unknown'; return output; }
    if (operation === 'cli') {
      output.updateAttempted = true;
      await deps.update();
      output.version = version(await deps.readVersion());
      if (!output.version) { output.messageCode = 'version_unknown'; return output; }
      if (output.version === output.previousVersion && !deps.pending(output.version)) {
        output.status = 'current'; output.messageCode = 'current'; return output;
      }
      deps.mark();
    }
    runtime = deps.runtime();
    while (true) {
      try { await runtime.stop(); stopped = true; break; }
      catch (error) {
        if (error.code !== 'busy') throw error;
        if (deps.now() >= deadline) { busy = true; throw error; }
        await deps.sleep(Math.min(5000, deadline - deps.now()));
      }
    }
    if (operation === 'desktop') {
      output.updateAttempted = true;
      await deps.update();
      output.version = version(await deps.readVersion());
      if (!output.version) throw new Error('Version unknown.');
      deps.mark();
    }
    await deps.restartProxies?.();
    await runtime.start(); stopped = false;
    if (await deps.hasOldProxies()) throw new Error('An old proxy has no verified restart supervisor.');
    deps.clear(); output.status = 'updated'; output.messageCode = 'updated';
  } catch {
    if (stopped) { try { await runtime.start(); } catch { /* A later explicit click retries using the private marker. */ } }
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
  return await execute({ now: Date.now, sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    readVersion: () => operation === 'desktop' ? run('/usr/bin/dpkg-query', ['-W', '-f=${Version}', 'chatgpt']) : run(executable, ['--version']),
    update: () => operation === 'desktop' ? run('/usr/bin/sudo', ['-n', '/usr/bin/apt-get', 'install', '--only-upgrade', '-y', 'chatgpt'], 180000) : cliUpdate(),
    pending: (expected) => { try { return JSON.parse(fs.readFileSync(markerFile(), 'utf8')).version === expected; } catch { return false; } },
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
    runtime: () => createCodexActivationRuntime(path.join(home, '.codex'), { scan: async () => privateScan(),
      launch: async (target, desktop) => {
        const actual = desktop ? target.exe : executable;
        const child = spawn(actual, target.args.slice(1), { cwd: target.cwd, env: target.env, detached: true,
          argv0: desktop ? target.args[0] : executable, stdio: ['ignore', 'ignore', 'ignore'] });
        await new Promise((resolve, reject) => { child.once('spawn', resolve); child.once('error', () => reject(new Error('Launch failed.'))); });
        child.unref(); output.restartedProcesses++;
      },
    }),
  });
  } finally { await release(); }
}
module.exports = { execute, loadRuntime, modules, output, version, updateCommand };
if (require.main === module) main().catch(() => output).then(() => process.stdout.write(JSON.stringify(output) + '\n'));
