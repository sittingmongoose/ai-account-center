/** Exact discovery/agent wire parity. No fields, ordering or floats are normalized. */
import { afterAll, expect } from 'bun:test';
import { execFileSync } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';

let compared = 0;
afterAll(() => {
  if (process.env.AAC_AGENT_SCAN_BIN) {
    expect(compared).toBeGreaterThan(0);
    console.log(`scan-usage: ${compared} exact byte comparisons`);
  }
});

export function scanUsageParity(
  request: Record<string, unknown>,
  scannerBytes: string,
  env: Record<string, string | undefined>
): void {
  const binary = process.env.AAC_AGENT_SCAN_BIN;
  if (!binary) return;
  const home = env.HOME ?? process.env.HOME;
  if (!home || !fs.realpathSync(home).startsWith(fs.realpathSync(os.tmpdir()) + path.sep))
    throw new Error('Scan parity requires an isolated fixture home');
  const vars = Object.fromEntries(
    Object.entries({ ...process.env, ...env }).filter(
      (entry): entry is [string, string] => typeof entry[1] === 'string'
    )
  );
  // Fixture-only context: production refuses this field. Never change the agent process HOME.
  const input =
    JSON.stringify({
      protocol: 1,
      verb: 'scan-usage',
      id: 'scan-parity',
      deadlineMs: 29000,
      args: { ...request, _scanFixture: { home, env: vars } },
    }) + '\n';
  const stdout = execFileSync(binary, [], {
    input,
    encoding: 'utf8',
    timeout: 30000,
    maxBuffer: 8 * 1024 * 1024,
    env: { ...process.env, TOKIO_WORKER_THREADS: '1' },
  });
  const event = JSON.parse(stdout);
  expect(event.type).toBe('result');
  expect(event.protocol).toBe(1);
  expect(event.id).toBe('scan-parity');
  const prefix = '{"type":"result","protocol":1,"id":"scan-parity","value":';
  expect(stdout.startsWith(prefix)).toBe(true);
  expect(stdout.endsWith('}\n')).toBe(true);
  expect(stdout.slice(prefix.length, -2)).toBe(scannerBytes);
  compared++;
}
