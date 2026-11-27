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

describe('the image builds Rust and keeps interpreters out of the runtime', () => {
  const runtime = dockerfile.match(/^FROM .* AS runtime$[\s\S]*$/m)?.[0] ?? '';

  it('builds the native dashboard and browser bundle in the build stage', () => {
    expect(dockerfile).toContain('bun run build:all');
    expect(dockerfile).toContain('test -x dist/native/aac-http');
    expect(dockerfile).toContain('test -s dist/ui/ui-build-manifest.json');
    expect(dockerfile).toContain('node scripts/verify-bundle.js');
    expect(dockerfile).not.toContain('bun run store:build');
    expect(dockerfile).not.toContain('bun run status:build');
  });

  it('uses a non-Node runtime with SSH and no Python interpreter', () => {
    expect(runtime).toMatch(/^FROM debian:bookworm-slim@sha256:/m);
    expect(runtime).toContain('ca-certificates curl openssh-client');
    expect(runtime).not.toMatch(/^FROM node:/m);
    expect(runtime).not.toMatch(/\bpython3\b/);
    expect(runtime).not.toMatch(/\bNODE_ENV\b/);
    expect(dockerfile).toMatch(/^USER node$/m);
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
