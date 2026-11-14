/**
 * Static Docker hygiene: the healthcheck probes the public /api/health
 * endpoint everywhere, the build context excludes private and rebuilt
 * trees, and the image build fails loudly when a native module is missing.
 * No image is built here; these are the file-level contracts.
 */
import { readFileSync } from 'fs';
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
  });
});

describe('the image build fails loudly when a native module is missing', () => {
  it('builds both native modules for the image platform', () => {
    expect(dockerfile).toContain('bun run store:build');
    expect(dockerfile).toContain('bun run status:build');
  });

  it('gates packaging on a shell test -f for each prebuilt module, not a listing', () => {
    expect(dockerfile).toMatch(/test -f native\/aac-store\/prebuilt\/\*\.node/);
    expect(dockerfile).toMatch(/test -f native\/aac-status\/prebuilt\/\*\.node/);
    expect(dockerfile).not.toMatch(/^\s*(RUN\s+)?ls\s+native\//m);
  });
});
