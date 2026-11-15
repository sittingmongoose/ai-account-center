/**
 * Static Docker hygiene: the healthcheck probes the public /api/health
 * endpoint everywhere, the build context excludes private and rebuilt
 * trees, and the image build fails loudly when a native module is missing.
 * No image is built here; these are the file-level contracts.
 */
import { readdirSync, readFileSync } from 'fs';
import path from 'path';
import { describe, expect, it } from 'bun:test';

const dockerfile = readFileSync('docker/Dockerfile', 'utf8');
const integrated = readFileSync('docker/Dockerfile.integrated', 'utf8');
const compose = readFileSync('docker/compose.yaml', 'utf8');
const legacyCompose = readFileSync('docker/docker-compose.yml', 'utf8');
const integratedCompose = readFileSync('docker/docker-compose.integrated.yml', 'utf8');
const dockerignore = readFileSync('.dockerignore', 'utf8');

const HEALTH_URL = 'http://127.0.0.1:3000/api/health';
const ROOT_PROBE = /http\.get\('http:\/\/127\.0\.0\.1:3000\/'/;

describe('the healthcheck targets the public /api/health endpoint', () => {
  it('in the canonical Dockerfile', () => {
    const block = dockerfile.match(/^HEALTHCHECK[\s\S]*?(?=\n\n|\nENTRYPOINT)/m);
    expect(block).not.toBeNull();
    expect(block![0]).toContain(HEALTH_URL);
    expect(block![0]).not.toMatch(ROOT_PROBE);
  });

  it('which stays a public, unauthenticated route', () => {
    const guard = readFileSync('src/web-server/middleware/api-request-guard.ts', 'utf8');
    const routes = readFileSync('src/web-server/routes/index.ts', 'utf8');
    expect(guard).toContain("['GET', '/health']");
    expect(routes).toContain("apiRoutes.get('/health'");
  });

  it('in the integrated image by inheritance, never a retired override', () => {
    expect(integrated).not.toMatch(/^HEALTHCHECK/m);
    expect(integrated).toContain('FROM ${AI_ACCOUNT_CENTER_IMAGE}');
  });

  it('in every compose file that defines one', () => {
    expect(compose).toContain('healthcheck:');
    expect(compose).toContain(HEALTH_URL);
    expect(compose).not.toMatch(ROOT_PROBE);
    // The compatibility files inherit the canonical image healthcheck.
    for (const file of [legacyCompose, integratedCompose]) {
      expect(file).not.toMatch(/^\s*healthcheck:/m);
      expect(file).not.toMatch(ROOT_PROBE);
      expect(file).not.toContain('8317');
    }
  });
});

describe('the build context excludes private and rebuilt trees', () => {
  const entries = dockerignore.split('\n').map((line) => line.trim());

  it('keeps dependencies, git metadata, build output, test homes and bundles out', () => {
    for (const required of ['node_modules', '.git', 'dist', 'tests', '*.bundle']) {
      expect(entries).toContain(required);
    }
    expect(entries).toContain('**/__tests__');
  });

  it('keeps machine-specific native artifacts out of the context', () => {
    expect(entries).toContain('native/*/target');
    expect(entries).toContain('native/*/prebuilt');
    expect(entries).toContain('target');
    expect(entries).toContain('crates/*/target');
    expect(entries).toContain('crates/*/prebuilt');
  });
});

describe('the image build fails loudly when a native module is missing', () => {
  it('builds both native modules for the image platform', () => {
    expect(dockerfile).toContain('bun run store:build');
    expect(dockerfile).toContain('bun run status:build');
  });

  it('gates packaging on a shell test -f for each prebuilt module, not a listing', () => {
    expect(dockerfile).toMatch(/test -f crates\/aac-store\/prebuilt\/\*\.node/);
    expect(dockerfile).toMatch(/test -f crates\/aac-status\/prebuilt\/\*\.node/);
    expect(dockerfile).not.toMatch(/^\s*(RUN\s+)?ls\s+native\//m);
  });
});

/** Docker's .dockerignore rule: a pattern matches a path or one of its folders; the last match wins. */
const ignoreRules = dockerignore
  .split('\n')
  .map((line) => line.trim())
  .filter((line) => line && !line.startsWith('#'))
  .map((line) => {
    const negated = line.startsWith('!');
    const glob = (negated ? line.slice(1) : line).replace(/^\/+/, '').replace(/\/+$/, '');
    let source = '';
    for (let i = 0; i < glob.length; i++) {
      if (glob.startsWith('**/', i)) {
        source += '(?:.*/)?';
        i += 2;
      } else if (glob.startsWith('**', i)) {
        source += '.*';
        i += 1;
      } else if (glob[i] === '*') source += '[^/]*';
      else if (glob[i] === '?') source += '[^/]';
      else source += glob[i].replace(/[.+^${}()|[\]\\]/g, '\\$&');
    }
    return { negated, pattern: new RegExp(`^${source}$`) };
  });

function ignoredByContext(file: string): boolean {
  const parts = file.split('/');
  const paths = parts.map((_, i) => parts.slice(0, i + 1).join('/'));
  let ignored = false;
  for (const rule of ignoreRules)
    if (paths.some((candidate) => rule.pattern.test(candidate))) ignored = !rule.negated;
  return ignored;
}

/** Package paths the compiled server resolves outside dist/ (`path.resolve(__dirname, '...')`, `require('../x.json')`). */
function runtimeReads(): string[] {
  const reads = new Set<string>();
  const call =
    /path\.(?:resolve|join)\(\s*__dirname\s*((?:,\s*(?:'[^'\n]*'|"[^"\n]*"|`[^`$\n]*`)\s*)*)/g;
  const literal = /'([^'\n]*)'|"([^"\n]*)"|`([^`$\n]*)`/g;
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const file = path.posix.join(dir, entry.name);
      if (entry.isDirectory()) walk(file);
      else if (/\.ts$/.test(file) && !/\.(test|d)\.ts$/.test(file)) {
        const source = readFileSync(file, 'utf8');
        const out = path.posix.join('dist', path.posix.dirname(file.slice('src/'.length)));
        const targets = [...source.matchAll(call)].map((m) =>
          path.posix.join(out, ...[...m[1].matchAll(literal)].map((l) => l[1] ?? l[2] ?? l[3]))
        );
        for (const m of source.matchAll(/require\(\s*'(\.\.?\/[^']+\.json)'\s*\)/g))
          targets.push(path.posix.join(out, m[1]));
        for (const target of targets)
          if (target !== '.' && target !== 'dist' && !target.startsWith('dist/')) reads.add(target);
      }
    }
  };
  walk('src');
  return [...reads].sort();
}

/** Where each runtime-stage `COPY --from=build /app/<from> <to>` lands, relative to /app. */
function runtimeCopies(): string[] {
  const stage = dockerfile.slice(dockerfile.indexOf(' AS runtime'));
  return [...stage.matchAll(/^COPY --from=build \/app\/(\S+) (\S+)$/gm)].map(([, from, to]) => {
    const dest = to.replace(/^\.\/?/, '');
    return dest === '' || to.endsWith('/')
      ? path.posix.join(dest, path.posix.basename(from))
      : dest;
  });
}

describe('the image carries every file the server and its native build read', () => {
  it('reads .dockerignore as Docker does', () => {
    expect(ignoredByContext('tests/unit/docker/docker-hygiene.test.ts')).toBe(true);
    expect(ignoredByContext('docs/README.md')).toBe(true);
    expect(ignoredByContext('crates/aac-status/target/release/x')).toBe(true);
    expect(ignoredByContext('src/web-server/__tests__/x.ts')).toBe(true);
    expect(ignoredByContext('src/web-server/index.ts')).toBe(false);
    expect(ignoredByContext('README.md')).toBe(false);
  });

  it('copies every package file the compiled server resolves outside dist/ into the runtime stage', () => {
    const reads = runtimeReads();
    // The scan sees the forms the server uses, including a call split over lines.
    for (const known of ['web-dashboard/public/computers.json', 'config/tables', 'package.json'])
      expect(reads).toContain(known);
    const copies = runtimeCopies();
    const missing = reads.filter(
      (file) => !copies.some((copy) => file === copy || file.startsWith(`${copy}/`))
    );
    expect(missing).toEqual([]);
    expect(reads.filter(ignoredByContext)).toEqual([]);
  });

  it('keeps every status-module build input in the build context', () => {
    const { sourceFiles } = require('../../../scripts/build-status-native.js');
    const inputs = (sourceFiles() as string[]).map((rel) =>
      path.posix.normalize(path.posix.join('crates/aac-status', rel))
    );
    expect(inputs).toContain('tests/spec/wire-words.json');
    expect(inputs.filter(ignoredByContext)).toEqual([]);
  });
});
