import { describe, expect, test } from 'bun:test';
import * as fs from 'node:fs';
import * as path from 'node:path';

function resolvePath(relativePath: string) {
  return path.resolve(import.meta.dir, relativePath);
}

describe('push ci workflow', () => {
  test('validates pushed branches without publishing a release', () => {
    const workflowPath = resolvePath('../../../../.github/workflows/push-ci.yml');

    expect(fs.existsSync(workflowPath)).toBe(true);

    const workflow = fs.readFileSync(workflowPath, 'utf8');

    expect(workflow).toContain('name: Push CI');
    expect(workflow).toContain('push:');
    expect(workflow).not.toMatch(/^\s+branches:/m);
    expect(workflow).toContain('group: push-ci-${{ github.ref }}');
    expect(workflow).toContain('cancel-in-progress: true');
    expect(workflow).toContain('runs-on: ubuntu-latest');
    expect(workflow).not.toContain('self-hosted');
    expect(workflow).toContain(
      "key: ${{ runner.os }}-bun-cache-v3-${{ hashFiles('bun.lock', 'web-dashboard/Cargo.lock') }}"
    );
    expect(workflow).not.toContain('ui/bun.lock');
    expect(workflow).not.toContain('restore-keys:');
    expect(workflow).toContain('name: ${{ matrix.check.name }}');
    // No TypeScript production source remains, so no typecheck, lint or format step.
    expect(workflow).not.toContain('bun run typecheck');
    expect(workflow).not.toContain('bun run lint');
    expect(workflow).not.toContain('bun run format:check');
    expect(workflow).toContain("cmd: 'node scripts/personal-detail-guard.js'");
    expect(workflow).toContain("cmd: 'node tests/spec/inventory.mjs'");
    // Every job installs straight from the lockfile: there is no dependency wrapper script.
    expect(workflow).not.toContain('scripts/ensure-deps.sh');
    expect(workflow.split('run: bun install --frozen-lockfile').length - 1).toBe(3);
    expect(workflow).toContain('run: bun run build:all');
    expect(workflow).toContain("FAST_TEST_BUDGET_SECONDS: '90'");
    expect(workflow).toContain('name: Test fast bucket with perf budget');
    expect(workflow).toContain('bun run test:fast');
    expect(workflow).toContain('test_status=$?');
    expect(workflow).toContain('exit "$test_status"');
    expect(workflow).toContain('::warning::test:fast took ${elapsed_seconds}s');
    expect(workflow).toContain('scripts/run-test-bucket.js');
    expect(workflow).toContain('run: bun run test:slow');
    expect(workflow).toContain('name: Test offline Claude quota collector');
    expect(workflow).toContain('env -i PATH="$PATH" HOME="$TEST_FIXTURE_ROOT"');
    expect(workflow).toContain('CCS_HOME="$TEST_FIXTURE_ROOT"');
    expect(workflow).toContain(
      'PYTHONDONTWRITEBYTECODE=1 python3 tests/unit/account-usage/claude_usage_test.py'
    );
    expect(workflow).not.toContain('CCS_E2E_SKIP_BUILD');
    expect(workflow).not.toContain('test:e2e');
    expect(workflow).not.toMatch(
      /npm publish|semantic-release|gh release|docker\/build-push-action/
    );
  });

  test('ships the Linux history store and status readers the spec suite needs', () => {
    const workflow = fs.readFileSync(
      resolvePath('../../../../.github/workflows/push-ci.yml'),
      'utf8'
    );
    const build = workflow.indexOf('run: bun run build:all');
    const natives = workflow.indexOf('bun run store:build\n          bun run status:build');
    const upload = workflow.indexOf('name: Upload dist artifact');
    // tests/spec/read-http-parity.ts refuses a stub store; the modules must be in dist/ before upload.
    expect(build).toBeGreaterThan(-1);
    expect(natives).toBeGreaterThan(build);
    expect(upload).toBeGreaterThan(natives);
  });

  test('keeps the in-checkout Bun cache out of every git-listed scan', () => {
    const workflow = fs.readFileSync(
      resolvePath('../../../../.github/workflows/push-ci.yml'),
      'utf8'
    );
    const ignore = fs.readFileSync(resolvePath('../../../../.gitignore'), 'utf8');
    // CI keeps BUN_INSTALL_CACHE_DIR inside the checkout; untracked scans (tests/spec/inventory.mjs
    // uses `git ls-files --others --exclude-standard`) must not see the cached packages' tests.
    expect(workflow).toContain('BUN_INSTALL_CACHE_DIR: ${{ github.workspace }}/.bun/install/cache');
    expect(ignore).toMatch(/^\.bun\/$/m);
  });
});
