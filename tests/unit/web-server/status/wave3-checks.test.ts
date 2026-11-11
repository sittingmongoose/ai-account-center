import { expect, test } from 'bun:test';
import { PinnedThreadsCollector } from '../../../../src/web-server/status/pinned-threads';
import { ReadersCheckWriter } from '../../../../src/web-server/status/readers-check';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
test('checks record scans before pins have been collected', async () => {
  let recorded = 0;
  let flushed = 0;
  const checks = new ReadersCheckWriter({ root: () => '/unused-test-root' });
  checks.recordStatusScan = () => { recorded++; };
  checks.flushSoon = () => { flushed++; };
  const collector = new PinnedThreadsCollector({ handle: { call: async () => '{}' }, readersCheck: checks });
  await collector.compose(new AgentReportsReader({ root: () => '/unused-test-root' }).snapshot(), []);
  expect(recorded).toBe(1);
  expect(flushed).toBe(1);
});
