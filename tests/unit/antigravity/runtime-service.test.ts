/**
 * The runtime registry: a scope without saved profiles is asked again after
 * the retry window, so a first profile added from the terminal appears
 * without a restart, and its monitor starts when it is the server's scope.
 */
import { expect, it } from 'bun:test';
import type { AntigravityRuntime } from '../../../src/antigravity/runtime-composition';

it('retries an empty scope after the window and starts a late runtime for the started scope', async () => {
  const service = (await import(
    '../../../src/antigravity/runtime-service.ts?empty-scope-retry'
  )) as typeof import('../../../src/antigravity/runtime-service');
  const started: string[] = [];
  const stopped: string[] = [];
  const runtimeFor = (scope: string) =>
    ({
      start: () => started.push(scope),
      stop: () => stopped.push(scope),
    }) as unknown as AntigravityRuntime;
  const calls: string[] = [];
  const withProfiles = new Set<string>();
  service.configureAntigravityRuntimeFactory((scope) => {
    calls.push(scope);
    return withProfiles.has(scope) ? runtimeFor(scope) : null;
  });
  const server = '/fixture/server-scope';
  const other = '/fixture/other-scope';
  const t0 = Date.now();
  expect(service.startAntigravityRuntime(server)).toBeNull();
  expect(calls).toEqual([server]);
  expect(service.getAntigravityRuntime(server, t0 + 1_000)).toBeNull();
  expect(calls).toEqual([server]);
  withProfiles.add(server);
  withProfiles.add(other);
  const late = service.getAntigravityRuntime(server, t0 + service.ANTIGRAVITY_RUNTIME_RETRY_MS + 1);
  expect(late).not.toBeNull();
  expect(started).toEqual([server]);
  expect(service.getAntigravityRuntime(server, t0 + 60_000)).toBe(late);
  expect(calls).toEqual([server, server]);
  expect(service.getAntigravityRuntime(other)).not.toBeNull();
  expect(started).toEqual([server]);
  service.stopAntigravityRuntime();
  expect(stopped).toEqual([server]);
});
