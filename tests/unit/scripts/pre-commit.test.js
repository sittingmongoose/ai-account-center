const { expect, test } = require('bun:test');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const hook = path.resolve(__dirname, '../../../.husky/pre-commit');
const phases = ['typecheck', 'lint:fix', 'format:check', 'ui:validate'];

// Run the real hook with harmless disposable command stand-ins. No git or
// product command executes; a failed phase must stop the hook's control flow.
function runHook(failedPhase) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-hook-fixture-'));
  try {
    fs.writeFileSync(path.join(directory, 'bun'),
      '#!/bin/sh\nprintf "%s\\n" "$2" >> "$AAC_HOOK_FIXTURE_LOG"\n' +
      'if [ "$2" = "$AAC_HOOK_FIXTURE_FAIL" ]; then exit 17; fi\n', {mode: 0o700});
    fs.writeFileSync(path.join(directory, 'git'),
      '#!/bin/sh\nprintf "%s\\n" "web-dashboard/ui/dashboard.slint"\n', {mode: 0o700});
    const log = path.join(directory, 'phases');
    const result = spawnSync('/bin/sh', ['-e', hook], {
      cwd: directory,
      env: {PATH: directory + ':/usr/bin:/bin', AAC_HOOK_FIXTURE_LOG: log,
        AAC_HOOK_FIXTURE_FAIL: failedPhase ?? ''},
      encoding: 'utf8', timeout: 5000,
    });
    return {status: result.status, error: result.error,
      phases: fs.readFileSync(log, 'utf8').trim().split('\n')};
  } finally {
    fs.rmSync(directory, {recursive: true, force: true});
  }
}

for (const [index, phase] of phases.entries()) {
  test.skipIf(process.platform === 'win32')(`pre-commit stops after ${phase} fails`, () => {
    const result = runHook(phase);
    expect(result.error).toBeUndefined();
    expect(result.status).toBe(17);
    expect(result.phases).toEqual(phases.slice(0, index + 1));
  });
}
test.skipIf(process.platform === 'win32')('pre-commit accepts successful source and UI checks', () => {
  const result = runHook();
  expect(result.error).toBeUndefined();
  expect(result.status).toBe(0);
  expect(result.phases).toEqual(phases);
});
