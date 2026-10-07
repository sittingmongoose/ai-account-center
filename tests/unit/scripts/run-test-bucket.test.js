const { describe, expect, test } = require('bun:test');
const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');
const { spawnSync } = require('node:child_process');
const bucket = require('../../../scripts/run-test-bucket.js');

// Disposable source fixtures test runner detection independently of retired product suites.
function withValidationFixtures(check) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-test-bucket-'));
  const root = path.resolve(__dirname, '../../../');
  const suitePath = path.join(directory, 'suite.test.ts');
  const standalonePath = path.join(directory, 'standalone.js');
  fs.writeFileSync(suitePath, "import { test } from 'bun:test';\n");
  fs.writeFileSync(standalonePath, "console.log('standalone fixture');\n");
  try {
    check({
      suite: path.relative(root, suitePath),
      standalone: path.relative(root, standalonePath),
    });
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
}

describe('run-test-bucket', () => {
  test('all declared slow and isolated tests still exist on disk', async () => {
    for (const relativePath of [...bucket.slowTests, ...bucket.isolatedTests]) {
      const absolutePath = path.resolve(__dirname, '../../../', relativePath);
      await expect(Bun.file(absolutePath).exists()).resolves.toBe(true);
    }
  });

  test('keeps web-server integration tests that bind ports in the slow bucket', () => {
    const slowSet = bucket.getSlowSet();

    expect(slowSet.has('tests/integration/web-server/codex-profiles-endpoint.test.ts')).toBe(true);
  });

  test('forces npm tests into the slow bucket', () => {
    expect(bucket.shouldForceSlow('tests/npm/cli.test.js')).toBe(true);
  });

  test('discovers colocated backend tests under src', () => {
    const discovered = bucket.getDiscoveredTests();

    expect(discovered).toContain('src/cliproxy/types/__tests__/types-backward-compat.test.ts');
  });

  test('passes selected tests to Bun as explicit relative paths', () => {
    const args = bucket.getBunArgs('fast', [
      'tests/unit/example.test.ts',
      'src/cliproxy/types/__tests__/types-backward-compat.test.ts',
    ]);

    expect(args).toEqual([
      'test',
      './tests/unit/example.test.ts',
      './src/cliproxy/types/__tests__/types-backward-compat.test.ts',
    ]);
  });

  test('keeps slow bucket concurrency flag before explicit test paths', () => {
    const args = bucket.getBunArgs('slow', ['tests/integration/example.test.ts']);

    expect(args).toEqual(['test', '--max-concurrency=1', './tests/integration/example.test.ts']);
  });

  test('isolates src tests while keeping already-covered tests in a shared run', () => {
    const runs = bucket.getBunRuns('fast', [
      'tests/unit/scripts/run-test-bucket.test.js',
      'src/cliproxy/types/__tests__/types-backward-compat.test.ts',
      'src/errors/__tests__/error-types.test.ts',
    ]);

    expect(runs.map((run) => run.label)).toEqual([
      'shared',
      'src/cliproxy/types/__tests__/types-backward-compat.test.ts',
      'src/errors/__tests__/error-types.test.ts',
    ]);
    expect(runs[0].selected).toEqual(['tests/unit/scripts/run-test-bucket.test.js']);
    expect(runs[0].bunArgs).toEqual(['test', './tests/unit/scripts/run-test-bucket.test.js']);
    expect(runs[1].bunArgs).toEqual([
      'test',
      './src/cliproxy/types/__tests__/types-backward-compat.test.ts',
    ]);
    expect(runs[1].quietOnPass).toBe(true);
  });

  test('isolates known sticky mock suites outside src', () => {
    const runs = bucket.getBunRuns('fast', [
      'tests/unit/scripts/run-test-bucket.test.js',
      'tests/unit/utils/fetch-proxy-setup.test.ts',
      'tests/unit/web-server/usage/account-attribution.test.ts',
    ]);

    expect(runs.map((run) => run.label)).toEqual([
      'shared',
      'tests/unit/utils/fetch-proxy-setup.test.ts',
      'tests/unit/web-server/usage/account-attribution.test.ts',
    ]);
  });

  test('isolates standalone validation scripts that are not Bun test suites', () => {
    withValidationFixtures(({ suite, standalone }) => {
      const runs = bucket.getBunRuns('slow', [suite, standalone]);

      expect(bucket.usesBunTestRunner(suite)).toBe(true);
      expect(bucket.usesBunTestRunner(standalone)).toBe(false);
      expect(runs.map((run) => run.label)).toEqual(['shared', standalone]);
      expect(runs[1].quietOnPass).toBe(true);
    });
  });

  test('parses Bun file counts from test summaries', () => {
    expect(bucket.parseBunFileCount('\u001b[32mRan 2559 tests across 272 files\u001b[0m')).toBe(
      272
    );
  });

  test('detects when Bun reports fewer files than the bucket selected', () => {
    const result = bucket.verifyReportedFileCount(364, 'Ran 2559 tests across 272 files');

    expect(result.ok).toBe(false);
    expect(result.reportedCount).toBe(272);
    expect(result.selectedCount).toBe(364);
    expect(result.message).toContain('Bun ran 272 files');
  });

  test('skips Bun file-count verification for standalone validation scripts', () => {
    withValidationFixtures(({ suite, standalone }) => {
      expect(bucket.shouldVerifyRunFileCount({ selected: [standalone] })).toBe(false);
      expect(bucket.shouldVerifyRunFileCount({ selected: [suite] })).toBe(true);
    });
  });

  test('keeps dist-independent javascript tests in the fast bucket', () => {
    expect(bucket.shouldForceSlow('tests/unit/flag-parsing-simple.test.js')).toBe(false);
  });

  test('keeps non-allowlisted javascript tests in the slow bucket', () => {
    expect(bucket.shouldForceSlow('tests/unit/scripts/run-test-bucket.test.js')).toBe(true);
  });

  test('still forces dist-dependent tests into the slow bucket', () => {
    expect(bucket.shouldForceSlow('tests/unit/config-dir-override.test.js')).toBe(true);
  });

  // GitHub Actions gives the runner pipes, not a terminal. Node writes to a pipe
  // asynchronously on Linux, and the runner used to end with process.exit(), which
  // threw away everything past the first 64 KB of Bun's output: the failing test,
  // Bun's summary and the reason for the exit code were never printed.
  test('prints all Bun output through pipes before it exits, failure last', () => {
    const runnerPath = path.resolve(__dirname, '../../../scripts/run-test-bucket.js');
    const child = `
      const bucket = require(${JSON.stringify(runnerPath)});
      const block = (letter) => (letter.repeat(99) + '\\n').repeat(10000);
      bucket.cli(['fast'], {
        selectBucket: () => ['tests/unit/flag-parsing-simple.test.js'],
        spawnSync: () => ({
          status: 1,
          signal: null,
          stdout: block('o') + 'stdout end marker\\n',
          stderr: block('e') + '(fail) hidden suite > the failure past 64 KB [1.00ms]\\n',
        }),
      });
    `;
    // The scripts run the bucket runner with Node (package.json), whose pipe writes are
    // the ones that can be cut short, so the check runs it with Node too, not Bun.
    const result = spawnSync('node', ['-e', child], {
      encoding: 'utf8',
      maxBuffer: 16 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    expect(result.status).toBe(1);
    expect(result.stdout.length).toBeGreaterThan(1_000_000);
    expect(result.stdout).toContain('stdout end marker');
    expect(result.stderr.length).toBeGreaterThan(1_000_000);
    expect(result.stderr).toContain('(fail) hidden suite > the failure past 64 KB');
    const lastLines = result.stderr.trimEnd().split('\n').slice(-3);
    expect(lastLines).toEqual([
      "[X] Bucket 'fast' failed:",
      "    [X] Bun run 'tests/unit/flag-parsing-simple.test.js' failed with exit code 1.",
      '      (fail) hidden suite > the failure past 64 KB [1.00ms]',
    ]);
  });

  test('says when a signal stopped Bun instead of a bare exit code', () => {
    const run = { label: 'shared', selected: ['a.test.ts', 'b.test.ts'] };

    expect(bucket.describeRunFailure(run, { status: null, signal: 'SIGKILL' }, 1)).toBe(
      "[X] Bun run 'shared (2 files)' was stopped by signal SIGKILL before it finished."
    );
    expect(bucket.describeRunFailure(run, { status: 3, signal: null }, 3)).toBe(
      "[X] Bun run 'shared (2 files)' failed with exit code 3."
    );
    expect(
      bucket.describeRunFailure(run, { status: null, error: new Error('spawnSync bun ENOBUFS') }, 1)
    ).toBe("[X] Bun run 'shared (2 files)' could not run: spawnSync bun ENOBUFS");
  });

  test('lists failed tests from Bun output once each, colors removed', () => {
    // Bun repeats every failure in its closing recap.
    const output = [
      '(pass) one [1.00ms]',
      '\u001b[31m(fail)\u001b[0m two > breaks [2.00ms]',
      '(fail) three',
      '2 tests failed:',
      '(fail) two > breaks [2.00ms]',
      '(fail) three',
    ].join('\n');

    expect(bucket.listFailedTests(output)).toEqual([
      '(fail) two > breaks [2.00ms]',
      '(fail) three',
    ]);
  });
});
