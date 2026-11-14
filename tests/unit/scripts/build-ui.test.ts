import { afterAll, afterEach, beforeAll, describe, expect, it } from 'bun:test';
import { spawnSync } from 'child_process';
import crypto from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';
import zlib from 'zlib';

const {
  buildUi,
  localRustBuildRefusal,
  readBuildCommit,
  assertLockedLeptos,
  assertLockedWasmBindgen,
  leptosFingerprint,
  fingerprintInputs,
  normalizedFingerprintPaths,
  SW_BUILD_ID_ERROR,
} = require('../../../scripts/build-ui.js');
const { verifyBundle } = require('../../../scripts/verify-bundle.js');
const { validateUi } = require('../../../scripts/validate-ui.js');
const roots: string[] = [];
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

/** One custom wasm section: id 0, a two-byte size, the name and its body. */
function section(name: string, body: Buffer): Buffer {
  const payload = Buffer.concat([Buffer.from([name.length]), Buffer.from(name), body]);
  return Buffer.concat([
    Buffer.from([0, (payload.length & 0x7f) | 0x80, payload.length >> 7]),
    payload,
  ]);
}
/** The module the build ships: a wasm header and one section to keep, compressible enough to precompress. */
const STRIPPED = Buffer.concat([
  Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]),
  section('target_features', Buffer.from('leptos-fixture-'.repeat(120))),
]);
/** What the fake wasm-pack leaves: the same module with a large debug "name" section, which the build strips. */
const BUILT = Buffer.concat([STRIPPED, section('name', Buffer.alloc(900, 7))]);
const LEPTOS_LOCK =
  'version = 4\n\n[[package]]\nname = "leptos"\nversion = "0.9.0-rc1"\n[[package]]\nname = "wasm-bindgen"\nversion = "0.2.130"\n[[package]]\nname = "wasm-bindgen-futures"\nversion = "0.4.80"\n[[package]]\nname = "js-sys"\nversion = "0.3.107"\n[[package]]\nname = "web-sys"\nversion = "0.3.107"\n';

/** A checkout with the dashboard's public files and a Leptos crate whose pkg/ the fake wasm-pack has "built". */
function fixture(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-leptos-build-'));
  roots.push(root);
  const crate = path.join(root, 'web-dashboard');
  const leptos = path.join(crate, 'leptos');
  for (const dir of [
    'public/icons',
    'tests',
    'leptos/src',
    'leptos/style',
    'leptos/build',
    'leptos/fonts',
    'leptos/pkg',
  ])
    fs.mkdirSync(path.join(crate, dir), { recursive: true });
  for (const name of ['data.test.mjs', 'analytics.test.mjs', 'activation-confirmation.test.mjs']) {
    fs.writeFileSync(path.join(crate, 'tests', name), '// Build-gate fixture');
  }
  // The installable shell: the worker carries the build placeholder, the manifest names one fixture icon, and the icon
  // file exists.
  fs.writeFileSync(
    path.join(crate, 'public', 'sw.js'),
    "importScripts('./sw-route.js');\nvar BUILD_ID = '__AAC_BUILD_ID__';\n"
  );
  fs.writeFileSync(path.join(crate, 'public', 'sw-route.js'), '// routing fixture\n');
  fs.writeFileSync(path.join(crate, 'public', 'view-model.mjs'), 'export const rows = [];\n');
  fs.writeFileSync(
    path.join(crate, 'public', 'manifest.webmanifest'),
    JSON.stringify({
      name: 'AI Account Center',
      short_name: 'AAC',
      start_url: '/',
      display: 'standalone',
      icons: [{ src: '/icons/fixture-192.png', sizes: '192x192', type: 'image/png' }],
    })
  );
  fs.writeFileSync(
    path.join(crate, 'public', 'icons', 'fixture-192.png'),
    Buffer.from(
      'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==',
      'base64'
    )
  );
  fs.writeFileSync(path.join(leptos, 'Cargo.toml'), '[package]\nname = "aac-web"\n');
  fs.writeFileSync(path.join(leptos, 'Cargo.lock'), LEPTOS_LOCK);
  fs.writeFileSync(path.join(leptos, 'src', 'lib.rs'), '// fixture\n');
  fs.writeFileSync(path.join(leptos, 'style', 'app.css'), 'body{margin:0}\n'.repeat(80));
  fs.writeFileSync(
    path.join(leptos, 'index.html'),
    `<!doctype html><link rel="stylesheet" href="{{CSS}}"><link rel="modulepreload" href="{{PKG}}/aac_web.js"><script type="module">import init from '{{PKG}}/aac_web.js'; init({ module_or_path: '{{PKG}}/aac_web_bg.wasm' });</script>${'<!-- page -->'.repeat(80)}`
  );
  fs.copyFileSync(
    path.join(__dirname, '../../../web-dashboard/leptos/build/strip-names.cjs'),
    path.join(leptos, 'build', 'strip-names.cjs')
  );
  fs.writeFileSync(path.join(leptos, 'fonts', 'MartianMono.ttf'), 'font');
  fs.writeFileSync(path.join(leptos, 'fonts', 'OFL-MartianMono.txt'), 'license');
  fs.writeFileSync(path.join(leptos, 'pkg', 'aac_web_bg.wasm'), BUILT);
  fs.writeFileSync(
    path.join(leptos, 'pkg', 'aac_web.js'),
    `export default async function init() {}\n${'// wasm-bindgen glue fixture\n'.repeat(80)}`
  );
  fs.writeFileSync(path.join(leptos, 'pkg', '.gitignore'), '*');
  fs.writeFileSync(path.join(leptos, 'pkg', 'package.json'), '{}');
  return root;
}

function runner(calls: Array<[string, string[]]>, rust = 'rustc 1.99.1 (fixture)') {
  return (command: string, args: string[]) => {
    calls.push([command, args]);
    if (command.includes('rustc')) return rust;
    if (command.includes('rustup')) return 'wasm32-unknown-unknown\nx86_64-unknown-linux-gnu\n';
    if (command.includes('wasm-pack') && args[0] === '--version') return 'wasm-pack 0.15.1\n';
    return '';
  };
}

// The build's computer-wide lock and shared cargo target live under a private cache here.
const savedCache = process.env.XDG_CACHE_HOME;
const savedTarget = process.env.CARGO_TARGET_DIR;
let cacheHome = '';
beforeAll(() => {
  cacheHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-leptos-cache-'));
  process.env.XDG_CACHE_HOME = cacheHome;
  delete process.env.CARGO_TARGET_DIR;
});
afterAll(() => {
  if (savedCache === undefined) delete process.env.XDG_CACHE_HOME;
  else process.env.XDG_CACHE_HOME = savedCache;
  if (savedTarget !== undefined) process.env.CARGO_TARGET_DIR = savedTarget;
  fs.rmSync(cacheHome, { recursive: true, force: true });
});
/** The owner-file lock runs wasm-pack directly, so the recorded calls match on every platform. */
const build = (options: Record<string, unknown>) =>
  buildUi({ lock: { useFlock: false }, ...options });

afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

/** A dist/ui with one file, as a previous build leaves it: a failed build must keep it. */
function existingOutput(root: string): string {
  const oldUi = path.join(root, 'dist', 'ui');
  fs.mkdirSync(path.join(oldUi, 'app'), { recursive: true });
  fs.writeFileSync(path.join(oldUi, 'app', 'index.html'), 'existing');
  return path.join(oldUi, 'app', 'index.html');
}

describe('the Leptos dashboard build', () => {
  it('builds, strips and content-addresses the app into the packaged dashboard', () => {
    const root = fixture();
    const calls: Array<[string, string[]]> = [];
    const manifest = build({ repoRoot: root, run: runner(calls) });
    expect(calls.find(([, args]) => args[0] === 'build')?.[1]).toEqual([
      'build',
      path.join(root, 'web-dashboard', 'leptos'),
      '--release',
      '--target',
      'web',
      '--out-dir',
      'pkg',
      '--out-name',
      'aac_web',
      '--',
      '--locked',
    ]);
    const id = sha256(STRIPPED).slice(0, 12);
    expect(manifest).toMatchObject({
      schemaVersion: 2,
      framework: 'leptos',
      version: '0.9.0-rc1',
      target: 'wasm32-unknown-unknown',
      entry: 'app/index.html',
      buildId: id,
      wasm: { path: `pkg/${id}/aac_web_bg.wasm`, bytes: STRIPPED.length, sha256: sha256(STRIPPED) },
    });
    expect(manifest).not.toHaveProperty('leptos');
    const ui = path.join(root, 'dist', 'ui');
    // The debug name section is gone and nothing else changed.
    expect(fs.readFileSync(path.join(ui, manifest.wasm.path)).equals(STRIPPED)).toBe(true);
    const page = fs.readFileSync(path.join(ui, 'app', 'index.html'), 'utf8');
    expect(page).toContain(`'/pkg/${id}/aac_web.js'`);
    expect(page).toContain(`'/pkg/${id}/aac_web_bg.wasm'`);
    expect(page).not.toContain('{{PKG}}');
    expect(page).not.toContain('{{CSS}}');
    expect(page).toMatch(/href="\/app\/app\.css\?v=[0-9a-f]{12}"/);
    for (const file of [
      'app/app.css',
      'app/MartianMono.ttf',
      'app/OFL-MartianMono.txt',
      `pkg/${id}/aac_web.js`,
    ]) {
      expect(fs.existsSync(path.join(ui, file))).toBe(true);
    }
    for (const file of ['.gitignore', 'package.json'])
      expect(fs.existsSync(path.join(ui, 'pkg', id, file))).toBe(false);
    // no classic page, bridge or Slint runtime
    for (const file of ['index.html', 'bridge.js'])
      expect(fs.existsSync(path.join(ui, file))).toBe(false);
    expect(
      manifest.precompressed.some((entry: { path: string }) => entry.path === 'app/index.html')
    ).toBe(true);
    expect(mode(path.join(ui, 'app'))).toBe(0o755);
    expect(mode(path.join(ui, 'app', 'index.html'))).toBe(0o644);
    expect(verifyBundle(ui).manifest).toEqual(manifest);
  });

  it('refuses a source change after the build', () => {
    const root = fixture();
    build({ repoRoot: root, run: runner([]) });
    expect(() => validateUi({ repoRoot: root, run: runner([]) })).not.toThrow();
    fs.appendFileSync(
      path.join(root, 'web-dashboard', 'leptos', 'style', 'app.css'),
      'p{color:red}\n'
    );
    expect(() => validateUi({ repoRoot: root, run: runner([]) })).toThrow(
      'Dashboard source changed after the build'
    );
  });

  it('refuses a changed shared computer registry after the build', () => {
    const root = fixture();
    const registry = path.join(root, 'web-dashboard', 'public', 'computers.json');
    fs.writeFileSync(registry, '{}');
    build({ repoRoot: root, run: runner([]) });
    expect(() => validateUi({ repoRoot: root, run: runner([]) })).not.toThrow();
    fs.writeFileSync(registry, '{"future":{}}');
    expect(() => validateUi({ repoRoot: root, run: runner([]) })).toThrow('source changed');
  });

  it('requires Cargo.lock to resolve the pinned Leptos and wasm-bindgen versions', () => {
    expect(() =>
      assertLockedLeptos({ package: [{ name: 'leptos', version: '0.9.0-rc1' }] })
    ).not.toThrow();
    expect(() => assertLockedLeptos({ package: [{ name: 'leptos', version: '0.9.0' }] })).toThrow(
      'must resolve leptos to 0.9.0-rc1'
    );
    expect(() => assertLockedLeptos({ package: [] })).toThrow('must resolve leptos');
    expect(() =>
      assertLockedWasmBindgen({ package: [{ name: 'wasm-bindgen', version: '0.2.129' }] }, 'Leptos')
    ).toThrow('Leptos Cargo.lock must resolve wasm-bindgen to 0.2.130');
    const root = fixture();
    fs.writeFileSync(
      path.join(root, 'web-dashboard', 'leptos', 'Cargo.lock'),
      LEPTOS_LOCK.replace('0.9.0-rc1', '0.8.23')
    );
    const before = existingOutput(root);
    expect(() => build({ repoRoot: root, run: runner([]) })).toThrow('must resolve leptos');
    expect(fs.readFileSync(before, 'utf8')).toBe('existing');
  });

  it('refuses a linked crate root, a linked parent folder and a dangling link among the inputs', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-leptos-links-'));
    roots.push(root);
    const real = path.join(root, 'real');
    for (const dir of ['src', 'elsewhere']) fs.mkdirSync(path.join(real, dir), { recursive: true });
    fs.writeFileSync(path.join(real, 'Cargo.toml'), '[package]\n');
    fs.writeFileSync(path.join(real, 'src', 'lib.rs'), '// page\n');
    fs.writeFileSync(path.join(real, 'elsewhere', 'config.toml'), '[build]\n');
    expect(leptosFingerprint(real).files).toBe(2);
    // the crate folder itself is a link
    fs.symlinkSync(real, path.join(root, 'linked'));
    expect(() => leptosFingerprint(path.join(root, 'linked'))).toThrow('must be a real folder');
    // .cargo is a link to another folder: path.join would follow it to .cargo/config.toml
    fs.symlinkSync(path.join(real, 'elsewhere'), path.join(real, '.cargo'));
    expect(() => leptosFingerprint(real)).toThrow('must not be symlinks');
    fs.unlinkSync(path.join(real, '.cargo'));
    // a dangling link is refused, not skipped
    fs.symlinkSync(path.join(root, 'nowhere.rs'), path.join(real, 'src', 'gone.rs'));
    expect(() => leptosFingerprint(real)).toThrow('must not be symlinks');
  });

  it('refuses a linked shared registry or its linked public folder', () => {
    const root = fixture();
    const crate = path.join(root, 'web-dashboard');
    const registry = path.join(crate, 'public', 'computers.json');
    fs.symlinkSync(path.join(root, 'missing.json'), registry);
    expect(() => leptosFingerprint(path.join(crate, 'leptos'))).toThrow('must not be symlinks');
    fs.unlinkSync(registry);
    fs.renameSync(path.join(crate, 'public'), path.join(crate, 'assets'));
    fs.symlinkSync(path.join(crate, 'assets'), path.join(crate, 'public'));
    expect(() => leptosFingerprint(path.join(crate, 'leptos'))).toThrow('must not be symlinks');
  });

  it('refuses a missing or dangling crate instead of building nothing', () => {
    const root = fixture();
    build({ repoRoot: root, run: runner([]) });
    expect(() => validateUi({ repoRoot: root, run: runner([]) })).not.toThrow();
    const leptos = path.join(root, 'web-dashboard', 'leptos');
    fs.renameSync(leptos, path.join(root, 'moved'));
    expect(() => build({ repoRoot: root, run: runner([]) })).toThrow('must be a real folder');
    fs.symlinkSync(path.join(root, 'nowhere'), leptos);
    expect(() => build({ repoRoot: root, run: runner([]) })).toThrow();
    expect(() => validateUi({ repoRoot: root, run: runner([]) })).toThrow();
  });

  it('rejects an older Rust compiler before modifying the existing browser output', () => {
    const root = fixture();
    const before = existingOutput(root);
    expect(() => build({ repoRoot: root, run: runner([], 'rustc 1.91.0 (fixture)') })).toThrow(
      '1.99.1'
    );
    expect(fs.readFileSync(before, 'utf8')).toBe('existing');
  });

  it('rejects an outdated wasm-pack before replacing the packaged dashboard', () => {
    const root = fixture();
    const calls: Array<[string, string[]]> = [];
    const run = runner(calls);
    const before = existingOutput(root);
    expect(() =>
      build({
        repoRoot: root,
        run: (command: string, args: string[]) =>
          command.includes('wasm-pack') && args[0] === '--version'
            ? 'wasm-pack 0.13.1\n'
            : run(command, args),
      })
    ).toThrow('requires wasm-pack 0.15.1');
    expect(calls.some(([, args]) => args[0] === 'build')).toBe(false);
    expect(fs.readFileSync(before, 'utf8')).toBe('existing');
  });

  it('uses the crate toolchain when the computer default compiler is older', () => {
    const root = fixture();
    const leptos = path.join(root, 'web-dashboard', 'leptos');
    const run = runner([]);
    const pinnedRun = (command: string, args: string[], options: { cwd?: string }) => {
      if (command.includes('rustc') && options.cwd !== leptos) return 'rustc 1.93.0 (default)';
      if (command.includes('cargo') && args[0] === 'fmt' && options.cwd !== leptos)
        throw new Error('Formatter did not use the crate toolchain');
      return run(command, args);
    };
    expect(() => build({ repoRoot: root, run: pinnedRun })).not.toThrow();
    expect(() => validateUi({ repoRoot: root, run: pinnedRun })).not.toThrow();
  });

  it('invalidates packaged output when Wasm compiler features or the toolchain change', () => {
    for (const [file, first, next] of [
      [
        '.cargo/config.toml',
        '[target.wasm32-unknown-unknown]\nrustflags = []\n',
        '[target.wasm32-unknown-unknown]\nrustflags = ["-C", "target-feature=+simd128"]\n',
      ],
      [
        'rust-toolchain.toml',
        '[toolchain]\nchannel = "1.99.1"\n',
        '[toolchain]\nchannel = "1.99.2"\n',
      ],
    ]) {
      const root = fixture();
      const config = path.join(root, 'web-dashboard', 'leptos', file);
      fs.mkdirSync(path.dirname(config), { recursive: true });
      fs.writeFileSync(config, first);
      build({ repoRoot: root, run: runner([]) });
      expect(() => validateUi({ repoRoot: root, run: runner([]) })).not.toThrow();
      fs.writeFileSync(config, next);
      expect(() => validateUi({ repoRoot: root, run: runner([]) })).toThrow(
        'Dashboard source changed after the build'
      );
    }
  });

  it('detects a mismatched packaged Wasm binary', () => {
    const root = fixture();
    const manifest = build({ repoRoot: root, run: runner([]) });
    const output = path.join(root, 'dist', 'ui');
    fs.appendFileSync(path.join(output, manifest.wasm.path), Buffer.from([1]));
    expect(() => verifyBundle(output)).toThrow('does not match');
  });

  it('validates current inputs, fixtures and artifacts without rebuilding', () => {
    const root = fixture();
    const calls: Array<[string, string[]]> = [];
    const manifest = build({ repoRoot: root, run: runner(calls) });
    calls.length = 0;
    expect(validateUi({ repoRoot: root, run: runner(calls) })).toEqual(manifest.source);
    expect(calls).toHaveLength(2);
    expect(calls[0]?.[1]).toEqual([
      'fmt',
      '--manifest-path',
      path.join(root, 'web-dashboard', 'leptos', 'Cargo.toml'),
      '--check',
    ]);
    expect(calls[1]?.[1][0]).toBe('--test');
    expect(calls[1]?.[1].slice(1).map((file) => path.basename(file))).toEqual([
      'activation-confirmation.test.mjs',
      'analytics.test.mjs',
      'data.test.mjs',
    ]);
    expect(calls.some(([command]) => command.includes('wasm-pack'))).toBe(false);
  });

  it('rejects a stale shell after a public file changes even when the wasm is valid', () => {
    const root = fixture();
    build({ repoRoot: root, run: runner([]) });
    fs.appendFileSync(path.join(root, 'web-dashboard', 'public', 'view-model.mjs'), '\n// changed');
    expect(() => validateUi({ repoRoot: root, run: runner([]) })).toThrow(
      'Dashboard shell changed after the build'
    );
  });

  it('puts the wasm-pack output only in pkg/<buildId>/, named after the wasm hash', () => {
    const root = fixture();
    const manifest = build({ repoRoot: root, run: runner([]), readCommit: () => null });
    const output = path.join(root, 'dist', 'ui');
    const buildId = sha256(STRIPPED).slice(0, 12);
    expect(manifest.buildId).toBe(buildId);
    expect(manifest.wasm).toEqual({
      path: `pkg/${buildId}/aac_web_bg.wasm`,
      bytes: STRIPPED.length,
      sha256: sha256(STRIPPED),
    });
    expect(fs.readdirSync(path.join(output, 'pkg'))).toEqual([buildId]);
    expect(fs.existsSync(path.join(output, 'pkg', 'aac_web_bg.wasm'))).toBe(false);
    expect(fs.existsSync(path.join(output, 'pkg', 'aac_web.js'))).toBe(false);
    expect(fs.readFileSync(path.join(output, manifest.wasm.path)).equals(STRIPPED)).toBe(true);
    expect(manifest).not.toHaveProperty('commit');
  });

  it('refuses a page template without its runtime and stylesheet placeholders, and keeps the existing output', () => {
    const root = fixture();
    fs.writeFileSync(
      path.join(root, 'web-dashboard', 'leptos', 'index.html'),
      '<!doctype html><script src="/pkg/aac_web.js"></script>'
    );
    const before = existingOutput(root);
    expect(() => build({ repoRoot: root, run: runner([]) })).toThrow(
      'must reference {{PKG}} and {{CSS}}'
    );
    expect(fs.readFileSync(before, 'utf8')).toBe('existing');
  });

  it('stamps the shell build id into the packaged sw.js and leaves the source worker unchanged', () => {
    const root = fixture();
    const source = path.join(root, 'web-dashboard', 'public', 'sw.js');
    const before = fs.readFileSync(source, 'utf8');
    const manifest = build({ repoRoot: root, run: runner([]) });
    const packaged = fs.readFileSync(path.join(root, 'dist', 'ui', 'sw.js'), 'utf8');
    expect(packaged).toBe(
      `importScripts('./sw-route.js');\nvar BUILD_ID = '${manifest.shellBuildId}';\n`
    );
    expect(fs.readFileSync(source, 'utf8')).toBe(before);
  });

  it('rolls the service worker on any shell change while pkg/ stays wasm-addressed', () => {
    const root = fixture();
    const swPath = path.join(root, 'dist', 'ui', 'sw.js');
    const first = build({ repoRoot: root, run: runner([]), readCommit: () => null });
    const swFirst = fs.readFileSync(swPath, 'utf8');
    expect(first.shellBuildId).toMatch(/^[a-f0-9]{12}$/);
    expect(first.shellBuildId).toBe(first.shell.sha256.slice(0, 12));
    // Identical inputs: identical stamp, byte-identical worker.
    const second = build({ repoRoot: root, run: runner([]), readCommit: () => null });
    expect(second.shellBuildId).toBe(first.shellBuildId);
    expect(fs.readFileSync(swPath, 'utf8')).toBe(swFirst);
    // A JS-only change rolls the worker; the wasm id does not move.
    fs.writeFileSync(
      path.join(root, 'web-dashboard', 'public', 'view-model.mjs'),
      'export const a = 1;\n'
    );
    const third = build({ repoRoot: root, run: runner([]), readCommit: () => null });
    expect(third.buildId).toBe(first.buildId);
    expect(third.shellBuildId).not.toBe(first.shellBuildId);
    expect(fs.readFileSync(swPath, 'utf8')).not.toBe(swFirst);
    // A stylesheet change rolls it too (the frame page names its stylesheet by content).
    fs.appendFileSync(path.join(root, 'web-dashboard', 'leptos', 'style', 'app.css'), 'p{}\n');
    const fourth = build({ repoRoot: root, run: runner([]), readCommit: () => null });
    expect(fourth.shellBuildId).not.toBe(third.shellBuildId);
    // The validator agrees the new output matches the new shell inputs.
    expect(() => validateUi({ repoRoot: root, run: runner([]) })).not.toThrow();
  });

  it('fails the build on a sw.js without the placeholder and keeps the existing output', () => {
    const root = fixture();
    fs.writeFileSync(
      path.join(root, 'web-dashboard', 'public', 'sw.js'),
      "var BUILD_ID = 'already-stamped';\n"
    );
    const before = existingOutput(root);
    expect(SW_BUILD_ID_ERROR).toBe(
      'sw.js must name the __AAC_BUILD_ID__ placeholder exactly once.'
    );
    expect(() => build({ repoRoot: root, run: runner([]) })).toThrow(SW_BUILD_ID_ERROR);
    expect(fs.readFileSync(before, 'utf8')).toBe('existing');
  });

  it('leaves a fixture public dir without sw.js alone', () => {
    const root = fixture();
    fs.rmSync(path.join(root, 'web-dashboard', 'public', 'sw.js'));
    const manifest = build({ repoRoot: root, run: runner([]) });
    expect(manifest.buildId).toMatch(/^[a-f0-9]{12}$/);
    expect(fs.existsSync(path.join(root, 'dist', 'ui', 'sw.js'))).toBe(false);
  });

  it('precompresses large wasm and text files with byte-identical, listed variants', () => {
    const root = fixture();
    const publicDir = path.join(root, 'web-dashboard', 'public');
    // Large text (compressible), small text (below 1 KiB) and an incompressible JSON file.
    fs.writeFileSync(path.join(publicDir, 'large.mjs'), 'export const rows = [];\n'.repeat(200));
    fs.writeFileSync(path.join(publicDir, 'small.css'), 'body{margin:0}');
    fs.writeFileSync(path.join(publicDir, 'noise.json'), crypto.randomBytes(4096));
    fs.writeFileSync(path.join(publicDir, 'image.png'), Buffer.alloc(4096));
    const manifest = build({ repoRoot: root, run: runner([]) });
    const output = path.join(root, 'dist', 'ui');
    const wasmPath = manifest.wasm.path;
    const gluePath = `pkg/${manifest.buildId}/aac_web.js`;
    expect(manifest.precompressed.map((row: { file: string }) => row.file).sort()).toEqual(
      [
        'large.mjs.br',
        'large.mjs.gz',
        'app/index.html.br',
        'app/index.html.gz',
        'app/app.css.br',
        'app/app.css.gz',
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
    for (const skipped of ['small.css', 'noise.json', 'image.png', 'sw.js']) {
      expect(fs.existsSync(path.join(output, `${skipped}.br`))).toBe(false);
      expect(fs.existsSync(path.join(output, `${skipped}.gz`))).toBe(false);
    }
    // gzip carries mtime 0, so a second build of the same input gives the same bytes.
    const firstGz = fs.readFileSync(path.join(output, `${wasmPath}.gz`));
    expect(firstGz.readUInt32LE(4)).toBe(0);
    const firstBr = fs.readFileSync(path.join(output, `${wasmPath}.br`));
    const second = build({ repoRoot: root, run: runner([]) });
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
    fs.chmodSync(path.join(root, 'web-dashboard', 'leptos', 'pkg', 'aac_web.js'), 0o600);
    build({ repoRoot: root, run: runner([]) });
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
    expect(build({ repoRoot: root, run: runner([]), readCommit: () => '8fb5e3de' }).commit).toBe(
      '8fb5e3de'
    );
    for (const value of ['', 'HEAD', '8FB5E3DE', '8fb5e3de\n--x', null]) {
      expect(
        build({ repoRoot: root, run: runner([]), readCommit: () => value })
      ).not.toHaveProperty('commit');
    }
  });

  it('reads the commit only when the build root is itself the top of a checkout', () => {
    const repository = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-build-commit-'));
    roots.push(repository);
    const git = (...args: string[]) =>
      spawnSync('git', ['-C', repository, ...args], { encoding: 'utf8' });
    expect(readBuildCommit(repository)).toBeNull(); // not a checkout at all
    expect(git('init', '-q').status).toBe(0);
    expect(
      git(
        '-c',
        'user.name=Fixture',
        '-c',
        'user.email=fixture@example.com',
        '-c',
        'commit.gpgsign=false',
        '-c',
        'core.hooksPath=/dev/null',
        'commit',
        '-q',
        '--allow-empty',
        '-m',
        'fixture'
      ).status
    ).toBe(0);
    const head = git('rev-parse', '--short=8', 'HEAD').stdout.trim();
    expect(head).toMatch(/^[a-f0-9]{8}$/);
    expect(readBuildCommit(repository)).toBe(head);
    // A package unpacked inside some other repository must not record that repository's commit.
    const nested = path.join(repository, 'unpacked', 'package');
    fs.mkdirSync(nested, { recursive: true });
    expect(readBuildCommit(nested)).toBeNull();
  });

  it('rejects tampered, stray, unversioned, retired or wrongly permissioned packaged files', () => {
    const cases: Array<[string, (output: string, manifest: any) => void, string]> = [
      [
        'tampered variant',
        (output, manifest) =>
          fs.appendFileSync(path.join(output, manifest.precompressed[0].file), Buffer.from([0])),
        'precompressed',
      ],
      [
        'stray variant',
        (output) => fs.writeFileSync(path.join(output, 'sw.js.gz'), zlib.gzipSync('x')),
        'precompressed',
      ],
      [
        'unversioned runtime',
        (output) => fs.writeFileSync(path.join(output, 'pkg', 'aac_web_bg.wasm'), STRIPPED),
        'unversioned',
      ],
      [
        'a frame page that starts another runtime',
        (output) =>
          fs.writeFileSync(
            path.join(output, 'app', 'index.html'),
            "<script>import('/pkg/000000000000/aac_web.js')</script>"
          ),
        'versioned WebAssembly runtime',
      ],
      [
        'the retired classic page',
        (output) =>
          fs.writeFileSync(path.join(output, 'index.html'), '<canvas id="canvas"></canvas>'),
        'retired Slint app',
      ],
      [
        'the retired Slint runtime',
        (output, manifest) =>
          fs.writeFileSync(
            path.join(output, 'pkg', manifest.buildId, 'ccs_account_dashboard_bg.wasm'),
            STRIPPED
          ),
        'retired Slint app',
      ],
      [
        'unstamped worker',
        (output) =>
          fs.writeFileSync(
            path.join(output, 'sw.js'),
            "importScripts('./sw-route.js');\nvar BUILD_ID = '__AAC_BUILD_ID__';\n"
          ),
        'stamped shell build id',
      ],
      [
        'missing mono font',
        (output) => fs.rmSync(path.join(output, 'app', 'MartianMono.ttf')),
        'Missing browser asset',
      ],
      [
        'manifest icon removed',
        (output) => fs.rmSync(path.join(output, 'icons', 'fixture-192.png')),
        'Missing browser asset',
      ],
      [
        'private file mode',
        (output) =>
          process.platform === 'win32'
            ? fs.writeFileSync(path.join(output, 'app', 'app.css.br'), 'x')
            : fs.chmodSync(path.join(output, 'app', 'index.html'), 0o600),
        process.platform === 'win32' ? 'precompressed' : '0644',
      ],
    ];
    for (const [, damage, message] of cases) {
      const root = fixture();
      const manifest = build({ repoRoot: root, run: runner([]) });
      const output = path.join(root, 'dist', 'ui');
      expect(() => verifyBundle(output)).not.toThrow();
      damage(output, manifest);
      expect(() => verifyBundle(output)).toThrow(message);
    }
  });
});

it('normalizes Windows folder paths before sorting mixed file and folder inputs', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-fingerprint-'));
  roots.push(root);
  fs.mkdirSync(path.join(root, 'leptos', 'build'), { recursive: true });
  fs.writeFileSync(path.join(root, 'leptos', 'index.html'), 'page');
  fs.writeFileSync(path.join(root, 'leptos', 'build', 'strip.cjs'), 'build');
  const inputs = ['leptos/index.html', 'leptos/build'];
  const expected = crypto
    .createHash('sha256')
    .update('leptos/build/strip.cjs\0build\0leptos/index.html\0page\0')
    .digest('hex');
  for (const implementation of [path.posix, path.win32]) {
    expect(fingerprintInputs(root, inputs, 'fixture', implementation)).toEqual({
      sha256: expected,
      files: 2,
    });
  }
});

it('sorts normalized paths, including mixed Windows file and folder inputs', () => {
  expect(normalizedFingerprintPaths(['leptos/index.html', 'leptos\\build\\strip.cjs'])).toEqual([
    'leptos/build/strip.cjs',
    'leptos/index.html',
  ]);
});

it.skipIf(process.platform === 'win32')(
  'rejects linked parents with Windows input separators',
  () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-fingerprint-link-'));
    roots.push(root);
    fs.mkdirSync(path.join(root, 'real'));
    fs.writeFileSync(path.join(root, 'real', 'input.txt'), 'input');
    fs.symlinkSync(path.join(root, 'real'), path.join(root, 'linked'), 'dir');
    expect(() => fingerprintInputs(root, ['linked\\input.txt'], 'fixture', path.win32)).toThrow(
      'must not be symlinks'
    );
  }
);

describe('local Rust build refusal', () => {
  it('refuses only when AAC_NO_LOCAL_RUST_BUILD is exactly 1', () => {
    expect(localRustBuildRefusal({})).toBeNull();
    expect(localRustBuildRefusal({ AAC_NO_LOCAL_RUST_BUILD: '0' })).toBeNull();
    expect(localRustBuildRefusal({ AAC_NO_LOCAL_RUST_BUILD: '1' })).toContain(
      'AAC_NO_LOCAL_RUST_BUILD=1'
    );
  });

  it('stops the build command before any toolchain runs', () => {
    const script = path.join(import.meta.dir, '../../../scripts/build-ui.js');
    const result = spawnSync(process.execPath, [script], {
      env: { ...process.env, AAC_NO_LOCAL_RUST_BUILD: '1' },
      encoding: 'utf8',
    });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain('not compiled here');
  });
});
