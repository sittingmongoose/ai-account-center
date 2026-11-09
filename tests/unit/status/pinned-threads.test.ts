import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { NativeStatusHandle } from '../../../src/status-native/native-loader';
import type { PinnedThread } from '../../../src/status-native/pinned';
import {
  PinnedThreadsCollector,
  zoneOffsetMinutes,
  type PinsMachine,
} from '../../../src/web-server/status/pinned-threads';
import type { AgentReportsState } from '../../../src/web-server/status/agent-reports-reader';
import type { CommandRunner } from '../../../src/web-server/status/status-collectors';

/**
 * A fake native handle: the same method split as the module, with merge and compose echoing
 * what they were given, so the collector's own rules (the one-scan grace, the per-machine
 * problems) are what gets tested. The module's rules are its own test suite.
 */
function fakeHandle(
  options: {
    vm?: () => { pinned: PinnedThread[] } | { unavailable: string };
    remoteShape?: (machine: PinsMachine) => boolean;
  } = {}
) {
  const composeInputs: unknown[] = [];
  const mergedLists: Array<Array<{ machine: string; pinned: PinnedThread[] }>> = [];
  const handle: NativeStatusHandle = {
    call: async (method, args) => {
      const parsed = JSON.parse(args) as Record<string, unknown>;
      if (method === 'pins') return JSON.stringify(options.vm?.() ?? { pinned: [] });
      if (method === 'parseRemotePins') {
        const machine = String(parsed.machine) as PinsMachine;
        return JSON.stringify(
          options.remoteShape?.(machine) ? { ok: false, reason: 'shape' } : { ok: true, pinned: [] }
        );
      }
      if (method === 'mergePins') {
        const lists = (parsed.lists ?? []) as Array<{ machine: string; pinned: PinnedThread[] }>;
        mergedLists.push(lists);
        const pinned = lists.flatMap((list) =>
          list.pinned.map((row) => ({ ...row, machine: list.machine }))
        );
        return JSON.stringify({ pinned });
      }
      if (method === 'compose') {
        composeInputs.push(parsed);
        const problems = (parsed.machineProblems ?? []) as Array<{ machine: string }>;
        return JSON.stringify({
          asOfMs: parsed.nowMs,
          head: 'fake head',
          counts: { pinned: (parsed.pins as unknown[]).length, looping: 0, waitingOnYou: 0 },
          machineProblems: problems.map((problem) => ({
            machine: problem.machine,
            label: problem.machine,
            text: `unavailable on ${problem.machine}`,
          })),
          updates: [],
        });
      }
      throw new Error(`unexpected method ${method}`);
    },
  };
  return { handle, composeInputs, mergedLists };
}

function pin(id: string): PinnedThread {
  return { id, title: `Thread ${id}`, pinnedAt: '2026-11-08T10:00:00Z', pinOrderKey: id };
}

const noReports: AgentReportsState = {
  root: '/fixture',
  state: 'ok',
  scannedAt: 1,
  complete: true,
  threads: [],
  tasks: [],
  cards: [],
  problems: [],
  boards: new Map(),
};

const neverRun: CommandRunner = async () => {
  throw new Error('the runner must not be reached; readRemote is faked');
};

describe('PinnedThreadsCollector', () => {
  it('collects every machine, merges, and composes from the validated reports', async () => {
    const { handle, composeInputs, mergedLists } = fakeHandle();
    let vmPins: PinnedThread[] = [pin('a')];
    const macPins: PinnedThread[] = [pin('b')];
    const collector = new PinnedThreadsCollector({
      handle: {
        call: async (method, args) => {
          const parsed = JSON.parse(args) as Record<string, unknown>;
          if (method === 'pins') return JSON.stringify({ pinned: vmPins });
          if (method === 'parseRemotePins') {
            const machine = String(parsed.machine);
            return JSON.stringify({
              ok: true,
              pinned: machine === 'mac' ? macPins : [],
            });
          }
          return handle.call(method, args);
        },
      },
      run: neverRun,
      readRemote: async () => '[]',
      now: () => 1_794_116_000_000,
      timeZone: () => 'UTC',
      reportsRoot: () => '/fixture',
    });
    expect(collector.view()).toEqual({ unavailable: 'the pinned threads have not been read yet' });

    await collector.collect();
    expect(mergedLists[0].map((list) => list.machine)).toEqual(['vm', 'mac', 'windows', 'nas1']);
    await collector.compose(noReports, []);

    const input = composeInputs[0] as Record<string, unknown>;
    expect((input.pins as unknown[]).map((row) => (row as { id: string }).id)).toEqual(['a', 'b']);
    expect(input.machineProblems).toEqual([]);
    expect(input.utcOffsetMinutes).toBe(0);
    const view = collector.view();
    if ('unavailable' in view) throw new Error('expected a composed view');
    expect(view.head).toBe('fake head');
    expect(view.counts.pinned).toBe(2);

    // A later collect sees a changed VM (the pin list is re-read every collect).
    vmPins = [];
    await collector.collect();
    await collector.compose(noReports, []);
    const next = composeInputs.at(-1) as Record<string, unknown>;
    expect((next.pins as unknown[]).map((row) => (row as { id: string }).id)).toEqual(['b']);
  });

  it('a machine that misses once keeps its last good pins; a second miss names it', async () => {
    const { handle, composeInputs } = fakeHandle();
    let macAnswers = true;
    const collector = new PinnedThreadsCollector({
      handle,
      run: neverRun,
      readRemote: async (machine) => {
        if (machine === 'mac' && !macAnswers) throw new Error('ssh did not answer');
        return '[]';
      },
      now: () => 1_794_116_000_000,
      reportsRoot: () => '/fixture',
    });

    await collector.collect(); // everyone answers
    macAnswers = false;
    await collector.collect(); // the Mac's first miss: the grace keeps its (empty) good read
    await collector.compose(noReports, []);
    expect((composeInputs.at(-1) as Record<string, unknown>).machineProblems).toEqual([]);

    await collector.collect(); // the second consecutive miss names it
    await collector.compose(noReports, []);
    const input = composeInputs.at(-1) as Record<string, unknown>;
    expect(input.machineProblems).toEqual([{ machine: 'mac', code: 'did_not_answer' }]);
    const view = collector.view();
    if ('unavailable' in view) throw new Error('expected a composed view');
    expect(view.machineProblems[0]?.text).toBe('unavailable on mac');
  });

  it('an unknown shape from a machine is the shape refusal, after the same grace', async () => {
    const { handle, composeInputs } = fakeHandle({
      remoteShape: (machine) => machine === 'windows' && shaped,
    });
    let shaped = false;
    const collector = new PinnedThreadsCollector({
      handle,
      run: neverRun,
      readRemote: async () => '[]',
      now: () => 1_794_116_000_000,
      reportsRoot: () => '/fixture',
    });
    await collector.collect(); // good
    shaped = true;
    await collector.collect(); // first miss: the grace holds
    await collector.compose(noReports, []);
    expect((composeInputs.at(-1) as Record<string, unknown>).machineProblems).toEqual([]);
    await collector.collect(); // second: named, with the shape code
    await collector.compose(noReports, []);
    expect((composeInputs.at(-1) as Record<string, unknown>).machineProblems).toEqual([
      { machine: 'windows', code: 'shape' },
    ]);
  });

  it("the VM's own unreadable database is a machine problem too", async () => {
    let vmCode: string | null = null;
    const { handle, composeInputs } = fakeHandle({
      vm: () => (vmCode ? { unavailable: vmCode } : { pinned: [] }),
    });
    const collector = new PinnedThreadsCollector({
      handle,
      run: neverRun,
      readRemote: async () => '[]',
      now: () => 1_794_116_000_000,
      reportsRoot: () => '/fixture',
    });
    await collector.collect(); // the VM reads fine
    vmCode = 'missing';
    await collector.collect(); // first miss: the grace holds the empty good read
    await collector.compose(noReports, []);
    expect((composeInputs.at(-1) as Record<string, unknown>).machineProblems).toEqual([]);
    await collector.collect(); // second: named
    await collector.compose(noReports, []);
    expect((composeInputs.at(-1) as Record<string, unknown>).machineProblems).toEqual([
      { machine: 'vm', code: 'missing' },
    ]);
  });

  it('a failed share listing carries into the composer as an unreadable status document', async () => {
    // The compose input is what the native composer words ("Could not be read since ...");
    // here the real docs reader reads a fixture share whose writers folder then cannot be
    // listed (EACCES: an ordinary read failure, not the share's rename-over race).
    const composeInputs: unknown[] = [];
    const handle: NativeStatusHandle = {
      call: async (method, args) => {
        const parsed = JSON.parse(args) as Record<string, unknown>;
        if (method === 'pins') return JSON.stringify({ pinned: [] });
        if (method === 'parseRemotePins') return JSON.stringify({ ok: true, pinned: [] });
        if (method === 'mergePins') return JSON.stringify({ pinned: [] });
        if (method === 'parseStatus') {
          const doc = JSON.parse(
            Buffer.from(String(parsed.contentBase64), 'base64').toString('utf8')
          ) as Record<string, unknown>;
          return JSON.stringify({ ok: true, doc, sha256: `sha:${String(doc.seq)}` });
        }
        if (method === 'compose') {
          composeInputs.push(parsed);
          return JSON.stringify({
            asOfMs: parsed.nowMs,
            head: 'head',
            counts: { pinned: 0, looping: 0, waitingOnYou: 0 },
            machineProblems: [],
            updates: [],
          });
        }
        throw new Error(`unexpected method ${method}`);
      },
    };
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-pinned-docs-'));
    const writer = path.join(dir, 'v1/writers/vm.a');
    fs.mkdirSync(writer, { recursive: true });
    fs.writeFileSync(
      path.join(writer, 'status.json'),
      JSON.stringify({
        schema: 'agent-reports/status@1',
        writer: 'vm.a',
        seq: 5,
        writtenAt: '2026-11-08T05:00:00Z',
        t3Thread: 't1',
        summary: 'Sweeping.',
      })
    );
    const collector = new PinnedThreadsCollector({
      handle,
      run: neverRun,
      readRemote: async () => '[]',
      now: () => 1_794_116_000_000,
      reportsRoot: () => dir,
    });
    try {
      await collector.collect();
      await collector.scanStatusDocs();
      await collector.compose(noReports, []);
      let statuses = (composeInputs.at(-1) as Record<string, unknown>)
        .statuses as Array<Record<string, unknown>>;
      expect(statuses).toHaveLength(1);
      expect(statuses[0].problem).toBeNull();

      fs.chmodSync(path.join(dir, 'v1/writers'), 0o000);
      await collector.scanStatusDocs();
      await collector.compose(noReports, []);
      statuses = (composeInputs.at(-1) as Record<string, unknown>)
        .statuses as Array<Record<string, unknown>>;
      expect(statuses[0].problem).toContain('folder could not be listed');
      expect(typeof statuses[0].problemSinceMs).toBe('number');
    } finally {
      fs.chmodSync(path.join(dir, 'v1/writers'), 0o755);
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('without the module the panel says so with the loader reason', async () => {
    const collector = new PinnedThreadsCollector({
      env: { AAC_STATUS_NATIVE: 'off' },
      now: () => 1,
      reportsRoot: () => '/fixture',
    });
    expect(collector.view()).toEqual({ unavailable: 'the aac-status native module is disabled' });
    await collector.collect(); // no throw
    await collector.compose(noReports, []);
    expect(collector.view()).toEqual({ unavailable: 'the aac-status native module is disabled' });
  });

  it('the dashboard zone becomes the offset the composer needs', () => {
    const november = Date.parse('2026-11-08T05:00:00Z');
    expect(zoneOffsetMinutes('UTC', november)).toBe(0);
    expect(zoneOffsetMinutes('America/New_York', november)).toBe(-300);
    expect(zoneOffsetMinutes('Asia/Kolkata', november)).toBe(330);
  });
});
