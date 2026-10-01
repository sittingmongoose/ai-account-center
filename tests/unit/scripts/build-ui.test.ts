import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';

const { buildUi, assertSlintPin, assertLockedSlint } = require('../../../scripts/build-ui.js');
const { verifyBundle } = require('../../../scripts/verify-bundle.js');
const { validateUi } = require('../../../scripts/validate-ui.js');
const roots: string[] = [];

function fixture(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-slint-build-'));
  roots.push(root);
  const crate = path.join(root, 'web-dashboard');
  fs.mkdirSync(path.join(crate, 'public'), { recursive: true });
  fs.mkdirSync(path.join(crate, 'pkg'), { recursive: true });
  fs.mkdirSync(path.join(crate, 'tests'), { recursive: true });
  for (const name of ['data.test.mjs', 'analytics.test.mjs', 'activation-confirmation.test.mjs']) {
    fs.writeFileSync(path.join(crate, 'tests', name), '// Build-gate fixture');
  }
  fs.writeFileSync(
    path.join(crate, 'Cargo.toml'),
    '[dependencies]\nslint = { version = "=1.18.1" }\n[build-dependencies]\nslint-build = "=1.18.1"\n'
  );
  fs.writeFileSync(
    path.join(crate, 'Cargo.lock'),
    '[[package]]\nname = "slint"\nversion = "1.18.1"\n[[package]]\nname = "slint-build"\nversion = "1.18.1"\n'
  );
  fs.writeFileSync(
    path.join(crate, 'public', 'index.html'),
    '<canvas></canvas><script type="module" src="/bridge.js"></script>'
  );
  fs.writeFileSync(
    path.join(crate, 'public', 'bridge.js'),
    "import './pkg/ccs_account_dashboard.js';"
  );
  fs.writeFileSync(
    path.join(crate, 'pkg', 'ccs_account_dashboard.js'),
    'export default async function init() {}'
  );
  fs.writeFileSync(
    path.join(crate, 'pkg', 'ccs_account_dashboard_bg.wasm'),
    Buffer.from([0, 97, 115, 109, 1, 0, 0, 0])
  );
  fs.writeFileSync(path.join(crate, 'pkg', '.gitignore'), '*');
  return root;
}

function runner(calls: Array<[string, string[]]>, rust = 'rustc 1.98.0 (fixture)') {
  return (command: string, args: string[]) => {
    calls.push([command, args]);
    if (command.includes('rustc')) return rust;
    if (command.includes('rustup')) return 'wasm32-unknown-unknown\nx86_64-unknown-linux-gnu\n';
    return '';
  };
}

afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

describe('Slint browser build integration', () => {
  it('requires exact Slint runtime and compiler pins', () => {
    expect(() =>
      assertSlintPin({
        dependencies: { slint: '^1.18.1' },
        'build-dependencies': { 'slint-build': '=1.18.1' },
      })
    ).toThrow();
    expect(() => assertLockedSlint({ package: [{ name: 'slint', version: '1.18.2' }] })).toThrow();
  });

  it('builds locked browser output directly into the packaged dashboard', () => {
    const root = fixture();
    const calls: Array<[string, string[]]> = [];
    const manifest = buildUi({ repoRoot: root, run: runner(calls) });
    expect(manifest.version).toBe('1.18.1');
    expect(calls.at(-1)?.[1]).toEqual([
      'build',
      path.join(root, 'web-dashboard'),
      '--release',
      '--target',
      'web',
      '--out-dir',
      'pkg',
      '--out-name',
      'ccs_account_dashboard',
      '--',
      '--locked',
    ]);
    const output = path.join(root, 'dist', 'ui');
    expect(verifyBundle(output).manifest).toEqual(manifest);
    expect(fs.readFileSync(path.join(output, 'index.html'), 'utf8')).not.toContain('react');
    expect(fs.existsSync(path.join(output, 'pkg', '.gitignore'))).toBe(false);
    expect(fs.existsSync(path.join(root, 'ui'))).toBe(false);
  });

  it('rejects an older Rust compiler before modifying the existing browser output', () => {
    const root = fixture();
    const calls: Array<[string, string[]]> = [];
    const oldUi = path.join(root, 'dist', 'ui');
    fs.mkdirSync(oldUi, { recursive: true });
    fs.writeFileSync(path.join(oldUi, 'index.html'), 'existing');
    expect(() => buildUi({ repoRoot: root, run: runner(calls, 'rustc 1.91.0 (fixture)') })).toThrow(
      '1.92'
    );
    expect(fs.readFileSync(path.join(oldUi, 'index.html'), 'utf8')).toBe('existing');
  });

  it('detects a mismatched packaged Wasm binary', () => {
    const root = fixture();
    const calls: Array<[string, string[]]> = [];
    buildUi({ repoRoot: root, run: runner(calls) });
    const output = path.join(root, 'dist', 'ui');
    fs.appendFileSync(path.join(output, 'pkg', 'ccs_account_dashboard_bg.wasm'), Buffer.from([1]));
    expect(() => verifyBundle(output)).toThrow('does not match');
  });

  it('validates current Slint inputs, fixtures and artifacts without rebuilding', () => {
    const root = fixture();
    const calls: Array<[string, string[]]> = [];
    const manifest = buildUi({ repoRoot: root, run: runner(calls) });
    calls.length = 0;
    expect(validateUi({ repoRoot: root, run: runner(calls) })).toEqual(manifest.source);
    expect(calls).toHaveLength(3);
    expect(calls[0]?.[1]).toEqual([
      'fmt',
      '--manifest-path',
      path.join(root, 'web-dashboard', 'Cargo.toml'),
      '--check',
    ]);
    expect(calls[1]?.[1][0]).toBe('--test');
    expect(calls[1]?.[1].slice(1).map((file) => path.basename(file))).toEqual([
      'activation-confirmation.test.mjs',
      'analytics.test.mjs',
      'data.test.mjs',
    ]);
    expect(calls[2]?.[1][0]).toBe('--check');
    expect(calls.some(([command]) => command.includes('wasm-pack'))).toBe(false);
  });

  it('rejects a stale browser bridge after source changes even when Wasm is valid', () => {
    const root = fixture();
    const calls: Array<[string, string[]]> = [];
    buildUi({ repoRoot: root, run: runner(calls) });
    fs.appendFileSync(path.join(root, 'web-dashboard', 'public', 'bridge.js'), '\n// changed');
    expect(() => validateUi({ repoRoot: root, run: runner(calls) })).toThrow('source changed');
  });
});
