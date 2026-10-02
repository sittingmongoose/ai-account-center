import { afterEach, describe, expect, it } from 'bun:test';
import crypto from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';
import zlib from 'zlib';

const {
  buildUi,
  assertSlintPin,
  assertLockedSlint,
  BRIDGE_IMPORT_ERROR,
} = require('../../../scripts/build-ui.js');
const { verifyBundle } = require('../../../scripts/verify-bundle.js');
const { validateUi } = require('../../../scripts/validate-ui.js');
const roots: string[] = [];
// A valid wasm header followed by a compressible body, large enough to precompress.
const WASM = Buffer.concat([
  Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]),
  Buffer.from('slint-dashboard-fixture-'.repeat(256)),
]);
const sha256 = (data: Buffer) => crypto.createHash('sha256').update(data).digest('hex');
const mode = (file: string) => fs.statSync(file).mode & 0o777;

function filesBelow(directory: string, relative = ''): string[] {
  return fs
    .readdirSync(path.join(directory, relative), { withFileTypes: true })
    .flatMap((entry) => {
      const child = relative ? `${relative}/${entry.name}` : entry.name;
      return entry.isDirectory() ? [child + '/', ...filesBelow(directory, child)] : [child];
    })
    .sort();
}

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
    `export default async function init() {}\n${'// wasm-bindgen glue fixture\n'.repeat(80)}`
  );
  fs.writeFileSync(path.join(crate, 'pkg', 'ccs_account_dashboard_bg.wasm'), WASM);
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
    const manifest = buildUi({ repoRoot: root, run: runner(calls) });
    const output = path.join(root, 'dist', 'ui');
    fs.appendFileSync(path.join(output, manifest.wasm.path), Buffer.from([1]));
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

  it('puts the wasm-pack output only in pkg/<buildId>/, named after the wasm hash', () => {
    const root = fixture();
    const manifest = buildUi({ repoRoot: root, run: runner([]), readCommit: () => null });
    const output = path.join(root, 'dist', 'ui');
    const buildId = sha256(WASM).slice(0, 12);
    expect(manifest.buildId).toBe(buildId);
    expect(manifest.wasm).toEqual({
      path: `pkg/${buildId}/ccs_account_dashboard_bg.wasm`,
      bytes: WASM.length,
      sha256: sha256(WASM),
    });
    expect(fs.readdirSync(path.join(output, 'pkg'))).toEqual([buildId]);
    expect(fs.existsSync(path.join(output, 'pkg', 'ccs_account_dashboard_bg.wasm'))).toBe(false);
    expect(fs.existsSync(path.join(output, 'pkg', 'ccs_account_dashboard.js'))).toBe(false);
    expect(fs.existsSync(path.join(output, 'pkg', buildId, '.gitignore'))).toBe(false);
    expect(fs.readFileSync(path.join(output, manifest.wasm.path)).equals(WASM)).toBe(true);
    expect(manifest).not.toHaveProperty('commit');
  });

  it('rewrites only the packaged bridge.js import and leaves the source bridge unchanged', () => {
    const root = fixture();
    const source = path.join(root, 'web-dashboard', 'public', 'bridge.js');
    const before = fs.readFileSync(source, 'utf8');
    const manifest = buildUi({ repoRoot: root, run: runner([]) });
    const packaged = fs.readFileSync(path.join(root, 'dist', 'ui', 'bridge.js'), 'utf8');
    expect(packaged).toBe(`import './pkg/${manifest.buildId}/ccs_account_dashboard.js';`);
    expect(fs.readFileSync(source, 'utf8')).toBe(before);
  });

  it.each([
    ['no import literal', "import './pkg/other.js';"],
    [
      'two import literals',
      "import './pkg/ccs_account_dashboard.js';\nimport('./pkg/ccs_account_dashboard.js');",
    ],
    ['a double-quoted import only', 'import "./pkg/ccs_account_dashboard.js";'],
  ])('fails the build on %s and keeps the existing output', (_name, bridge) => {
    const root = fixture();
    fs.writeFileSync(path.join(root, 'web-dashboard', 'public', 'bridge.js'), bridge);
    const oldUi = path.join(root, 'dist', 'ui');
    fs.mkdirSync(oldUi, { recursive: true });
    fs.writeFileSync(path.join(oldUi, 'index.html'), 'existing');
    expect(BRIDGE_IMPORT_ERROR).toBe(
      'bridge.js must import ./pkg/ccs_account_dashboard.js exactly once.'
    );
    expect(() => buildUi({ repoRoot: root, run: runner([]) })).toThrow(BRIDGE_IMPORT_ERROR);
    expect(fs.readFileSync(path.join(oldUi, 'index.html'), 'utf8')).toBe('existing');
  });

  it('precompresses large wasm and text files with byte-identical, listed variants', () => {
    const root = fixture();
    const publicDir = path.join(root, 'web-dashboard', 'public');
    // Large text (compressible), small text (below 1 KiB) and an incompressible JSON file.
    fs.writeFileSync(path.join(publicDir, 'large.mjs'), 'export const rows = [];\n'.repeat(200));
    fs.writeFileSync(path.join(publicDir, 'small.css'), 'body{margin:0}');
    fs.writeFileSync(path.join(publicDir, 'noise.json'), crypto.randomBytes(4096));
    fs.writeFileSync(path.join(publicDir, 'image.png'), Buffer.alloc(4096));
    const manifest = buildUi({ repoRoot: root, run: runner([]) });
    const output = path.join(root, 'dist', 'ui');
    const wasmPath = manifest.wasm.path;
    const gluePath = `pkg/${manifest.buildId}/ccs_account_dashboard.js`;
    expect(manifest.precompressed.map((row: { file: string }) => row.file).sort()).toEqual(
      [
        'large.mjs.br',
        'large.mjs.gz',
        `${gluePath}.br`,
        `${gluePath}.gz`,
        `${wasmPath}.br`,
        `${wasmPath}.gz`,
      ].sort()
    );
    for (const row of manifest.precompressed) {
      const variant = fs.readFileSync(path.join(output, row.file));
      const original = fs.readFileSync(path.join(output, row.path));
      expect(row.file).toBe(`${row.path}${row.encoding === 'br' ? '.br' : '.gz'}`);
      expect(row.bytes).toBe(variant.length);
      expect(row.sha256).toBe(sha256(variant));
      expect(variant.length).toBeLessThan(original.length * 0.9);
      const decoded =
        row.encoding === 'br' ? zlib.brotliDecompressSync(variant) : zlib.gunzipSync(variant);
      expect(decoded.equals(original)).toBe(true);
    }
    for (const skipped of ['small.css', 'noise.json', 'image.png', 'index.html', 'bridge.js']) {
      expect(fs.existsSync(path.join(output, `${skipped}.br`))).toBe(false);
      expect(fs.existsSync(path.join(output, `${skipped}.gz`))).toBe(false);
    }
    // gzip carries mtime 0, so a second build of the same input gives the same bytes.
    const firstGz = fs.readFileSync(path.join(output, `${wasmPath}.gz`));
    expect(firstGz.readUInt32LE(4)).toBe(0);
    const firstBr = fs.readFileSync(path.join(output, `${wasmPath}.br`));
    const second = buildUi({ repoRoot: root, run: runner([]) });
    expect(fs.readFileSync(path.join(output, `${wasmPath}.gz`)).equals(firstGz)).toBe(true);
    expect(fs.readFileSync(path.join(output, `${wasmPath}.br`)).equals(firstBr)).toBe(true);
    expect(second.precompressed).toEqual(manifest.precompressed);
    expect(verifyBundle(output).manifest).toEqual(second);
  });

  it('normalizes every packaged folder to 0755 and file to 0644', () => {
    if (process.platform === 'win32') return;
    const root = fixture();
    const publicDir = path.join(root, 'web-dashboard', 'public');
    fs.mkdirSync(path.join(publicDir, 'assets'), { mode: 0o700 });
    fs.writeFileSync(path.join(publicDir, 'analytics-data.mjs'), 'export {};', { mode: 0o600 });
    fs.writeFileSync(path.join(publicDir, 'assets', 'mark.svg'), '<svg/>', { mode: 0o600 });
    fs.chmodSync(path.join(publicDir, 'analytics-data.mjs'), 0o600);
    fs.chmodSync(path.join(publicDir, 'assets', 'mark.svg'), 0o600);
    fs.chmodSync(path.join(publicDir, 'assets'), 0o700);
    fs.chmodSync(path.join(root, 'web-dashboard', 'pkg', 'ccs_account_dashboard.js'), 0o600);
    buildUi({ repoRoot: root, run: runner([]) });
    const output = path.join(root, 'dist', 'ui');
    const entries = filesBelow(output);
    expect(entries).toContain('assets/mark.svg');
    expect(entries).toContain('analytics-data.mjs');
    expect(mode(output)).toBe(0o755);
    for (const entry of entries) {
      expect([entry, mode(path.join(output, entry))]).toEqual([
        entry,
        entry.endsWith('/') ? 0o755 : 0o644,
      ]);
    }
  });

  it('records a valid build commit and omits anything else', () => {
    const root = fixture();
    expect(buildUi({ repoRoot: root, run: runner([]), readCommit: () => '8fb5e3de' }).commit).toBe(
      '8fb5e3de'
    );
    for (const value of ['', 'HEAD', '8FB5E3DE', '8fb5e3de\n--x', null]) {
      expect(
        buildUi({ repoRoot: root, run: runner([]), readCommit: () => value })
      ).not.toHaveProperty('commit');
    }
  });

  it('rejects tampered, stray, unversioned or wrongly permissioned packaged files', () => {
    const cases: Array<[string, (output: string, manifest: any) => void, string]> = [
      [
        'tampered variant',
        (output, manifest) =>
          fs.appendFileSync(path.join(output, manifest.precompressed[0].file), Buffer.from([0])),
        'precompressed',
      ],
      [
        'stray variant',
        (output) => fs.writeFileSync(path.join(output, 'index.html.gz'), zlib.gzipSync('x')),
        'precompressed',
      ],
      [
        'unversioned runtime',
        (output) =>
          fs.writeFileSync(path.join(output, 'pkg', 'ccs_account_dashboard_bg.wasm'), WASM),
        'unversioned',
      ],
      [
        'unrewritten bridge',
        (output) =>
          fs.writeFileSync(
            path.join(output, 'bridge.js'),
            "import './pkg/ccs_account_dashboard.js';"
          ),
        'versioned WebAssembly runtime',
      ],
      [
        'private file mode',
        (output) =>
          process.platform === 'win32'
            ? fs.writeFileSync(path.join(output, 'index.html.br'), 'x')
            : fs.chmodSync(path.join(output, 'index.html'), 0o600),
        process.platform === 'win32' ? 'precompressed' : '0644',
      ],
    ];
    for (const [, damage, message] of cases) {
      const root = fixture();
      const manifest = buildUi({ repoRoot: root, run: runner([]) });
      const output = path.join(root, 'dist', 'ui');
      expect(() => verifyBundle(output)).not.toThrow();
      damage(output, manifest);
      expect(() => verifyBundle(output)).toThrow(message);
    }
  });
});
