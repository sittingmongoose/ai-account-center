import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { NativeStatusHandle } from '../../../src/status-native/native-loader';
import type { PinnedThread } from '../../../src/status-native/pinned';
import {
  modelLabelFor,
  PinnedThreadsCollector,
  zoneOffsetMinutes,
  type PinsMachine,
} from '../../../src/web-server/status/pinned-threads';
import {
  AgentReportsReader,
  type AgentReportsState,
} from '../../../src/web-server/status/agent-reports-reader';
import type { CommandRunner } from '../../../src/web-server/status/status-collectors';
import {
  cardDoc,
  item,
  tasksDoc,
  threadDoc,
  writeCard,
  writeReport,
} from '../web-server/status/status-test-helpers';

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

  it('the composer input carries the catalog model label and the project label (D9)', async () => {
    expect(modelLabelFor('claude-opus-5-5')).toBe('Opus 5.5');
    expect(modelLabelFor('anthropic/claude-opus-5-5')).toBe('Opus 5.5');
    expect(modelLabelFor('gpt-6.1-sol')).toBe('Sol 6.1');
    expect(modelLabelFor('unknown-model-9')).toBeNull();
    expect(modelLabelFor(null)).toBeNull();

    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-pinned-who-'));
    try {
      writeReport(dir, 'vm.lead', 'thread.json', threadDoc('vm.lead'));
      writeReport(
        dir,
        'vm.lead',
        'tasks.json',
        tasksDoc('vm.lead', [item('F-01', 'done')], {
          project: 'puppet-master',
          projectLabel: 'Puppet Master',
          stream: 'F',
          streamLabel: 'PM7 follow-ups',
        })
      );
      const reader = new AgentReportsReader({
        root: () => dir,
        now: () => 1_794_116_000_000,
      });
      await reader.scan();
      expect(reader.snapshot().problems).toEqual([]);
      const { handle, composeInputs } = fakeHandle({
        vm: () => ({ pinned: [pin('t1')] }),
      });
      const collector = new PinnedThreadsCollector({
        handle,
        run: neverRun,
        readRemote: async () => '[]',
        now: () => 1_794_116_000_000,
        reportsRoot: () => dir,
      });
      await collector.collect();
      await collector.compose(reader.snapshot(), []);
      const input = composeInputs.at(-1) as Record<string, unknown>;
      const threads = input.threads as Array<Record<string, unknown>>;
      const tasks = input.tasks as Array<Record<string, unknown>>;
      expect(threads).toHaveLength(1);
      expect(threads[0].model).toBe('claude-opus-5-5');
      expect(threads[0].modelLabel).toBe('Opus 5.5');
      expect(tasks).toHaveLength(1);
      expect(tasks[0].projectLabel).toBe('Puppet Master');
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('open cards reach the composer with their kinds so only questions can wait (D10)', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-pinned-kinds-'));
    try {
      writeCard(dir, cardDoc('vm.lead', 'Q1'));
      writeCard(
        dir,
        cardDoc('vm.lead', 'O1', { kind: 'objection', options: [], recommendation: null })
      );
      writeCard(
        dir,
        cardDoc('vm.lead', 'S1', { kind: 'sample', options: [], recommendation: null })
      );
      const reader = new AgentReportsReader({
        root: () => dir,
        now: () => 1_794_116_000_000,
      });
      await reader.scan();
      expect(reader.snapshot().problems).toEqual([]);
      const { handle, composeInputs } = fakeHandle({
        vm: () => ({ pinned: [pin('t1')] }),
      });
      const collector = new PinnedThreadsCollector({
        handle,
        run: neverRun,
        readRemote: async () => '[]',
        now: () => 1_794_116_000_000,
        reportsRoot: () => dir,
      });
      await collector.collect();
      await collector.compose(reader.snapshot(), []);
      const input = composeInputs.at(-1) as Record<string, unknown>;
      const cards = input.openCards as Array<Record<string, unknown>>;
      expect(cards.map((card) => card.kind).sort()).toEqual(['objection', 'question', 'sample']);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('the dashboard zone becomes the offset the composer needs', () => {
    const november = Date.parse('2026-11-08T05:00:00Z');
    expect(zoneOffsetMinutes('UTC', november)).toBe(0);
    expect(zoneOffsetMinutes('America/New_York', november)).toBe(-300);
    expect(zoneOffsetMinutes('Asia/Kolkata', november)).toBe(330);
  });

  it('a status@1 refusal is carried into the pinned view with its file and reason', async () => {
    // Item 38: the reader retains the TS refusal file/reason/since; the pinned field must
    // project them instead of showing a bare "doesn't report its status yet".
    const composeInputs: unknown[] = [];
    const handle: NativeStatusHandle = {
      call: async (method, args) => {
        const parsed = JSON.parse(args) as Record<string, unknown>;
        if (method === 'pins') return JSON.stringify({ pinned: [{ id: 'fixture-thread', title: 'Fixture', pinnedAt: 'a', pinOrderKey: 'a' }] });
        if (method === 'parseRemotePins') return JSON.stringify({ ok: true, pinned: [] });
        if (method === 'mergePins') return JSON.stringify({ pinned: [{ id: 'fixture-thread', title: 'Fixture', pinnedAt: 'a', pinOrderKey: 'a', machine: 'vm' }] });
        if (method === 'parseStatus') throw new Error('must not reach native: TypeScript refuses first');
        if (method === 'compose') {
          composeInputs.push(parsed);
          const statusProblems = (parsed.statusProblems ?? []) as Array<Record<string, unknown>>;
          const statuses = (parsed.statuses ?? []) as Array<Record<string, unknown>>;
          const updates = statuses.map((status) => {
            const problem = status.problem as string | null;
            const hasRefusal = typeof problem === 'string' && problem.length > 0;
            return {
              id: String(status.t3Thread ?? 'fixture-thread'),
              title: 'Fixture',
              machine: 'vm',
              who: null,
              reportState: hasRefusal ? 'unreadable' : 'none',
              percent: null,
              percentNote: hasRefusal ? null : 'No task list.',
              now: hasRefusal ? null : 'Fixture clean',
              blocked: { text: 'None.', waitingOnYou: false },
              looping: { answer: 'cantTell', text: "Can't tell." },
              left: [],
              done: [],
              updated: null,
              reportNote: hasRefusal ? null : "This thread doesn't report its status yet.",
              staleNote: null,
              unreadableNote: hasRefusal ? 'Could not be read since 12:00 PM.' : null,
              summary: null,
              refusal: hasRefusal
                ? { file: `writers/${String(status.writer)}/status.json`, reason: problem, sinceMs: status.problemSinceMs as number | null }
                : null,
            };
          });
          // A problem with no doc also appears as a standalone statusProblems entry: the
          // collector's statusProblems is the full list; the composer passes it through.
          const allProblems = statusProblems;
          return JSON.stringify({
            asOfMs: parsed.nowMs,
            head: '1 pinned in T3',
            counts: { pinned: 1, looping: 0, waitingOnYou: 0 },
            machineProblems: [],
            statusProblems: allProblems,
            updates: updates.length > 0 ? updates : (statusProblems.length > 0 ? [{ id: 'fixture-thread', title: 'Fixture', machine: 'vm', who: null, reportState: 'none', percent: null, percentNote: 'No task list.', now: null, blocked: { text: 'Not reported.', waitingOnYou: false }, looping: { answer: 'cantTell', text: "Can't tell." }, left: [], done: [], updated: null, reportNote: "This thread doesn't report its status yet.", staleNote: null, unreadableNote: null, summary: null, refusal: statusProblems.length > 0 ? { file: String(statusProblems[0].file), reason: String(statusProblems[0].reason), sinceMs: statusProblems[0].sinceMs as number | null } : null }] : []),
          });
        }
        throw new Error(`unexpected method ${method}`);
      },
    };
    const dir = fs.mkdtempSync(require('os').tmpdir() + '/aac-pinned-refusal-');
    const writerDir = require('path').join(dir, 'v1/writers/vm.probe');
    require('fs').mkdirSync(writerDir, { recursive: true });
    require('fs').writeFileSync(
      require('path').join(writerDir, 'status.json'),
      JSON.stringify({
        schema: 'agent-reports/status@1',
        writer: 'vm.probe',
        seq: 9,
        writtenAt: '2026-11-08T12:00:00Z',
        t3Thread: 'fixture-thread',
        summary: 'Bearer\uFEFF' + 'i'.repeat(24),
      })
    );
    try {
      const collector = new PinnedThreadsCollector({
        handle,
        run: neverRun,
        readRemote: async () => '[]',
        now: () => Date.parse('2026-11-08T12:00:00Z'),
        reportsRoot: () => dir,
      });
      await collector.collect();
      await collector.scanStatusDocs();
      const snap = collector.docs.snapshot();
      // The reader retains the TS refusal even though the native module never saw it.
      expect(snap.problems.length).toBe(1);
      expect(snap.problems[0].file).toBe('writers/vm.probe/status.json');
      expect(snap.problems[0].reason.toLowerCase()).toContain('bearer');
      expect(snap.docs.length).toBe(0);
      await collector.compose(noReports, []);
      const input = composeInputs.at(-1) as Record<string, unknown>;
      const statusProblems = input.statusProblems as Array<Record<string, unknown>>;
      expect(statusProblems.length).toBe(1);
      expect(statusProblems[0].file).toBe('writers/vm.probe/status.json');
      expect(String(statusProblems[0].reason).toLowerCase()).toContain('bearer');
      expect(typeof statusProblems[0].sinceMs).toBe('number');
      expect(statusProblems[0].sinceMs).toBe(Date.parse('2026-11-08T12:00:00Z'));
      const view = collector.view();
      if ('unavailable' in view) throw new Error('expected a composed view');
      const v = view as Record<string, unknown>;
      const viewProblems = v.statusProblems as Array<Record<string, unknown>> | undefined;
      // Before the fix the pinned field dropped file and reason entirely.
      expect(viewProblems?.length).toBe(1);
      expect(viewProblems?.[0]?.file).toBe('writers/vm.probe/status.json');
      expect(String(viewProblems?.[0]?.reason).toLowerCase()).toContain('bearer');
      const updates = view.updates as Array<Record<string, unknown>>;
      // A first refusal with no accepted doc must not look like "no report": the view carries
      // the refusal. The held-copy case likewise carries the per-update refusal.
      if (updates.length > 0 && updates[0]?.refusal) {
        const refusal = updates[0].refusal as Record<string, unknown>;
        expect(refusal.file).toBe('writers/vm.probe/status.json');
        expect(String(refusal.reason).toLowerCase()).toContain('bearer');
      } else {
        // For the first refusal the top-level statusProblems already carries it; either is a
        // disclosure, a bare "none" with no file/reason is not.
        const hasTopLevelReason = (String(viewProblems?.[0]?.reason).toLowerCase()).includes('bearer');
        expect(hasTopLevelReason).toBe(true);
        expect(JSON.stringify(view)).not.toContain('Bearer\uFEFF');
      }
    } finally {
      require('fs').rmSync(dir, { recursive: true, force: true });
    }
  });

  it('a held status document keeps its refusal file and reason on its own update', async () => {
    const composeInputs: unknown[] = [];
    const handle: NativeStatusHandle = {
      call: async (method, args) => {
        const parsed = JSON.parse(args) as Record<string, unknown>;
        if (method === 'pins') return JSON.stringify({ pinned: [{ id: 'fixture-thread', title: 'Fixture', pinnedAt: 'a', pinOrderKey: 'a' }] });
        if (method === 'parseRemotePins') return JSON.stringify({ ok: true, pinned: [] });
        if (method === 'mergePins') return JSON.stringify({ pinned: [{ id: 'fixture-thread', title: 'Fixture', pinnedAt: 'a', pinOrderKey: 'a', machine: 'vm' }] });
        if (method === 'parseStatus') {
          const doc = JSON.parse(Buffer.from(String(parsed.contentBase64), 'base64').toString('utf8')) as Record<string, unknown>;
          return JSON.stringify({ ok: true, doc, sha256: 'sha1' });
        }
        if (method === 'compose') {
          composeInputs.push(parsed);
          const statuses = (parsed.statuses ?? []) as Array<Record<string, unknown>>;
          const problems = (parsed.statusProblems ?? []) as Array<Record<string, unknown>>;
          const s = statuses[0] ?? {};
          const hasProblem = typeof s.problem === 'string' && (s.problem as string).length > 0;
          return JSON.stringify({
            asOfMs: parsed.nowMs,
            head: '1 pinned in T3',
            counts: { pinned: 1, looping: 0, waitingOnYou: 0 },
            machineProblems: [],
            statusProblems: problems,
            updates: [{
              id: 'fixture-thread',
              title: 'Fixture',
              machine: 'vm',
              who: null,
              reportState: hasProblem ? 'unreadable' : 'fresh',
              percent: null,
              percentNote: 'No task list.',
              now: 'Fixture clean',
              blocked: { text: 'None.', waitingOnYou: false },
              looping: { answer: 'cantTell', text: "Can't tell." },
              left: [], done: [], updated: null,
              reportNote: null, staleNote: null,
              unreadableNote: hasProblem ? 'Could not be read since 12:00 PM.' : null,
              summary: 'Fixture clean',
              refusal: hasProblem ? { file: `writers/${String(s.writer)}/status.json`, reason: s.problem, sinceMs: s.problemSinceMs as number | null } : null,
            }],
          });
        }
        throw new Error(`unexpected method ${method}`);
      },
    };
    const dir = fs.mkdtempSync(require('os').tmpdir() + '/aac-pinned-held-');
    const writerDir = require('path').join(dir, 'v1/writers/vm.held');
    require('fs').mkdirSync(writerDir, { recursive: true });
    const good = {
      schema: 'agent-reports/status@1',
      writer: 'vm.held',
      seq: 5,
      writtenAt: '2026-11-08T05:00:00Z',
      t3Thread: 'fixture-thread',
      summary: 'Fixture clean',
    };
    require('fs').writeFileSync(require('path').join(writerDir, 'status.json'), JSON.stringify(good));
    try {
      const collector = new PinnedThreadsCollector({
        handle,
        run: neverRun,
        readRemote: async () => '[]',
        now: () => Date.parse('2026-11-08T12:00:00Z'),
        reportsRoot: () => dir,
      });
      await collector.collect();
      await collector.scanStatusDocs();
      await collector.compose(noReports, []);
      // Refuse with a secret: the held copy stays unreadable and this scan's problem is carried.
      require('fs').writeFileSync(
        require('path').join(writerDir, 'status.json'),
        JSON.stringify({ ...good, seq: 6, summary: 'Bearer\uFEFF' + 'x'.repeat(24) })
      );
      await collector.scanStatusDocs();
      await collector.compose(noReports, []);
      const view = collector.view();
      if ('unavailable' in view) throw new Error('expected a view');
      const v = view as Record<string, unknown>;
      const viewProblems = v.statusProblems as Array<Record<string, unknown>>;
      expect(viewProblems?.length).toBe(1);
      expect(viewProblems?.[0]?.file).toBe('writers/vm.held/status.json');
      expect(String(viewProblems?.[0]?.reason).toLowerCase()).toContain('bearer');
      const upd = (view.updates as Array<Record<string, unknown>>)[0];
      expect(upd?.reportState).toBe('unreadable');
      const refusal = upd?.refusal as Record<string, unknown> | null;
      expect(refusal?.file).toBe('writers/vm.held/status.json');
      expect(String(refusal?.reason).toLowerCase()).toContain('bearer');
      expect(JSON.stringify(view)).not.toContain('Bearer\uFEFF');
    } finally {
      require('fs').rmSync(dir, { recursive: true, force: true });
    }
  });
});
