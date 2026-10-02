import { readFileSync } from 'fs';
import { spawnSync } from 'child_process';
import { describe, expect, it } from 'bun:test';

const dockerfile = readFileSync('docker/Dockerfile', 'utf8');
const entrypoint = readFileSync('docker/entrypoint.sh', 'utf8');

describe('source-built dashboard Docker lifecycle', () => {
  it('installs locked build and runtime dependencies without package lifecycle side effects', () => {
    const installCommands = dockerfile.match(/\bbun install[^\n]*/g) ?? [];

    expect(installCommands.length).toBe(2);
    for (const command of installCommands) {
      expect(command).toContain('--frozen-lockfile');
      expect(command).toContain('--ignore-scripts');
    }
    expect(installCommands.some((command) => command.includes('--production'))).toBe(true);
    expect(dockerfile).toContain('bun run build:all');
    // The wasm moved to dist/ui/pkg/<buildId>/; the manifest-driven check finds it.
    expect(dockerfile).toContain('test -s dist/ui/ui-build-manifest.json');
    expect(dockerfile).toContain('node scripts/verify-bundle.js');
    expect(dockerfile).not.toContain('dist/ui/pkg/ccs_account_dashboard_bg.wasm');
    expect(dockerfile).not.toMatch(/\b(?:npm|bun)\s+(?:install|add)[^\n]*@kaitranntt\/ccs/);
  });

  it('starts only the dashboard as the existing unprivileged runtime identity', () => {
    const command = dockerfile.match(/^CMD (.+)$/m);
    expect(command).not.toBeNull();
    expect(JSON.parse(command![1])).toEqual([
      'ai-account-center',
      'dashboard',
      '--host',
      '0.0.0.0',
      '--port',
      '3000',
      '--no-open',
    ]);
    expect(dockerfile).toMatch(/^USER node$/m);
    expect(dockerfile).toMatch(/^EXPOSE 3000$/m);
    expect(dockerfile).not.toMatch(/^EXPOSE[^\n]*8317/m);
    expect(dockerfile).toContain('http://127.0.0.1:3000/');
    expect(entrypoint).toContain('exec "$@"');
  });

  it('preserves direct directory overrides and the legacy home that appends .ccs', () => {
    const assignment = entrypoint.match(/^ccs_home_dir=.*$/m)?.[0];
    expect(assignment).toBeDefined();
    const cases: Array<[Record<string, string>, string]> = [
      [{}, '/home/node/.ccs'],
      [{ CCS_HOME: '/fixture-home' }, '/fixture-home/.ccs'],
      [{ CCS_HOME_DIR: '/fixture-legacy', CCS_HOME: '/fixture-home' }, '/fixture-legacy'],
      [
        { CCS_DIR: '/fixture-direct', CCS_HOME_DIR: '/fixture-legacy', CCS_HOME: '/fixture-home' },
        '/fixture-direct',
      ],
    ];
    for (const [overrides, expected] of cases) {
      const result = spawnSync('bash', ['-c', `${assignment}\nprintf '%s' "$ccs_home_dir"`], {
        encoding: 'utf8',
        env: { PATH: process.env.PATH ?? '/usr/bin:/bin', ...overrides },
      });
      expect(result.status).toBe(0);
      expect(result.stdout).toBe(expected);
    }
  });

  it('keeps private state intact and rejects unreadable or unwritable mounts', () => {
    expect(entrypoint).toContain('umask 077');
    expect(entrypoint).toContain('mkdir -p "$ccs_home_dir"');
    expect(entrypoint).toContain('[ ! -r "$ccs_home_dir" ]');
    expect(entrypoint).toContain('[ ! -w "$ccs_home_dir" ]');
    expect(entrypoint).not.toMatch(/\b(?:chown|chmod|su|runuser)\b/);
    expect(entrypoint).not.toMatch(/\b(?:rm|mv|cp)\s/);
    expect(entrypoint).not.toMatch(/\b(?:auth setup|npm install|bun add)\b/);
  });
});
