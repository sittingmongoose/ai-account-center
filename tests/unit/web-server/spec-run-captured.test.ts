// The M0 spec capture keeps a large corpus diff and names the failing step (TOOL-ISSUES catalog-astra-T1).
// Before the fix the same child lost its output: ENOBUFS under Node's default pipe buffer, or a silent
// truncation to a few hundred KB under Bun 1.4.4's spawnSync, hiding the failing step.
import { expect, test } from 'bun:test';
import { failingStep, runCaptured } from '../../spec/run-captured.mjs';

/** The spawn failure shape: an Error carrying the child's exit code and the captured output. */
type CaptureError = Error & { code?: string | number; stdout?: string };

function captureError(fn: () => unknown): CaptureError {
  try {
    fn();
  } catch (e) {
    // The helper throws Error; the intersection adds the fields the compiler does not track.
    if (e instanceof Error) return e as CaptureError;
    throw e;
  }
  throw new Error('expected the run to fail');
}

// A runner like tests/spec/run.mjs: exec each step with stdio inherit. The inner step writes 2 MiB (past
// what the pipe capture kept) and fails, so the outer exits non-zero with the step's "Command failed:"
// line in the output.
const inner =
  "process.stdout.write('x'.repeat(2 * 1024 * 1024)); process.stderr.write('golden diff: scoreboard\\n'); process.exit(3);";
const runner = `require('node:child_process').execFileSync('node', ['-e', ${JSON.stringify(inner)}], { stdio: 'inherit' });`;

test('a large failing diff is kept in full and names its step first', () => {
  const error = captureError(() => runCaptured('node', ['-e', runner]));
  expect(error.message.startsWith('failing step: node -e ')).toBe(true);
  expect(error.message).toContain('golden diff: scoreboard');
  // The full 2 MiB diff is kept: the pipe capture truncated it to a few hundred KB (or ENOBUFS).
  expect(error.stdout ? error.stdout.length : 0).toBeGreaterThanOrEqual(2 * 1024 * 1024);
  // The tail is bounded; the 2 MiB diff itself is not pasted into the report.
  expect(error.message.length).toBeLessThan(200_000);
});

test('success returns the captured stdout', () => {
  const out = runCaptured('node', ['-e', "process.stdout.write('ok')"]);
  expect(out.toString()).toBe('ok');
});

test('a large successful output is kept in full, not truncated', () => {
  const out = runCaptured('node', [
    '-e',
    "process.stdout.write('x'.repeat(2 * 1024 * 1024));",
  ]);
  expect(out.length).toBe(2 * 1024 * 1024);
});

test('failingStep takes the innermost command line', () => {
  const output =
    'Command failed: node tests/spec/run.mjs\nnoise\nCommand failed: bun tests/spec/capture.ts\n';
  expect(failingStep(output)).toBe('bun tests/spec/capture.ts');
  expect(failingStep('')).toBeNull();
});

test('a failure without a step marker still reports a bounded tail', () => {
  const error = captureError(() =>
    runCaptured('node', ['-e', "process.stderr.write('boom\\n'); process.exit(2);"])
  );
  expect(error.message).toContain('no failing step marker');
  expect(error.message).toContain('boom');
});

// Real clock on purpose: the subject is spawnSync's own timeout/kill against a real child; fake timers
// cannot drive the child. The 500 ms budget bounds the test.
test('the caller timeout still fires', () => {
  const error = captureError(() =>
    runCaptured('node', ['-e', 'setTimeout(() => {}, 5000);'], { timeout: 500 })
  );
  expect(error.code).toBe('ETIMEDOUT');
});
