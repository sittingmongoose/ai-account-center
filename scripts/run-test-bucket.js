#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const rootDir = path.resolve(__dirname, '..');
const candidateRoots = ['tests/unit', 'tests/integration', 'tests/npm', 'src'];
// Add a `.ts` test to `slowTests` when ANY of these apply:
//   1. It spawns a child process (CLI, bun test, node, gh, etc.).
//   2. It binds a port, starts a server, or talks to localhost.
//   3. It reads a real file from `dist/` or the repo root at runtime.
//   4. It waits on a timer > 500ms or a filesystem watcher.
//   5. A single run consistently takes > 1500ms on reference hardware.
// Tests that literally reference `dist/` in source are auto-forced slow by
// `readsBuiltDist`. This list is the manual catch-all for `.ts` tests that
// meet the criteria above without the literal `dist/` string.
// `tests/unit/scripts/run-test-bucket.test.js` verifies every path here exists
// (catches deletion drift) but CANNOT detect new undeclared slow tests.
// Automated perf-budget enforcement tracked in issue #1071.
const slowTests = [
  'tests/integration/logging-request-context.test.ts',
  'tests/integration/web-server/codex-profiles-endpoint.test.ts',
  'tests/unit/docker/dockerfile-lifecycle-scripts.test.ts',
  'tests/unit/web-server/muse-failure-contract-interop.test.ts',
  'tests/unit/web-server/claude-host-transport.test.ts',
  'tests/unit/web-server/signin-process.test.ts',
  'tests/unit/antigravity/signin-driver.test.ts',
  'tests/unit/web-server/dashboard-auth-runtime.test.ts',
  'tests/unit/web-server/analytics-remote-helper.test.ts',
  'tests/unit/web-server/antigravity-usage-helper.test.ts',
  'tests/unit/web-server/app-update-helper-sync.test.ts',
  'tests/unit/web-server/app-update-nas1.test.ts',
  'tests/unit/app-updates/codex-update-runtime-bundle.test.ts',
];
// CommonJS-heavy JS suites stay slow by default because many of them mutate
// module cache or process state. Opt them into `test:fast` only after they are
// proven stable in the mixed fast bucket.
const fastJsTests = new Set(['tests/unit/flag-parsing-simple.test.js']);

const isolatedTests = new Set([
  'tests/unit/commands/bar-command.test.ts',
  'tests/unit/utils/fetch-proxy-setup.test.ts',
  'tests/unit/web-server/usage/account-attribution.test.ts',
]);

const filePattern = /(\.test\.(c|m)?[jt]s|\.spec\.(c|m)?[jt]s|-test\.(c|m)?[jt]s)$/;

function collectFiles(dir, files = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      collectFiles(fullPath, files);
      continue;
    }

    if (filePattern.test(entry.name)) {
      files.push(path.relative(rootDir, fullPath).split(path.sep).join('/'));
    }
  }

  return files;
}

function readsBuiltDist(relativePath) {
  const source = fs.readFileSync(path.join(rootDir, relativePath), 'utf8');
  return source.includes('dist/');
}

function getDiscoveredTests() {
  return candidateRoots
    .flatMap((relativeDir) => collectFiles(path.join(rootDir, relativeDir)))
    .sort();
}

function shouldForceSlow(file) {
  if (file.startsWith('tests/npm/')) {
    return true;
  }

  if (/\.(c|m)?js$/.test(file) && !fastJsTests.has(file)) {
    return true;
  }

  return readsBuiltDist(file);
}

function usesBunTestRunner(relativePath) {
  const source = fs.readFileSync(path.join(rootDir, relativePath), 'utf8');
  return source.includes('bun:test') || /(^|[^\w.])(?:describe|it)\s*\(/m.test(source);
}

function getSlowSet() {
  const discovered = getDiscoveredTests();
  const forceSlow = discovered.filter((file) => shouldForceSlow(file));
  return new Set([...slowTests, ...forceSlow]);
}

function selectBucket(name) {
  const discovered = getDiscoveredTests();
  const slowSet = getSlowSet();

  return name === 'slow' ? [...slowSet].sort() : discovered.filter((file) => !slowSet.has(file));
}

function toBunTestPath(relativePath) {
  if (
    relativePath.startsWith('./') ||
    relativePath.startsWith('../') ||
    path.isAbsolute(relativePath)
  ) {
    return relativePath;
  }

  return `./${relativePath}`;
}

function getBunArgs(name, selected = selectBucket(name)) {
  const testPaths = selected.map(toBunTestPath);

  // Slow bucket forces sequential execution because it spawns subprocesses,
  // binds ports, and touches shared state — parallelism causes flakes.
  // Fast bucket keeps bun's default parallelism for speed.
  return name === 'slow' ? ['test', '--max-concurrency=1', ...testPaths] : ['test', ...testPaths];
}

function shouldRunIsolated(file) {
  return file.startsWith('src/') || isolatedTests.has(file) || !usesBunTestRunner(file);
}

function getBunRuns(name, selected = selectBucket(name)) {
  const shared = selected.filter((file) => !shouldRunIsolated(file));
  const isolated = selected.filter(shouldRunIsolated);
  const runs = [];

  if (shared.length > 0) {
    runs.push({
      label: 'shared',
      selected: shared,
      bunArgs: getBunArgs(name, shared),
      quietOnPass: false,
    });
  }

  for (const file of isolated) {
    runs.push({
      label: file,
      selected: [file],
      bunArgs: getBunArgs(name, [file]),
      quietOnPass: true,
    });
  }

  return runs;
}

function stripAnsi(value) {
  return value.replace(/\x1b\[[0-9;]*m/g, '');
}

function parseBunFileCount(output) {
  const match = stripAnsi(output).match(/Ran\s+\d+\s+tests?\s+across\s+(\d+)\s+files?/i);
  return match ? Number(match[1]) : null;
}

function verifyReportedFileCount(selectedCount, output) {
  const reportedCount = parseBunFileCount(output);

  if (reportedCount === null) {
    return {
      ok: false,
      message: '[X] Could not find Bun test file count in output.',
      reportedCount,
      selectedCount,
    };
  }

  if (reportedCount !== selectedCount) {
    return {
      ok: false,
      message:
        `[X] Bun ran ${reportedCount} files, but the bucket selected ${selectedCount} files. ` +
        'Check test path arguments for filter/path ambiguity.',
      reportedCount,
      selectedCount,
    };
  }

  return {
    ok: true,
    reportedCount,
    selectedCount,
  };
}

function shouldVerifyRunFileCount(run) {
  return run.selected.every((file) => usesBunTestRunner(file));
}

function ensureBuildForSlowBucket() {
  if (fs.existsSync(path.join(rootDir, 'dist', 'ccs.js'))) {
    return 0;
  }

  const build = spawnSync('bun', ['run', 'build'], {
    cwd: rootDir,
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });

  return build.status ?? 1;
}

// Write `text` and wait until Node has handed all of it to the operating system.
// On Linux, Node writes to a pipe (which is what CI gives us) asynchronously: one
// write call gets at most the pipe's 64 KB into the kernel and queues the rest.
// The queue only drains while the event loop runs, and `spawnSync` for the next
// Bun run blocks it, so every run's output is flushed before the next one starts.
function writeAndFlush(stream, text) {
  if (!text) {
    return Promise.resolve();
  }

  return new Promise((resolve) => {
    stream.write(text, () => resolve());
  });
}

async function writeRunOutput(result) {
  await writeAndFlush(process.stdout, result.stdout);
  await writeAndFlush(process.stderr, result.stderr);
}

// Bun prints each failed test twice (where it ran and in its closing recap); list it once.
function listFailedTests(output) {
  const lines = stripAnsi(output)
    .split(/\r?\n/)
    .filter((line) => line.startsWith('(fail) '));
  return [...new Set(lines)];
}

// Describe why a Bun run failed, in one line. A run that a signal stopped has a
// null exit status; say so instead of reporting a bare exit code 1.
function describeRunFailure(run, result, exitCode) {
  const what =
    run.selected.length === 1 ? run.selected[0] : `${run.label} (${run.selected.length} files)`;

  if (result.error) {
    return `[X] Bun run '${what}' could not run: ${result.error.message}`;
  }

  if (result.signal) {
    return `[X] Bun run '${what}' was stopped by signal ${result.signal} before it finished.`;
  }

  return `[X] Bun run '${what}' failed with exit code ${exitCode}.`;
}

async function runBunTest(run, deps = {}) {
  const spawn = deps.spawnSync ?? spawnSync;
  const report = deps.report ?? (() => {});
  const result = spawn('bun', run.bunArgs, {
    cwd: rootDir,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    shell: process.platform === 'win32',
  });

  const output = `${result.stdout ?? ''}\n${result.stderr ?? ''}`;
  const fail = async (exitCode, message = describeRunFailure(run, result, exitCode)) => {
    await writeRunOutput(result);
    await writeAndFlush(process.stderr, `${message}\n`);
    report({ summary: message, failedTests: listFailedTests(output) });
    return exitCode;
  };

  if (result.error) {
    return fail(1);
  }

  const exitCode = result.status ?? 1;
  if (exitCode !== 0) {
    return fail(exitCode);
  }

  if (shouldVerifyRunFileCount(run)) {
    const countCheck = verifyReportedFileCount(run.selected.length, output);
    if (!countCheck.ok) {
      return fail(1, countCheck.message);
    }
  }

  if (run.quietOnPass) {
    await writeAndFlush(process.stdout, `[OK] ${run.label}\n`);
  } else {
    await writeRunOutput(result);
  }

  return 0;
}

async function runBucket(name, deps = {}) {
  const selected = (deps.selectBucket ?? selectBucket)(name);

  if (selected.length === 0) {
    console.error(`[X] No tests matched the '${name}' bucket.`);
    return 1;
  }

  if (name === 'slow') {
    const buildStatus = ensureBuildForSlowBucket();
    if (buildStatus !== 0) {
      return buildStatus;
    }
  }

  const runs = getBunRuns(name, selected);
  const isolatedCount = runs.filter((run) => run.quietOnPass).length;
  if (isolatedCount > 0) {
    await writeAndFlush(
      process.stdout,
      `[i] Running ${isolatedCount} test file(s) in isolated Bun processes.\n`
    );
  }

  const failures = [];
  let exitCode = 0;
  for (const run of runs) {
    const status = await runBunTest(run, {
      ...deps,
      report: (failure) => failures.push(failure),
    });
    if (status !== 0) {
      exitCode = status;
    }
  }

  if (exitCode === 0) {
    await writeAndFlush(
      process.stdout,
      `[OK] Bucket '${name}' ran ${selected.length} selected test files.\n`
    );
  } else {
    // Repeat every failure at the very end, so it is the last thing in a long CI log.
    const lines = [`[X] Bucket '${name}' failed:`];
    for (const failure of failures) {
      lines.push(`    ${failure.summary}`);
      for (const failedTest of failure.failedTests) {
        lines.push(`      ${failedTest}`);
      }
    }
    await writeAndFlush(process.stderr, `${lines.join('\n')}\n`);
  }

  return exitCode;
}

async function main(args = process.argv.slice(2), deps = {}) {
  const bucket = args[0];

  if (!['fast', 'slow', 'all'].includes(bucket)) {
    console.error('[X] Usage: node scripts/run-test-bucket.js <fast|slow|all>');
    return 1;
  }

  if (bucket === 'all') {
    let exitCode = 0;

    for (const name of ['fast', 'slow']) {
      const status = await runBucket(name, deps);
      if (status !== 0) {
        exitCode = status;
      }
    }

    return exitCode;
  }

  return runBucket(bucket, deps);
}

// Never end with `process.exit()`: it drops output Node has not yet handed to a
// pipe. GitHub Actions lost everything past the first 64 KB of Bun's output that
// way, including the failing test. Setting `process.exitCode` lets Node finish
// writing first.
async function cli(args = process.argv.slice(2), deps = {}) {
  const exitCode = await main(args, deps);
  process.exitCode = exitCode;
  return exitCode;
}

if (require.main === module) {
  cli().catch((error) => {
    console.error(`[X] ${error && error.stack ? error.stack : error}`);
    process.exitCode = 1;
  });
}

module.exports = {
  slowTests,
  fastJsTests,
  isolatedTests,
  readsBuiltDist,
  shouldForceSlow,
  getDiscoveredTests,
  getSlowSet,
  selectBucket,
  toBunTestPath,
  getBunArgs,
  usesBunTestRunner,
  shouldRunIsolated,
  getBunRuns,
  parseBunFileCount,
  verifyReportedFileCount,
  shouldVerifyRunFileCount,
  describeRunFailure,
  listFailedTests,
  main,
  cli,
};
