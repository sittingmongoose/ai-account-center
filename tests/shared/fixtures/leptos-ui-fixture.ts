/**
 * A dashboard source fixture for tests that build a real dist/ui with scripts/build-ui.js: the Leptos crate (its pkg/
 * already "built" by a fake wasm-pack), its page template, stylesheet and mono font, and a fake toolchain. Synthetic
 * bytes only.
 */
import fs from 'fs';
import path from 'path';

/** One custom wasm section: id 0, a two-byte size, the name and its body. */
function section(name: string, body: Buffer): Buffer {
  const payload = Buffer.concat([Buffer.from([name.length]), Buffer.from(name), body]);
  return Buffer.concat([
    Buffer.from([0, (payload.length & 0x7f) | 0x80, payload.length >> 7]),
    payload,
  ]);
}

/** A valid module with one large, compressible custom section (the build keeps it; only "name" is stripped). */
export const FIXTURE_WASM = Buffer.concat([
  Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]),
  section('fixture', Buffer.from('leptos-serving-fixture-'.repeat(512))),
]);

const LOCK =
  'version = 4\n\n[[package]]\nname = "leptos"\nversion = "0.9.0-rc1"\n[[package]]\nname = "wasm-bindgen"\nversion = "0.2.130"\n[[package]]\nname = "wasm-bindgen-futures"\nversion = "0.4.80"\n[[package]]\nname = "js-sys"\nversion = "0.3.107"\n[[package]]\nname = "web-sys"\nversion = "0.3.107"\n';

/** The frame page template: the build replaces {{PKG}} and {{CSS}}. */
export const FIXTURE_TEMPLATE = `<!doctype html><title>AI Account Center</title><link rel="stylesheet" href="{{CSS}}"><link rel="modulepreload" href="{{PKG}}/aac_web.js">${'<!-- page -->'.repeat(120)}<script type="module">import init from '{{PKG}}/aac_web.js'; init({ module_or_path: '{{PKG}}/aac_web_bg.wasm' });</script>`;

/** Writes web-dashboard/leptos (with a built pkg/) under `root`. */
export function writeLeptosFixture(root: string): void {
  const leptos = path.join(root, 'web-dashboard', 'leptos');
  for (const dir of ['src', 'style', 'build', 'fonts', 'pkg'])
    fs.mkdirSync(path.join(leptos, dir), { recursive: true });
  fs.writeFileSync(path.join(leptos, 'Cargo.toml'), '[package]\nname = "aac-web"\n');
  fs.writeFileSync(path.join(leptos, 'Cargo.lock'), LOCK);
  fs.writeFileSync(path.join(leptos, 'src', 'lib.rs'), '// fixture\n');
  fs.writeFileSync(path.join(leptos, 'style', 'app.css'), 'body{margin:0}\n'.repeat(80));
  fs.writeFileSync(path.join(leptos, 'index.html'), FIXTURE_TEMPLATE);
  fs.copyFileSync(
    path.join(__dirname, '../../../web-dashboard/leptos/build/strip-names.cjs'),
    path.join(leptos, 'build', 'strip-names.cjs')
  );
  fs.writeFileSync(path.join(leptos, 'fonts', 'MartianMono.ttf'), 'font');
  fs.writeFileSync(path.join(leptos, 'fonts', 'OFL-MartianMono.txt'), 'license');
  fs.writeFileSync(path.join(leptos, 'pkg', 'aac_web_bg.wasm'), FIXTURE_WASM);
  fs.writeFileSync(
    path.join(leptos, 'pkg', 'aac_web.js'),
    `export default async function init() {}\n${'// glue\n'.repeat(200)}`
  );
}

/** The toolchain the build asks about, answered as the pinned one; wasm-pack's build does nothing (pkg/ is there). */
export function fakeRun(command: string, args: string[]): string {
  if (command.includes('rustc')) return 'rustc 1.99.1 (fixture)';
  if (command.includes('rustup')) return 'wasm32-unknown-unknown\n';
  if (command.includes('wasm-pack') && args[0] === '--version') return 'wasm-pack 0.15.1\n';
  return '';
}
