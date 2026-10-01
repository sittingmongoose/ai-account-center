import { describe, expect, test } from 'bun:test';
import * as fs from 'node:fs';
import * as path from 'node:path';

function resolvePath(relativePath: string) {
  return path.resolve(import.meta.dir, relativePath);
}

describe('pr ci workflow', () => {
  test('keeps full coverage on pull requests', () => {
    const workflowPath = resolvePath('../../../../.github/workflows/ci.yml');
    const trustedAuthorGate =
      'contains(fromJSON(\'["COLLABORATOR","MEMBER","OWNER"]\'), github.event.pull_request.author_association)';

    expect(fs.existsSync(workflowPath)).toBe(true);

    const workflow = fs.readFileSync(workflowPath, 'utf8');

    expect(workflow).toContain('name: CI');
    expect(workflow).toContain('pull_request:');
    // Unrestricted target branches keep validation available when the product
    // default branch changes, including the current feat/activate-in-place.
    expect(workflow).not.toMatch(/^\s+branches:/m);
    // Every source-executing job is gated: validate (matrix), build and test.
    expect(workflow.split(trustedAuthorGate).length - 1).toBe(3);
    expect(workflow).toContain('needs: [validate, build, test]');
    expect(workflow).toContain('group: ci-${{ github.ref }}');
    expect(workflow).toContain('cancel-in-progress: true');
    expect(workflow).toContain('fail-fast: false');
    expect(workflow).toContain('runs-on: ubuntu-latest');
    expect(workflow).not.toContain('self-hosted');
    expect(workflow).toContain("cmd: 'bun run typecheck'");
    expect(workflow).toContain("cmd: 'bun run lint'");
    expect(workflow).toContain("cmd: 'bun run format:check'");
    expect(workflow).toContain(
      "key: ${{ runner.os }}-bun-cache-v3-${{ hashFiles('bun.lock', 'web-dashboard/Cargo.lock') }}"
    );
    expect(workflow).not.toContain('ui/bun.lock');
    expect(workflow).not.toContain('restore-keys:');
    expect(workflow).toContain('name: dist');
    expect(workflow).toContain('path: dist/');
    expect(workflow).toContain('needs: [build]');
    expect(workflow).toContain('run: bun run test:all');
    expect(workflow).toContain('name: Test offline Claude quota collector');
    expect(workflow).toContain('env -i PATH="$PATH" HOME="$TEST_FIXTURE_ROOT"');
    expect(workflow).toContain('CCS_HOME="$TEST_FIXTURE_ROOT"');
    expect(workflow).toContain(
      'PYTHONDONTWRITEBYTECODE=1 python3 tests/unit/account-usage/claude_usage_test.py'
    );
    expect(workflow).not.toContain('CCS_E2E_SKIP_BUILD');
    expect(workflow).not.toContain('test:e2e');
  });
});
