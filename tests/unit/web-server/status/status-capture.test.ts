import { afterEach, expect, it } from 'bun:test';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { StatusService } from '../../../../src/web-server/status/status-service';
import type { PinnedThreadsCollector } from '../../../../src/web-server/status/pinned-threads';
import type { StoreReportCapture } from '../../../../src/store/store-service';
import { tempDirs, threadDoc, writeReport } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

it('captures accepted revisions, retains thread identity, retries failures and skips unchanged docs', async () => {
  const root = dirs.make();
  const reports = path.join(root, 'reports');
  const writer = 'vm.capture';
  let now = Date.parse('2026-11-13T10:00:00Z');
  const seen: StoreReportCapture[] = [];
  let fails = true;
  const store = {
    captureReports: async (records: StoreReportCapture[]) => {
      if (fails) throw new Error('disk unavailable');
      seen.push(...records);
      return { stored: records.length, duplicates: 0 };
    },
  };
  const doc = (seq: number, state = 'working') =>
    threadDoc(writer, {
      seq,
      writtenAt: new Date(now).toISOString(),
      state,
      agent: { t3Thread: 'thread:example', model: 'openai/example', effort: 'high' },
    });
  writeReport(reports, writer, 'thread.json', doc(1));
  const pinned = {
    docs: { snapshot: () => ({ docs: [] }) },
    scanStatusDocs: async () => {},
    compose: async () => {},
  } as unknown as PinnedThreadsCollector;
  const service = new StatusService({
    reader: new AgentReportsReader({ root: () => reports, now: () => now }),
    historyStore: () => store,
    pinned,
    spendNative: null,
    now: () => now,
  });
  await service.scanReports();
  expect(seen).toHaveLength(0);
  fails = false;
  await service.scanReports();
  expect(seen).toHaveLength(1);
  expect(seen[0].ids).toMatchObject({
    computer: 'ubuntu',
    t3ThreadId: 'thread:example',
    effort: 'high',
  });
  expect(seen[0].doc.schema).toBe('agent-reports/thread@1');
  await service.scanReports();
  expect(seen).toHaveLength(1);
  now += 1000;
  writeReport(reports, writer, 'thread.json', doc(2, 'done'));
  await service.scanReports();
  expect(seen.map((row) => row.seq)).toEqual([1, 2]);
  now += 1000;
  writeReport(reports, writer, 'thread.json', doc(3, 'working'));
  await service.scanReports();
  expect(seen.map((row) => row.doc.state)).toEqual(['working', 'done', 'working']);
  // Rejected source regressions must not become authoritative history revisions.
  writeReport(reports, writer, 'thread.json', doc(1, 'done'));
  await service.scanReports();
  expect(seen).toHaveLength(3);
});

it('isolates native refusals, captures later batches and retries only unacknowledged revisions', async () => {
  const root = dirs.make();
  const reports = path.join(root, 'reports');
  const now = Date.parse('2026-11-13T10:00:00Z');
  const seen: StoreReportCapture[] = [];
  const attempted: string[][] = [];
  let refusedWriter: string | undefined;
  let refuses = true;
  const store = {
    captureReports: async (records: StoreReportCapture[]) => {
      attempted.push(records.map((row) => row.writer));
      refusedWriter ??= records[0].writer;
      if (refuses && records.some((row) => row.writer === refusedWriter)) {
        throw new Error('invalid: capture report failed validation');
      }
      seen.push(...records);
      return { stored: records.length, duplicates: 0 };
    },
  };
  for (let i = 0; i < 12; i++) {
    const writer = `vm.batch${i}`;
    writeReport(
      reports,
      writer,
      'thread.json',
      threadDoc(writer, { writtenAt: new Date(now).toISOString() })
    );
  }
  const pinned = {
    docs: { snapshot: () => ({ docs: [] }) },
    scanStatusDocs: async () => {},
    compose: async () => {},
  } as unknown as PinnedThreadsCollector;
  const service = new StatusService({
    reader: new AgentReportsReader({ root: () => reports, now: () => now }),
    historyStore: () => store,
    pinned,
    spendNative: null,
    now: () => now,
  });
  await service.scanReports();
  expect(attempted[0]).toHaveLength(4);
  expect(seen).toHaveLength(11);
  expect(new Set(seen.map((row) => row.writer)).size).toBe(11);
  expect(seen.some((row) => row.writer === refusedWriter)).toBe(false);
  expect(attempted.slice(-2).map((batch) => batch.length)).toEqual([4, 4]);
  attempted.length = 0;
  await service.scanReports();
  expect(attempted).toEqual([[refusedWriter!]]);
  expect(seen).toHaveLength(11);
  refuses = false;
  await service.scanReports();
  expect(seen).toHaveLength(12);
  expect(new Set(seen.map((row) => row.writer)).size).toBe(12);
  attempted.length = 0;
  await service.scanReports();
  expect(attempted).toEqual([]);
});
