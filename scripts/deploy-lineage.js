#!/usr/bin/env node
/**
 * Deployment lineage. A build records every commit it contains in `dist/build-info.json` and
 * whether its commit is on origin/main; the installed dashboard records the build it runs in
 * `<CCS dir>/deployed-build.json`. Installation refuses a package that is not on origin/main,
 * has uncommitted changes, or does not contain that running build.
 *
 * Fixes installed from unmerged branches were removed by the next package built from main:
 * the Oct 16 usage deadlines by the Oct 20 Muse install, and the Oct 20 Muse fallback by the
 * Oct 21 updater install. A failed postinstall makes npm keep the previous installation.
 * `AAC_ALLOW_DEPLOY_REGRESSION=1` permits an intentional rollback or emergency install.
 *
 * Usage: node scripts/deploy-lineage.js record-build   (writes dist/build-info.json)
 *        node scripts/deploy-lineage.js preflight      (checkout committed and on origin/main)
 *        node scripts/deploy-lineage.js check          (checks this checkout's dist/ build)
 */
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const BUILD_INFO = 'build-info.json';
const LEDGER = 'deployed-build.json';
const OVERRIDE = 'AAC_ALLOW_DEPLOY_REGRESSION';
const COMMIT = /^[a-f0-9]{40}$/;
const MAX_LINEAGE = 100000;
const MAX_FILE_BYTES = 8 * 1024 * 1024;
// Inputs that compile into or ship with the package; other untracked files do not.
const PACKAGE_INPUTS = [
  'src',
  'scripts',
  'lib',
  'config',
  'web-dashboard',
  'browser-bridge',
  'macos-bar',
  'windows-bar',
  'package.json',
  'bun.lock',
  'tsconfig.json',
];

function git(repoRoot, args) {
  const result = spawnSync('git', ['-C', repoRoot, ...args], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'ignore'],
    timeout: 30000,
    windowsHide: true,
    maxBuffer: 16 * 1024 * 1024,
  });
  return result.status === 0 ? String(result.stdout) : null;
}

function safeBranch(value) {
  return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$/.test(value)
    ? value
    : null;
}

function short(commit) {
  return commit.slice(0, 8);
}

function containedIn(repoRoot, commit, ref) {
  return (
    spawnSync('git', ['-C', repoRoot, 'merge-base', '--is-ancestor', commit, ref], {
      stdio: 'ignore',
      timeout: 30000,
      windowsHide: true,
    }).status === 0
  );
}

/** Whether origin/main contains the commit, refreshing a stale origin/main ref once. */
function isOnMain(repoRoot, commit) {
  const ref = 'refs/remotes/origin/main';
  if (containedIn(repoRoot, commit, ref)) return true;
  git(repoRoot, ['fetch', '--quiet', 'origin', 'main']);
  return containedIn(repoRoot, commit, ref);
}

/** The commits a checkout contains, or null outside the top of a git checkout. */
function readSourceBuild(repoRoot, now = new Date()) {
  const top = git(repoRoot, ['rev-parse', '--show-toplevel']);
  try {
    if (!top || fs.realpathSync(top.trim()) !== fs.realpathSync(repoRoot)) return null;
  } catch {
    return null;
  }
  const commit = (git(repoRoot, ['rev-parse', 'HEAD']) ?? '').trim();
  const lineage = (git(repoRoot, ['rev-list', `--max-count=${MAX_LINEAGE}`, 'HEAD']) ?? '')
    .split(/\r?\n/)
    .filter((line) => COMMIT.test(line));
  if (!COMMIT.test(commit) || lineage[0] !== commit) return null;
  const inputs = PACKAGE_INPUTS.filter((input) => fs.existsSync(path.join(repoRoot, input)));
  const status = git(repoRoot, [
    'status',
    '--porcelain',
    '--untracked-files=normal',
    '--',
    ...inputs,
  ]);
  if (status === null) return null;
  const branch = (git(repoRoot, ['rev-parse', '--abbrev-ref', 'HEAD']) ?? '').trim();
  return {
    version: 1,
    commit,
    branch: branch === 'HEAD' ? null : safeBranch(branch),
    dirty: status.trim() !== '',
    onMain: isOnMain(repoRoot, commit),
    builtAt: now.toISOString(),
    lineage,
  };
}

function recordBuild(repoRoot, outDir = path.join(repoRoot, 'dist')) {
  const info = readSourceBuild(repoRoot) ?? {
    version: 1,
    commit: null,
    branch: null,
    dirty: null,
    onMain: null,
    builtAt: new Date().toISOString(),
    lineage: [],
  };
  fs.mkdirSync(outDir, { recursive: true });
  fs.writeFileSync(path.join(outDir, BUILD_INFO), `${JSON.stringify(info)}\n`, { mode: 0o644 });
  return info;
}

function readJson(file) {
  try {
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.size > MAX_FILE_BYTES) return null;
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch {
    return null;
  }
}

function parseBuildInfo(value) {
  if (!value || typeof value !== 'object' || value.version !== 1) return null;
  if (value.commit === null)
    return { commit: null, branch: null, dirty: null, onMain: null, lineage: [] };
  if (
    !COMMIT.test(value.commit) ||
    typeof value.dirty !== 'boolean' ||
    !Array.isArray(value.lineage) ||
    value.lineage[0] !== value.commit ||
    !value.lineage.every((commit) => typeof commit === 'string' && COMMIT.test(commit))
  )
    return null;
  return {
    commit: value.commit,
    branch: safeBranch(value.branch),
    dirty: value.dirty,
    onMain: value.onMain === true,
    lineage: value.lineage,
  };
}

function parseLedger(value) {
  if (!value || typeof value !== 'object' || value.version !== 1 || !COMMIT.test(value.commit))
    return null;
  return { commit: value.commit, branch: safeBranch(value.branch), dirty: value.dirty === true };
}

function ccsDirFrom(env = process.env) {
  return env.CCS_DIR
    ? path.resolve(env.CCS_DIR)
    : path.join(path.resolve(env.CCS_HOME || os.homedir()), '.ccs');
}

/** A git checkout runs its own build; only installed packages are guarded and recorded. */
function isSourceCheckout(packageRoot) {
  return (
    fs.existsSync(path.join(packageRoot, '.git')) ||
    fs.existsSync(path.join(packageRoot, 'src', 'ccs.ts'))
  );
}

function describe(commit, branch) {
  return branch ? `${short(commit)} (${branch})` : short(commit);
}

/**
 * Whether the package at `packageRoot` is a committed build of origin/main that keeps the
 * build this computer's dashboard runs. Returns `{ ok, message }`; ok=true with a message is
 * a notice.
 */
function checkDeployLineage({ packageRoot, env = process.env }) {
  const info = parseBuildInfo(readJson(path.join(packageRoot, 'dist', BUILD_INFO)));
  const ledger = parseLedger(readJson(path.join(ccsDirFrom(env), LEDGER)));
  return verdict(buildProblem(info) ?? runningBuildProblem(info, ledger), env);
}

/** Whether the package keeps the build this computer's dashboard runs. */
function runningBuildProblem(info, ledger) {
  if (!ledger) return null;
  const running = describe(ledger.commit, ledger.branch);
  if (ledger.dirty)
    return (
      `The dashboard on this computer runs ${running} with uncommitted changes, which this ` +
      'package cannot prove it keeps.'
    );
  if (!info?.commit)
    return (
      'This package does not record the commits it was built from, so it cannot prove it keeps ' +
      `${running}, which the dashboard on this computer runs. Build it from a git checkout.`
    );
  if (!info.lineage.includes(ledger.commit))
    return (
      `This package (${describe(info.commit, info.branch)}) does not contain ${running}, which ` +
      'the dashboard on this computer runs. Installing it would remove deployed fixes. ' +
      `Merge ${ledger.branch ?? short(ledger.commit)} into main, rebuild from main and install ` +
      'that package.'
    );
  return null;
}

/** A build's own problem: uncommitted changes, or a commit that origin/main does not contain. */
function buildProblem(info) {
  if (info?.dirty === true)
    return (
      `This package was built from ${describe(info.commit, info.branch)} with uncommitted ` +
      'changes. A later build could not keep them. Commit, merge into main and rebuild.'
    );
  if (info?.commit && !info.onMain)
    return (
      `This package was built from ${describe(info.commit, info.branch)}, which is not on ` +
      'origin/main. A fix installed before it is merged is removed by the next build of main. ' +
      'Merge it into main, push, then rebuild and install.'
    );
  return null;
}

function verdict(problem, env) {
  const override = env[OVERRIDE] === '1';
  if (!problem) return { ok: true, message: null };
  if (override) return { ok: true, message: `${problem} Installing anyway because ${OVERRIDE}=1.` };
  return {
    ok: false,
    message: `${problem} Only for an intentional rollback or emergency, set ${OVERRIDE}=1.`,
  };
}

module.exports = {
  BUILD_INFO,
  LEDGER,
  OVERRIDE,
  ccsDirFrom,
  buildProblem,
  checkDeployLineage,
  isSourceCheckout,
  parseBuildInfo,
  parseLedger,
  readSourceBuild,
  recordBuild,
};

if (require.main === module) {
  const repoRoot = path.resolve(__dirname, '..');
  const command = process.argv[2];
  if (command === 'record-build') {
    const info = recordBuild(repoRoot);
    if (info.commit) {
      console.log(
        `[OK] Build lineage: ${describe(info.commit, info.branch)}, ${info.lineage.length} commits` +
          (info.onMain
            ? ', on origin/main'
            : ', NOT on origin/main (installation will refuse it)') +
          (info.dirty ? ', with uncommitted changes (installation will refuse it)' : '')
      );
    } else {
      console.log('[!] Build lineage: not a git checkout; installation cannot verify this build.');
    }
  } else if (command === 'preflight') {
    // Before a long build: the checkout itself must be committed and on origin/main.
    const info = readSourceBuild(repoRoot);
    const result = verdict(
      info ? buildProblem(info) : 'The checkout commits could not be read with git.',
      process.env
    );
    if (result.message)
      console[result.ok ? 'log' : 'error'](`[${result.ok ? '!' : 'X'}] ${result.message}`);
    process.exitCode = result.ok ? 0 : 1;
  } else if (command === 'check') {
    const result = checkDeployLineage({ packageRoot: repoRoot });
    if (result.message)
      console[result.ok ? 'log' : 'error'](`[${result.ok ? '!' : 'X'}] ${result.message}`);
    process.exitCode = result.ok ? 0 : 1;
  } else {
    console.error('Usage: node scripts/deploy-lineage.js record-build|preflight|check');
    process.exitCode = 2;
  }
}
