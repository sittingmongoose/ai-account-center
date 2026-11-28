/**
 * Static Docker hygiene: the healthcheck probes the public /api/health
 * endpoint with a native HTTP client, the build context excludes private and
 * rebuilt trees, and the runtime ships no Node or Python interpreter.
 */
import { readFileSync } from 'fs';
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
    expect(block![0]).toContain(HEALTH_URL);
    expect(block![0]).toContain('curl');
    expect(block![0]).not.toMatch(/\bnode\b/);
    expect(block![0]).not.toMatch(ROOT_PROBE);
  });

  it('which stays a public, unauthenticated route in the native route table', () => {
    const routes = JSON.parse(readFileSync('config/rust-service-routes.json', 'utf8')) as Array<{
      method: string;
      path: string;
      owner: string;
      roles: string[];
    }>;
    const health = routes.filter((route) => route.path === '/api/health');
    expect(health).toHaveLength(1);
    expect(health[0].method).toBe('GET');
    expect(health[0].owner).toBe('rust');
    expect(health[0].roles).toContain('public');
  });

  it('in the integrated image by inheritance, never a retired override', () => {
    expect(integrated).not.toMatch(/^HEALTHCHECK/m);
    expect(integrated).toContain('FROM ${AI_ACCOUNT_CENTER_IMAGE}');
  });

  it('in every compose file that defines one', () => {
    const healthcheck = compose.match(/^\s+healthcheck:[\s\S]*?(?=\n\nvolumes:)/m)?.[0] ?? '';
    expect(compose).toContain('healthcheck:');
    expect(compose).toContain(HEALTH_URL);
    expect(compose).toContain('curl');
    expect(healthcheck).not.toMatch(/\bnode\b/);
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

describe('the image packages the gated release tarball and keeps interpreters out of the runtime', () => {
  const unpack = dockerfile.match(/^FROM .* AS unpack$[\s\S]*?(?=^FROM .* AS runtime$)/m)?.[0] ?? '';
  const runtime = dockerfile.match(/^FROM .* AS runtime$[\s\S]*$/m)?.[0] ?? '';

  it('unpacks and verifies a gated Linux release tarball instead of compiling from source', () => {
    expect(dockerfile).toMatch(/^FROM scratch AS release$/m);
    expect(unpack).toMatch(/^FROM debian:trixie-slim@sha256:/m);
    expect(unpack).toContain('ARG AAC_RELEASE_TGZ=');
    expect(unpack).toContain('ARG AAC_RELEASE_SHA256=');
    expect(unpack).toContain('--mount=type=bind,from=release,target=/mnt/release');
    expect(unpack).toContain('test -x /stage/dist/native/aac-http');
    expect(unpack).toContain('test -s /stage/dist/ui/ui-build-manifest.json');
    expect(unpack).toContain('test -s /stage/dist/build-info.json');
    expect(unpack).toContain('test -s /stage/config/agent-artifacts.json');
    expect(unpack).toContain('/stage/dist/native/aac-http --build-commit');
    expect(unpack).toContain('x86_64-unknown-linux-gnu');
    expect(unpack).toContain('aarch64-apple-darwin');
    expect(unpack).toContain('x86_64-pc-windows-msvc');
    expect(unpack).toContain('dist/native/aac-cursor-account');
    expect(unpack).toContain('dist/native/aac-usage');
    expect(dockerfile).not.toContain('bun run build:all');
    expect(dockerfile).not.toContain('bun run store:build');
    expect(dockerfile).not.toContain('bun run status:build');
    expect(dockerfile).not.toMatch(/^FROM node:/m);
  });

  it('requires AAC_BUILD_COMMIT and verifies the tarball matches it', () => {
    expect(unpack).toContain('ARG AAC_BUILD_COMMIT');
    expect(unpack).toContain('ENV AAC_BUILD_COMMIT=${AAC_BUILD_COMMIT}');
    expect(unpack).toContain('^[0-9a-f]{40}$');
    expect(unpack).toContain('/stage/dist/build-info.json');
    expect(runtime).toContain('ARG AAC_BUILD_COMMIT');
    expect(runtime).toContain('org.opencontainers.image.revision=');
  });

  it('wires the release context and commit arg in Compose and smoke-test.sh', () => {
    const smoke = readFileSync('docker/smoke-test.sh', 'utf8');
    expect(smoke).toContain('--build-context "release=$RELEASE_DIR"');
    expect(smoke).toContain('--build-arg "AAC_BUILD_COMMIT=$BUILD_COMMIT"');
    expect(smoke).toContain('dist/build-info.json');
    expect(smoke).toContain('deployed-build.json');
    expect(smoke).toContain('--verify-release-artifacts');
    expect(smoke).toContain('x86_64-unknown-linux-gnu');
    expect(smoke).toContain('aarch64-apple-darwin');
    expect(smoke).toContain('x86_64-pc-windows-msvc');
    expect(compose).toContain('additional_contexts:');
    expect(compose).toContain('AAC_RELEASE_DIR');
    expect(compose).toContain('AAC_BUILD_COMMIT');
    expect(compose).toContain('AAC_RELEASE_TGZ');
  });

  it('uses a non-Node runtime with SSH, tzdata, and no Python, Bun, or jq interpreter', () => {
    expect(runtime).toMatch(/^FROM debian:trixie-slim@sha256:/m);
    expect(runtime).toContain('bash ca-certificates curl openssh-client tzdata');
    expect(runtime).toContain('COPY --from=unpack /stage/dist ./dist');
    expect(runtime).toContain('COPY --from=unpack /stage/config/agent-artifacts.json ./config/agent-artifacts.json');
    expect(runtime).not.toMatch(/^FROM node:/m);
    expect(runtime).not.toMatch(/\bpython3\b/);
    expect(runtime).not.toMatch(/\bbun\b/);
    expect(runtime).not.toMatch(/\bjq\b/);
    expect(runtime).not.toMatch(/\bNODE_ENV\b/);
    expect(dockerfile).toMatch(/^USER node$/m);
  });

  it('copies from the unpacked release only paths the package ships', () => {
    const shipped = (JSON.parse(readFileSync('package.json', 'utf8')) as { files: string[] }).files.map(
      (entry) => entry.replace(/\/+$/, '')
    );
    const always = ['package.json', 'README.md', 'LICENSE'];
    const copied = [...runtime.matchAll(/^COPY --from=unpack \/stage\/(\S+) /gm)].map((m) => m[1]);
    expect(copied.length).toBeGreaterThan(0);
    for (const source of copied) {
      const inPackage =
        always.includes(source) ||
        shipped.some((entry) => source === entry || source.startsWith(`${entry}/`));
      expect({ source, inPackage }).toEqual({ source, inPackage: true });
    }
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

describe('the build context carries every file the native build reads', () => {
  it('reads .dockerignore as Docker does', () => {
    expect(ignoredByContext('tests/unit/docker/docker-hygiene.test.ts')).toBe(true);
    expect(ignoredByContext('docs/README.md')).toBe(true);
    expect(ignoredByContext('crates/aac-status/target/release/x')).toBe(true);
    expect(ignoredByContext('crates/aac-http/src/__tests__/x.rs')).toBe(true);
    expect(ignoredByContext('crates/aac-http/src/lib.rs')).toBe(false);
    expect(ignoredByContext('README.md')).toBe(false);
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
