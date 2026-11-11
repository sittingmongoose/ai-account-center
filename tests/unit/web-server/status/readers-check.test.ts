import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { NativeStatusHandle } from '../../../../src/status-native/native-loader';
import type { MergedPin } from '../../../../src/status-native/pinned';
import {
  AgentReportsReader,
  type AgentReportsState,
} from '../../../../src/web-server/status/agent-reports-reader';
import { matchPin, normaliseThreadId, ReadersCheckWriter } from '../../../../src/web-server/status/readers-check';
import { StatusDocsReader } from '../../../../src/web-server/status/status-docs';
import { cardDoc, tasksDoc, threadDoc, item } from './status-test-helpers';

const NOW = Date.parse('2026-11-10T09:00:00.000Z');

function writeReport(root: string, writer: string, file: string, body: unknown): void {
  const dir = path.join(root, 'v1', 'writers', writer);
  fs.mkdirSync(path.join(dir, path.dirname(file)), { recursive: true });
  fs.writeFileSync(path.join(dir, file), JSON.stringify(body));
}

/** The fake native handle the docs tests use: parseStatus accepts what TypeScript accepted. */
const fakeHandle: NativeStatusHandle = {
  call: async (method, args) => {
    const parsed = JSON.parse(args) as Record<string, unknown>;
    if (method === 'parseStatus') {
      const doc = JSON.parse(Buffer.from(String(parsed.contentBase64), 'base64').toString('utf8'));
      return JSON.stringify({ ok: true, doc, sha256: `sha:${String(doc.seq)}` });
    }
    throw new Error(`unexpected method ${method}`);
  },
};

const pins: MergedPin[] = [
  {
    machine: 'vm',
    id: 'mcp:launch:320669b2e6cf23a317d2030821cc8a74289073eab144883a2c9b47acdade3c44',
    title: 'Build PMConcept7 and Background Activity',
    pinnedAt: '2026-11-08T10:00:00Z',
    pinOrderKey: 'a',
  },
];

describe('the readers check file', () => {
  it('writes one reader@1 file per writer from a real scan, with the pin match and refusals', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-readers-check-'));
    try {
      // One writer whose reports all read, with the pin spelled as run ids spell it.
      const plain = pins[0].id;
      const encoded = plain.replace(/:/g, '%3A');
      writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { seq: 12 }));
      // The reader's own projection reads the agent block; write it through the real shape.
      fs.writeFileSync(
        path.join(root, 'v1/writers/vm.lead/thread.json'),
        JSON.stringify({
          ...threadDoc('vm.lead', { seq: 12 }),
          agent: { name: 'lead', harness: null, model: 'claude-opus-5-5', effort: 'high', account: null, machine: 'vm', t3Thread: encoded, parent: null },
        })
      );
      writeReport(root, 'vm.lead', 'tasks.json', tasksDoc('vm.lead', [item('F-01', 'done'), item('F-02', 'todo')]));
      writeReport(root, 'vm.lead', 'cards/vm.lead--q1.json', cardDoc('vm.lead', 'q1'));
      writeReport(
        root,
        'vm.lead',
        'status.json',
        {
          schema: 'agent-reports/status@1',
          writer: 'vm.lead',
          seq: 18,
          writtenAt: '2026-11-10T08:00:00Z',
          t3Thread: plain,
          summary: 'Landing F-02.',
        }
      );
      // A writer whose status.json is refused outright: a secret, in the reader's fixed words.
      writeReport(root, 'vm.probe', 'status.json', {
        schema: 'agent-reports/status@1',
        writer: 'vm.probe',
        seq: 3,
        writtenAt: '2026-11-10T08:30:00Z',
        t3Thread: 'mcp:launch:other',
        summary: 'Bearer\uFEFF' + 'x'.repeat(40),
      });

      const reports = new AgentReportsReader({ root: () => root, now: () => NOW });
      await reports.scan();
      const docs = new StatusDocsReader({ root: () => root, now: () => NOW, handle: () => fakeHandle });
      await docs.scan();
      const check = new ReadersCheckWriter({ root: () => root, now: () => NOW });
      check.recordStatusScan(reports.snapshot(), docs.snapshot(), pins);
      check.recordRuns({
        state: 'ok',
        scannedAt: NOW,
        writers: 1,
        runs: [
          {
            file: 'writers/vm.lead/runs/2026-11-10/run-1.json',
            doc: { seq: 7 } as never,
            firstSeenAt: NOW - 120_000,
            sha256: 'ab'.repeat(32),
          },
        ],
        problems: [],
      });
      await check.flush();

      const lead = JSON.parse(
        fs.readFileSync(path.join(root, 'v1/readers/aac/vm.lead.json'), 'utf8')
      );
      expect(lead.schema).toBe('agent-reports/reader@1');
      expect(lead.reader).toBe('aac');
      expect(lead.writer).toBe('vm.lead');
      expect(lead.writtenAt).toBe('2026-11-10T09:00:00.000Z');
      const byFile = new Map<string, Record<string, unknown>>(
        lead.files.map((row: Record<string, unknown>) => [row.file, row])
      );
      expect([...byFile.keys()].sort()).toEqual([
        'cards/vm.lead--q1.json',
        'runs/2026-11-10/run-1.json',
        'status.json',
        'tasks.json',
        'thread.json',
      ]);
      // The thread row carries the pin it is shown under, however the report spelled the id.
      expect(byFile.get('thread.json')).toEqual({
        file: 'thread.json',
        seq: 12,
        sha256: expect.any(String),
        readAt: '2026-11-10T09:00:00.000Z',
        result: 'accepted',
        reason: null,
        pin: { t3Thread: pins[0].id, title: 'Build PMConcept7 and Background Activity' },
      });
      expect(byFile.get('status.json')).toMatchObject({
        seq: 18,
        result: 'accepted',
        reason: null,
        pin: { t3Thread: pins[0].id, title: 'Build PMConcept7 and Background Activity' },
      });
      // Cards, tasks and runs have no pin key at all.
      for (const file of ['tasks.json', 'cards/vm.lead--q1.json', 'runs/2026-11-10/run-1.json']) {
        expect(Object.hasOwn(byFile.get(file) as object, 'file')).toBe(true);
        expect(Object.hasOwn(byFile.get(file) as object, 'pin')).toBe(false);
      }
      expect(byFile.get('runs/2026-11-10/run-1.json')).toMatchObject({
        seq: 7,
        sha256: 'ab'.repeat(32),
        result: 'accepted',
      });
      // No temporary files are left behind: the write is atomic.
      expect(fs.readdirSync(path.join(root, 'v1/readers/aac')).sort()).toEqual([
        'vm.lead.json',
        'vm.probe.json',
      ]);

      // A refusal with no good copy: the row is the fixed reason, never the file's text.
      const probe = JSON.parse(
        fs.readFileSync(path.join(root, 'v1/readers/aac/vm.probe.json'), 'utf8')
      );
      expect(probe.files).toHaveLength(1);
      expect(probe.files[0]).toMatchObject({
        file: 'status.json',
        seq: null,
        sha256: null,
        result: 'refused',
        pin: null,
      });
      // The reason is the reader's own fixed sentence, word for word.
      expect(probe.files[0].reason).toBe('status.json refused: it contains what looks like a bearer token');
      expect(JSON.stringify(probe)).not.toContain('Bearer');
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('normalises the id forms the composer tolerates, and matches no pin by title', () => {
    const hex = '320669b2e6cf23a317d2030821cc8a74289073eab144883a2c9b47acdade3c44';
    const plain = `mcp:launch:${hex}`;
    expect(normaliseThreadId(plain)).toBe(hex);
    expect(normaliseThreadId(plain.replace(/:/g, '%3A'))).toBe(hex);
    expect(normaliseThreadId(`t3-thread://v2/ef60010f-0371-4770-ad52-d8378888411b/${plain}`)).toBe(hex);
    expect(normaliseThreadId(` ${hex.toUpperCase()} `)).toBe(hex);
    expect(matchPin(plain.replace(/:/g, '%3A'), pins)?.title).toBe(pins[0].title);
    // Another id, and a title that happens to match the pin's title: no match either way.
    expect(matchPin(`mcp:launch:${hex}ff`, pins)).toBeNull();
    expect(matchPin(null, pins)).toBeNull();
    expect(matchPin('Build PMConcept7 and Background Activity', pins)).toBeNull();
  });

  it('feeds one file per writer from both scans wherever they share a root', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-readers-merge-'));
    try {
      const state: AgentReportsState = {
        root,
        state: 'ok',
        scannedAt: NOW,
        complete: true,
        threads: [],
        tasks: [],
        cards: [],
        problems: [],
        boards: new Map(),
      };
      // Two instances over one root (the Status scan and the Scoreboard's runs scan).
      const statusSide = new ReadersCheckWriter({ root: () => root, now: () => NOW });
      const runsSide = new ReadersCheckWriter({ root: () => root, now: () => NOW });
      statusSide.recordStatusScan(state, { state: 'ok', scannedAt: NOW, docs: [], problems: [] }, []);
      runsSide.recordRuns({
        state: 'ok',
        scannedAt: NOW,
        writers: 1,
        runs: [
          {
            file: 'writers/vm.lead/runs/2026-11-10/run-2.json',
            doc: { seq: 9 } as never,
            firstSeenAt: NOW,
            sha256: 'cd'.repeat(32),
          },
        ],
        problems: [
          { file: 'writers/vm.probe/runs/2026-11-10/run-9.json', reason: 'not valid JSON', since: NOW },
        ],
      });
      await statusSide.flush();
      const lead = JSON.parse(
        fs.readFileSync(path.join(root, 'v1/readers/aac/vm.lead.json'), 'utf8')
      );
      expect(lead.files.map((row: { file: string }) => row.file)).toEqual([
        'runs/2026-11-10/run-2.json',
      ]);
      await runsSide.flush();
      const probe = JSON.parse(
        fs.readFileSync(path.join(root, 'v1/readers/aac/vm.probe.json'), 'utf8')
      );
      expect(probe.files[0]).toMatchObject({
        file: 'runs/2026-11-10/run-9.json',
        result: 'refused',
        reason: 'not valid JSON',
        seq: null,
      });
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});
