const assert = require('assert');
const fs = require('fs');
const { spawnSync } = require('child_process');
const os = require('os');
const path = require('path');

describe('account-center package lifecycle', () => {
  const scripts = path.resolve(__dirname, '../../scripts');
  let testHome;

  beforeEach(() => {
    testHome = fs.mkdtempSync(path.join(os.tmpdir(), 'account-center-lifecycle-'));
  });

  afterEach(() => {
    fs.rmSync(testHome, { recursive: true, force: true });
  });

  function run(script, overrides = {}) {
    return spawnSync('node', [path.join(scripts, script)], {
      encoding: 'utf8',
      env: { ...process.env, CCS_HOME: testHome, CCS_DIR: '', ...overrides },
    });
  }

  it('creates only the private account directory on a fresh install', () => {
    const result = run('postinstall.js');
    assert.strictEqual(result.status, 0, result.stderr);
    assert.deepStrictEqual(fs.readdirSync(testHome), ['.ccs']);
    assert.deepStrictEqual(fs.readdirSync(path.join(testHome, '.ccs')), []);
    if (process.platform !== 'win32') {
      assert.strictEqual(fs.statSync(path.join(testHome, '.ccs')).mode & 0o777, 0o700);
    }
  });

  it('preserves existing configs, account files, Claude settings and old integrations through install and uninstall', () => {
    const files = {
      '.ccs/config.yaml': 'existing malformed YAML: [\n',
      '.ccs/config.json': '{"profiles":{"existing":"~/existing.settings.json"}}\n',
      '.ccs/codex/profiles/saved/auth.json': '{"fixture":"saved account"}\n',
      '.ccs/hooks/websearch-transformer.cjs': '// existing user-owned integration\n',
      '.ccs/.hook-migrated': 'existing migration marker\n',
      '.ccs/completions/ccs.bash': '# existing completion\n',
      '.claude/settings.json': '{"fixture":"existing Claude settings"}\n',
    };
    for (const [relative, contents] of Object.entries(files)) {
      const file = path.join(testHome, relative);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, contents, { mode: 0o600 });
    }
    const originalModes = Object.fromEntries(
      Object.keys(files).map((relative) => [
        relative,
        fs.statSync(path.join(testHome, relative)).mode,
      ])
    );
    for (const script of ['postinstall.js', 'postinstall.js', 'postuninstall.js']) {
      const result = run(script);
      assert.strictEqual(result.status, 0, result.stderr);
    }
    for (const [relative, contents] of Object.entries(files)) {
      const file = path.join(testHome, relative);
      assert.strictEqual(fs.readFileSync(file, 'utf8'), contents, relative);
      assert.strictEqual(fs.statSync(file).mode, originalModes[relative], relative);
    }
    assert(!fs.existsSync(path.join(testHome, '.ccs/shared')));
    assert(!fs.existsSync(path.join(testHome, '.ccs/config.json.bak')));
    assert(!fs.existsSync(path.join(testHome, '.ccs/uninstall.log')));
  });

  it('honors CCS_DIR without creating the legacy home directory', () => {
    const directory = path.join(testHome, 'custom-account-directory');
    const result = run('postinstall.js', { CCS_DIR: directory });
    assert.strictEqual(result.status, 0, result.stderr);
    assert.deepStrictEqual(fs.readdirSync(testHome), ['custom-account-directory']);
    assert.deepStrictEqual(fs.readdirSync(directory), []);
  });

  it('fails without replacing a file at the account directory path', () => {
    const file = path.join(testHome, '.ccs');
    fs.writeFileSync(file, 'preserve this file');
    const result = run('postinstall.js');
    assert.strictEqual(result.status, 1);
    assert.strictEqual(fs.readFileSync(file, 'utf8'), 'preserve this file');
  });

  it('fails without repairing a dangling account-directory symlink', () => {
    if (process.platform === 'win32') return;
    const link = path.join(testHome, '.ccs');
    fs.symlinkSync(path.join(testHome, 'missing'), link);
    const result = run('postinstall.js');
    assert.strictEqual(result.status, 1);
    assert(fs.lstatSync(link).isSymbolicLink());
    assert(!fs.existsSync(path.join(testHome, 'missing')));
  });

  it('uninstall alone does not create or remove user storage', () => {
    const result = run('postuninstall.js');
    assert.strictEqual(result.status, 0, result.stderr);
    assert.deepStrictEqual(fs.readdirSync(testHome), []);
  });
});
