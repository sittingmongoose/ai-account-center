const { describe, it, expect, beforeEach, afterEach } = require('bun:test');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const packageRoot = path.resolve(__dirname, '../..');
const cliSource = path.join(packageRoot, 'src/ccs.ts');
let testHome;
let configDir;
let credentialFiles;

beforeEach(() => {
  testHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ai-account-center-cli-'));
  configDir = path.join(testHome, '.ccs');
  fs.mkdirSync(configDir, { recursive: true });
  fs.mkdirSync(path.join(testHome, '.codex'), { recursive: true });
  credentialFiles = new Map([
    [
      path.join(configDir, 'config.yaml'),
      'version: 2\ndashboard_auth:\n  enabled: true\n  username: test-account\n  password_hash: test-only-hash\n',
    ],
    [path.join(configDir, 'codex-profiles.yaml'), 'version: 1\ndefault: null\nprofiles: {}\n'],
    [path.join(configDir, 'profiles.json'), '{"profiles":{"saved":{"path":"preserve-me"}}}\n'],
    [path.join(testHome, '.codex/auth.json'), '{"test_fixture":"preserve-auth"}\n'],
  ]);
  for (const [filename, content] of credentialFiles) fs.writeFileSync(filename, content);
});

afterEach(() => {
  for (const [filename, content] of credentialFiles) {
    expect(fs.readFileSync(filename, 'utf8')).toBe(content);
  }
  fs.rmSync(testHome, { recursive: true, force: true });
});

function runCli(args) {
  const env = {
    ...process.env,
    CCS_HOME: testHome,
    CODEX_HOME: path.join(testHome, '.codex'),
    CI: '1',
    NO_COLOR: '1',
  };
  delete env.CCS_DIR;
  return spawnSync(process.execPath, [cliSource, ...args], {
    cwd: packageRoot,
    env,
    encoding: 'utf8',
    timeout: 10000,
  });
}

describe('AI Account Center CLI', () => {
  it('shows product help with retained commands and no profile launch defaults', () => {
    for (const args of [[], ['--help'], ['-h'], ['help']]) {
      const result = runCli(args);
      expect(result.error).toBeUndefined();
      expect(result.status).toBe(0);
      expect(result.stdout).toContain('AI Account Center');
      expect(result.stdout).not.toMatch(/\u001b\[/);
      expect(result.stdout).toContain('codex-auth');
      expect(result.stdout).toContain('dashboard');
      expect(result.stdout).not.toContain('ccs <profile>');
      expect(result.stdout).not.toContain('CLIProxy variants');
    }
  });

  it('reads package version and reports existing config paths', () => {
    for (const args of [['--version'], ['-v'], ['version']]) {
      const result = runCli(args);
      expect(result.status).toBe(0);
      expect(result.stdout).toContain(
        `AI Account Center v${require('../../package.json').version}`
      );
      expect(result.stdout).toContain(configDir);
    }
  });

  it('preserves dashboard/config help aliases without starting the server', () => {
    for (const args of [
      ['dashboard', '--help'],
      ['config', '--help'],
      ['dashboard', '--help=true'],
      ['config', '--help=true'],
      ['help', 'dashboard'],
      ['help', 'config'],
    ]) {
      const result = runCli(args);
      expect(result.status).toBe(0);
      expect(result.stdout).toContain('--no-open');
      expect(result.stdout).not.toContain('Starting dashboard server');
      expect(result.stdout).not.toContain('Starting CLIProxy');
    }
  });

  it('selects --config-dir before logs and version commands read configuration', () => {
    const alternate = path.join(testHome, 'private-config');
    fs.mkdirSync(alternate);
    const result = runCli(['--config-dir', alternate, '--version']);
    expect(result.status).toBe(0);
    expect(result.stdout).toContain(`Account configuration: ${alternate}`);
    expect(result.stdout).not.toContain(`Account configuration: ${configDir}`);
  });

  it('rejects invalid global paths before commands execute', () => {
    const result = runCli(['--config-dir', path.join(testHome, 'missing'), '--version']);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain('Config directory not found');
    expect(result.stdout).not.toContain('AI Account Center v');
  });

  it('retired commands and profile invocations fail explicitly without launching CLIs', () => {
    for (const args of [
      ['glm'],
      ['codex', 'login'],
      ['cursor', 'probe'],
      ['docker', 'up'],
      ['api', 'create'],
      ['-p', 'old prompt'],
      ['config', 'channels'],
      ['--install'],
      ['--uninstall'],
      ['--doctor'],
    ]) {
      const result = runCli(args);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain('retired');
      expect(result.stderr).toContain('ai-account-center dashboard');
      expect(result.stdout).not.toContain('Starting');
    }
  });

  it('Codex retired shell commands emit no exports and preserve private files', () => {
    for (const command of ['use', 'switch']) {
      const result = runCli(['codex-auth', command, 'saved']);
      expect(result.status).toBe(1);
      expect(result.stdout).toBe('');
      expect(result.stderr).toContain('codex-auth activate <saved-login>');
      expect(result.stderr).toContain('No CODEX_HOME exports');
    }
  });

  it('legacy launcher bins point to a migration-only stub with no shell output', () => {
    const pkg = require('../../package.json');
    for (const alias of ['ccsx', 'ccs-codex', 'ccsd', 'ccs-droid', 'ccsxp']) {
      expect(pkg.bin[alias]).toBe('dist/bin/compat-cli.js');
    }
    const env = { ...process.env, CCS_HOME: testHome, CODEX_HOME: path.join(testHome, '.codex') };
    delete env.CCS_DIR;
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, 'src/bin/compat-cli.ts'), 'auth', 'use', 'saved'],
      {
        cwd: packageRoot,
        env,
        encoding: 'utf8',
        timeout: 10000,
      }
    );
    expect(result.status).toBe(1);
    expect(result.stdout).toBe('');
    expect(result.stderr).toContain('retired upstream CCS launcher');
    expect(result.stderr).toContain('codex-auth activate <saved-login>');
  });

  it('retired self-update cannot invoke package installation for any old flag variant', () => {
    for (const args of [
      ['update'],
      ['--update', '--force'],
      ['update', '--beta'],
      ['update', '--dev'],
      ['update', '--help'],
    ]) {
      const result = runCli(args);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain('Upstream CCS self-update is retired');
      expect(result.stderr).toContain('Installed application updates remain available');
      expect(result.stdout).not.toContain('Installing');
      expect(result.stdout).not.toContain('npm');
    }
  });
});
