import { describe, expect, it } from 'bun:test';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(import.meta.dir, '../../..');

const CHILD = `
import { getGlobalDispatcher } from 'undici/index.js';
// Dynamic import: the bundle path is a runtime mkdtemp argument, unknown at author time.
const { applyGlobalFetchProxy } = await import(process.argv[2]);
process.env.HTTPS_PROXY = 'http://proxy.example:8443';
const result = applyGlobalFetchProxy();
if (!result.enabled) {
  console.error('proxy not enabled: ' + JSON.stringify(result));
  process.exit(2);
}
const routing = getGlobalDispatcher();
// The first native fetch-class use installs Node >= 26's native undici, which
// redefines the legacy global dispatcher symbol npm-undici reads.
new Headers();
new Request('https://example.com/');
const sym = Symbol.for('undici.globalDispatcher.1');
const after = Object.getOwnPropertyDescriptor(globalThis, sym)?.value;
const ok = after === routing && getGlobalDispatcher() === routing;
console.log(JSON.stringify({ node: process.version, routingSurvives: ok }));
process.exit(ok ? 0 : 1);
`;

// SPEC row 37's real-runtime scenario against the PATH node (the CI matrix's Node):
// after applyGlobalFetchProxy, a first native Headers/Request use must not replace the
// routing dispatcher. Bundled because Node cannot load the source's extensionless TS
// imports; the bundle lives inside the checkout so bare undici resolves.
describe('global fetch proxy under the PATH node', () => {
  it('keeps the routing dispatcher after native Headers/Request init', () => {
    const tmp = fs.mkdtempSync(path.join(root, '.tmp-fetch-proxy-'));
    try {
      const built = spawnSync(
        'bun',
        [
          'build',
          path.join(root, 'src/utils/fetch-proxy-setup.ts'),
          '--target=node',
          '--format=esm',
          '--packages=external',
          `--outfile=${path.join(tmp, 'fetch-proxy.mjs')}`,
        ],
        { cwd: root, encoding: 'utf8', timeout: 60_000 }
      );
      expect(built.status, `bun build failed: ${built.stderr}`).toBe(0);
      fs.writeFileSync(path.join(tmp, 'child.mjs'), CHILD);
      const ran = spawnSync('node', ['./child.mjs', './fetch-proxy.mjs'], {
        cwd: tmp,
        encoding: 'utf8',
        timeout: 60_000,
      });
      expect(ran.status, `node child failed: ${ran.stdout} ${ran.stderr}`).toBe(0);
      expect(JSON.parse(ran.stdout.trim()).routingSurvives).toBe(true);
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });
});
