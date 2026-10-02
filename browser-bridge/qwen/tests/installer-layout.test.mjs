import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

// The Windows installer copies the plan collectors from the package's single
// scripts/account-usage copy. These checks pin that relative path to the real
// package layout, so a move fails here instead of at Windows install time.
const bridgeDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const packageRoot = path.resolve(bridgeDir, '..', '..');
const installer = fs.readFileSync(path.join(bridgeDir, 'install-native-host.ps1'), 'utf8');

function installerHelpers() {
  const source = /^\$SourceHelpers = \[IO\.Path\]::GetFullPath\(\(Join-Path \$SourceRoot '([^']+)'\)\)\r?$/m.exec(installer);
  assert.ok(source, 'the installer resolves $SourceHelpers from $SourceRoot');
  const names = /^\$HelperNames = @\(([^)]*)\)\r?$/m.exec(installer);
  assert.ok(names, 'the installer lists $HelperNames');
  return {
    dir: path.resolve(bridgeDir, ...source[1].split('\\')),
    names: [...names[1].matchAll(/'([^']+)'/g)].map((match) => match[1]),
  };
}

test('the installer reads the plan collectors from the package scripts/account-usage folder', () => {
  const helpers = installerHelpers();
  assert.equal(helpers.dir, path.join(packageRoot, 'scripts', 'account-usage'));
  assert.deepEqual(helpers.names, ['plan_common.py', 'plan_usage.py']);
  for (const name of helpers.names) {
    assert.ok(fs.statSync(path.join(helpers.dir, name)).isFile(), `${name} exists in scripts/account-usage`);
  }
});

test('the package ships the bridge and the collector folder together', () => {
  const pkg = JSON.parse(fs.readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  assert.ok(pkg.files.includes('scripts/'), 'package.json files ships scripts/');
  assert.ok(pkg.files.includes('browser-bridge/'), 'package.json files ships browser-bridge/');
});

test('the bridge keeps no second copy of the collectors', () => {
  for (const name of installerHelpers().names) {
    assert.equal(fs.existsSync(path.join(bridgeDir, 'helpers', name)), false, `no bridge copy of ${name}`);
  }
});
