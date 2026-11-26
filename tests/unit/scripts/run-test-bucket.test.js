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

  test('keeps declared slow tests that read the repository root in the slow bucket', () => {
    const slowSet = bucket.getSlowSet();

    expect(slowSet.has('tests/unit/foundation/npm-rust-files.test.ts')).toBe(true);
  });

  test('builds the slow bucket without a missing explicit file', () => {
    expect(() => bucket.getBunRuns('slow')).not.toThrow();
  });

  test('forces npm tests into the slow bucket', () => {
    expect(bucket.shouldForceSlow('tests/npm/cli.test.js')).toBe(true);
  });

  test('discovers tests only under tests/ now that src/ is gone', () => {
    const discovered = bucket.getDiscoveredTests();

    expect(discovered).toContain('tests/unit/scripts/run-test-bucket.test.js');
    expect(discovered.every((file) => file.startsWith('tests/'))).toBe(true);
  });

  test('passes selected tests to Bun as explicit relative paths', () => {
    const args = bucket.getBunArgs('fast', [
      'tests/unit/example.test.ts',
      'tests/integration/example.test.ts',
    ]);

    expect(args).toEqual([
      'test',
      './tests/unit/example.test.ts',
      './tests/integration/example.test.ts',
    ]);
  });

  test('keeps slow bucket concurrency flag before explicit test paths', () => {
    const args = bucket.getBunArgs('slow', ['tests/integration/example.test.ts']);

    expect(args).toEqual(['test', '--max-concurrency=1', './tests/integration/example.test.ts']);
  });

  test('isolates known sticky suites while keeping the rest in a shared run', () => {
    const runs = bucket.getBunRuns('fast', [
      'tests/unit/scripts/run-test-bucket.test.js',
      'tests/unit/web-server/rust-slow-filesystem.test.ts',
    ]);

    expect(runs.map((run) => run.label)).toEqual([
      'shared',
      'tests/unit/web-server/rust-slow-filesystem.test.ts',
    ]);
    expect(runs[0].selected).toEqual(['tests/unit/scripts/run-test-bucket.test.js']);
    expect(runs[1].quietOnPass).toBe(true);
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
    expect(bucket.shouldForceSlow('tests/unit/scripts/build-agent.test.ts')).toBe(true);
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

  test('splits a shared group whose quoted command line would overflow cmd.exe', () => {
    const paths = Array.from(
      { length: 100 },
      (_, i) => `./tests/unit/web-server/some-quite-long-suite-name-${i}.test.ts`
    );

    const chunks = bucket.chunkByCommandLineLength(paths, 400);

    // Every test still runs exactly once, in the selected order.
    expect(chunks.flat()).toEqual(paths);
    expect(chunks.length).toBeGreaterThan(1);
    for (const chunk of chunks) {
      expect(chunk.length).toBeGreaterThan(0);
      // Each chunk's quoted, space-joined form stays under the cap (a single path that
      // alone exceeds it still runs, as its own chunk).
      if (chunk.length > 1) {
        expect(chunk.map((p) => `"${p}"`).join(' ').length).toBeLessThanOrEqual(400);
      }
    }
  });

  test('keeps a shared group that fits in one chunk unsplit', () => {
    const paths = ['./tests/unit/a.test.ts', './tests/unit/b.test.ts', './tests/unit/c.test.ts'];

    expect(bucket.chunkByCommandLineLength(paths, 400)).toEqual([paths]);
  });

  test('still runs a single path that alone exceeds the cap, as its own chunk', () => {
    const long = `./tests/unit/${'x'.repeat(120)}.test.ts`;

    expect(bucket.chunkByCommandLineLength(['./tests/unit/a.test.ts', long], 40)).toEqual([
      ['./tests/unit/a.test.ts'],
      [long],
    ]);
  });

  test('never chunks off Windows because only cmd.exe caps the command line', () => {
    expect(bucket.commandLineLimitChars('win32')).toBeLessThanOrEqual(8191);
    expect(bucket.commandLineLimitChars('linux')).toBe(Number.POSITIVE_INFINITY);
    expect(bucket.commandLineLimitChars('darwin')).toBe(Number.POSITIVE_INFINITY);
  });
});

describe('slow bucket build on machines without local Rust builds', () => {
  test('builds only the server when AAC_NO_LOCAL_RUST_BUILD is 1', () => {
    expect(bucket.slowBucketBuildScript({ AAC_NO_LOCAL_RUST_BUILD: '1' })).toBe('build:server');
    expect(bucket.slowBucketBuildScript({})).toBe('build');
    expect(bucket.slowBucketBuildScript({ AAC_NO_LOCAL_RUST_BUILD: '0' })).toBe('build');
  });
});
