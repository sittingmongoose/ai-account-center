import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { describe, expect, it } from 'bun:test';

const retiredHelpers = [
  'scripts/account-usage/claude_usage.py',
  'scripts/account-usage/cursor_usage.py',
  'scripts/account-usage/desktop_helpers.py',
  'scripts/account-usage/desktop_usage.py',
  'scripts/account-usage/failover_additional.py',
  'scripts/account-usage/failover_core.py',
  'scripts/account-usage/failover_special.py',
  'scripts/account-usage/failover_usage.py',
  'scripts/account-usage/key_store.py',
  'scripts/account-usage/muse_native.py',
  'scripts/account-usage/signin_expiry.py',
  'scripts/antigravity',
  'scripts/claude-history',
  'scripts/claude-signin',
  'scripts/analytics-remote',
  'scripts/app-updates',
  'scripts/agent-usage/aac-usage',
  'scripts/agent-usage/aac-usage.cmd',
  'crates/aac-http/src/app_updates/helpers.rs',
  'crates/aac-http/src/app_updates/helper_sync.py',
  'crates/aac-http/src/app_updates/helper_rollback.py',
  'crates/aac-http/src/claude_host/mac.py',
  'crates/aac-http/src/claude_host/windows.ps1',
];

describe('the shipped runtime has no retired Python helper surface', () => {
  it('does not package or execute the replaced helper trees', () => {
    expect(retiredHelpers.filter((path) => existsSync(path))).toEqual([]);
    expect(readdirSync('scripts/account-usage').sort()).toEqual([
      '.gitignore',
      '.npmignore',
      'README.md',
      'muse_console.py',
      'plan_common.py',
      'plan_usage.py',
    ]);

    const dockerfile = readFileSync('docker/Dockerfile', 'utf8');
    const runtime = dockerfile.slice(dockerfile.indexOf(' AS runtime'));
    expect(runtime).not.toContain('COPY --from=build /app/scripts ./scripts');
    expect(runtime).toContain(
      'COPY --from=build /app/scripts/account-usage ./scripts/account-usage'
    );
    expect(runtime).toContain(
      'COPY --from=build /app/scripts/agent-usage ./scripts/agent-usage'
    );

    const collection = readFileSync('crates/aac-http/src/collection.rs', 'utf8');
    expect(collection).not.toContain('account_usage::helper');
    expect(collection).not.toContain('read_helper');


    const updates = readFileSync('crates/aac-http/src/app_updates/mod.rs', 'utf8');
    expect(updates).not.toContain('mod helpers');

    const snapshot = readFileSync('crates/aac-snapshot/src/client.rs', 'utf8');
    expect(snapshot).not.toContain('scripts/claude-signin');
    expect(snapshot).not.toContain('RUNNER_FILES');
  });
});
