import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import type { SnapshotAccount } from '../../../../src/web-server/agent-usage/agent-usage-markdown';
import type { MachineSample } from '../../../../src/web-server/status/status-collectors';
import {
  buildStatusView,
  machineWarnings,
  type Collected,
  type StatusInputs,
} from '../../../../src/web-server/status/status-view';
import {
  ago,
  cardDoc,
  item,
  NOW,
  tasksDoc,
  tempDirs,
  threadDoc,
  writeCard,
  writeReport,
} from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const GIB = 1024 ** 3;
it('retains accepted top-level card media in the Status API view', async () => {
  const root = dirs.make();
  writeCard(
    root,
    cardDoc('vm.lead', 'media', {
      media: [{ kind: 'image', path: '/mnt/Cursor/share/example.png', label: 'built' }],
    })
  );
  const view = buildStatusView(await inputs(root));
  expect(view.cards.open[0].media).toHaveLength(1);
  expect(view.cards.open[0].media[0].path).toBe('/mnt/Cursor/share/example.png');
});
const none = <T>(): Collected<T> => ({ data: null, asOf: null, error: null });

function sample(fields: Partial<MachineSample> = {}): MachineSample {
  return {
    cores: 8,
    load1: 1.234,
    busyPct: 12,
    memAvailableBytes: 16 * GIB,
    memTotalBytes: 32 * GIB,
    disk: { mount: '/', freeBytes: 200 * GIB, totalBytes: 500 * GIB },
    cpuTimes: null,
    ...fields,
  };
}

async function inputs(root: string, extra: Partial<StatusInputs> = {}): Promise<StatusInputs> {
  const reader = new AgentReportsReader({ root: () => root, now: () => NOW });
  await reader.scan();
  const ok = (data: MachineSample): Collected<MachineSample> => ({
    data,
    asOf: NOW - 30_000,
    error: null,
  });
  return {
    now: NOW,
    timeZone: 'America/New_York',
    reports: reader.snapshot(),
    registry: { data: [], asOf: NOW, error: null },
    machines: { vm: ok(sample()), mac: ok(sample()), windows: ok(sample()), nas1: ok(sample()), alexis: ok(sample()) },
    browsers: none(),
    simlock: none(),
    usage: null,
    answers: [],
    ...extra,
  };
}

describe('status view', () => {
  it('counts completion, groups projects and shows board-only streams', async () => {
    const root = path.join(dirs.make(), 'agent-reports');
    writeReport(
      root,
      'vm.lead',
      'tasks.json',
      tasksDoc('vm.lead', [
        item('A', 'landed'),
        item('B', 'done'),
        item('C', 'blocked'),
        item('D', 'dropped', { why: 'Not needed' }),
        item('E', 'todo'),
      ])
    );
    const board = path.join(root, 'boards', 'X.md');
    fs.mkdirSync(path.dirname(board), { recursive: true });
    fs.writeFileSync(board, '# X');
    writeReport(
      root,
      'vm.x-lead',
      'thread.json',
      threadDoc('vm.x-lead', { stream: 'X', links: [{ label: 'Board', path: board }] })
    );
    const view = buildStatusView(await inputs(root));
    expect(view.projects).toHaveLength(1);
    const [project] = view.projects;
    expect(project.label).toBe('Project');
    const tasks = project.streams.find((s) => s.id === 'S');
    expect(tasks?.completion).toEqual({ done: 2, of: 4 });
    expect(tasks?.counts?.total).toBe(5);
    expect(tasks?.counts?.blocked).toBe(1);
    const only = project.streams.find((s) => s.id === 'X');
    expect(only?.source).toBe('board-only');
    expect(only?.completion).toBeNull();
    expect(only?.counts).toBeNull();
    expect(only?.board?.path).toBe(board);
    expect(view.headline.counts.blockedItems).toBe(1);
    expect(view.headline.runs.map((r) => r.text).join('')).toContain('2 of 4 items landed');
  });

  it('marks stale threads, leaves out quiet ones unless blocked, and adds pm-mail threads', async () => {
    const root = dirs.make();
    writeReport(root, 'vm.fresh', 'thread.json', threadDoc('vm.fresh'));
    writeReport(
      root,
      'vm.stale',
      'thread.json',
      threadDoc('vm.stale', { writtenAt: ago(3600_000) })
    );
    writeReport(
      root,
      'vm.quiet',
      'thread.json',
      threadDoc('vm.quiet', { writtenAt: ago(2 * 86_400_000) })
    );
    writeReport(
      root,
      'mac.stuck',
      'thread.json',
      threadDoc('mac.stuck', {
        writtenAt: ago(2 * 86_400_000),
        state: 'blocked',
        blockedOn: { kind: 'owner', what: 'A decision', since: null },
      })
    );
    const view = buildStatusView(
      await inputs(root, {
        registry: {
          data: [
            { name: 'fresh', kind: 'claude', machine: 'vm', about: 'dup', lastSeen: ago(60_000) },
            {
              name: 'helper',
              kind: 'claude',
              machine: 'mac',
              about: 'Helping',
              lastSeen: ago(60_000),
            },
            {
              name: 'old',
              kind: 'claude',
              machine: 'vm',
              about: 'Old',
              lastSeen: ago(3 * 86_400_000),
            },
            { name: 'aac-answers', kind: null, machine: 'vm', about: null, lastSeen: ago(1000) },
          ],
          asOf: NOW,
          error: null,
        },
        hiddenThreadNames: ['aac-answers'],
      })
    );
    const rows = Object.fromEntries(view.threads.rows.map((r) => [r.name, r]));
    expect(Object.keys(rows).sort()).toEqual(['fresh', 'helper', 'stale', 'stuck']);
    expect(rows.fresh.stale).toBe(false);
    expect(rows.stale.stale).toBe(true);
    expect(rows.stuck.state).toBe('blocked');
    expect(rows.helper.source).toBe('pm-mail');
    expect(rows.helper.state).toBeNull();
    expect(rows.helper.doing).toBe('Helping');
  });

  it('sorts open cards by urgency then age, hides answered versions and builds batches', async () => {
    const root = dirs.make();
    writeReport(root, 'vm.lead', 'thread.json', threadDoc('vm.lead', { writtenAt: ago(3600_000) }));
    writeCard(
      root,
      cardDoc('vm.lead', 'Q1', {
        urgency: 'whenever',
        batch: 'b1',
        options: [
          {
            key: 'A',
            label: 'First',
            media: [
              {
                kind: 'video',
                path: 'images/a-b.mp4',
                label: 'built',
                poster: 'images/a b.png'.replace(' ', '-'),
              },
            ],
          },
          { key: 'B', label: 'Second' },
        ],
      })
    );
    writeCard(
      root,
      cardDoc('vm.lead', 'Q2', { urgency: 'now', batch: 'b1', recommendation: null })
    );
    writeCard(root, cardDoc('vm.lead', 'Q3', { urgency: 'today', askedAt: ago(9_000_000) }));
    writeCard(root, cardDoc('vm.lead', 'Q4', { urgency: 'today', askedAt: ago(1_000_000) }));
    writeCard(root, cardDoc('vm.lead', 'Q5', { state: 'withdrawn' }));
    const base = await inputs(root);
    const q4 = base.reports.cards.find((c) => c.doc.id === 'vm.lead--Q4');
    const q1 = base.reports.cards.find((c) => c.doc.id === 'vm.lead--Q1');
    const done = { state: 'done' as const, at: ago(900), detail: 'ok', attempts: 1 };
    const view = buildStatusView({
      ...base,
      answers: [
        {
          id: 'n',
          sendId: null,
          answer: {
            schema: 'agent-reports/answer@1',
            cardId: 'vm.lead--Q1',
            cardSeq: 1,
            cardSha256: q1?.sha256 ?? '',
            kind: 'note',
            choice: [],
            note: 'A third way?',
            via: 'deck',
            answeredAt: ago(2000),
            answeredBy: 'owner',
            device: 'iPhone',
            signature: { alg: 'ed25519', keyId: 'k', value: 'v' },
          },
          card: q1?.doc as never,
          share: done,
          doorbell: done,
        },
        {
          id: 'x',
          sendId: null,
          answer: {
            schema: 'agent-reports/answer@1',
            cardId: 'vm.lead--Q4',
            cardSeq: 1,
            cardSha256: q4?.sha256 ?? '',
            kind: 'answer',
            choice: ['A'],
            note: '',
            via: 'deck',
            answeredAt: ago(1000),
            answeredBy: 'owner',
            device: 'Mac',
            signature: { alg: 'ed25519', keyId: 'k', value: 'v' },
          },
          card: q4?.doc as never,
          share: { state: 'done', at: ago(900), detail: 'ok', attempts: 1 },
          doorbell: { state: 'done', at: ago(800), detail: 'Rang lead.', attempts: 1 },
        },
      ],
    });
    expect(view.cards.open.map((c) => c.id)).toEqual(['vm.lead--Q2', 'vm.lead--Q3']);
    // The asker waits on the owner through its open question cards (N4): its stale
    // heartbeat reads as its state, never as gone.
    expect(view.cards.open[0].askerState).toBe('working');
    expect(view.cards.open[0].cardSha256).toMatch(/^[0-9a-f]{64}$/);
    expect(view.cards.answered[0].card.id).toBe('vm.lead--Q4');
    expect(view.cards.answered[0].answer.delivery.state).toBe('rang');
    expect(view.cards.batches).toEqual([{ id: 'b1', label: 'b1', cardIds: ['vm.lead--Q2'] }]);
    expect(view.cards.answered).toHaveLength(2);
    expect(view.cards.answered[0].answer.kind).toBe('answer');
    const written = view.cards.answered.find((c) => c.card.id === 'vm.lead--Q1');
    expect(written?.answer).toMatchObject({ kind: 'note', note: 'A third way?', choice: [] });
    expect(written?.card.label).toBe('S-Q1');
    expect(written?.card.options[0].media).toEqual([
      {
        kind: 'video',
        path: 'images/a-b.mp4',
        caption: null,
        label: 'built',
        poster: 'images/a-b.png',
        url: '/api/status/files/vm.lead/images/a-b.mp4',
        posterUrl: '/api/status/files/vm.lead/images/a-b.png',
      },
    ]);
    expect(written?.card.options[1]).toEqual({
      key: 'B',
      label: 'Second',
      gets: null,
      costs: null,
      media: [],
      sections: [],
      stance: null,
    });
    expect(view.headline.tone).toBe('bad');
    expect(view.headline.runs[0]).toEqual({ text: '2 questions', strong: true, tone: 'warn' });
  });

  it('the headline names the worst pinned thread: looping first, then waiting on you (D1)', async () => {
    // Built from the composed pinned panel, never from fixture text: whatever sentences the
    // panel's updates carry, the headline names the thread and its looping task itself.
    const pin = (over: Partial<{ looping: unknown; blocked: unknown; title: string }>) => ({
      id: 't1',
      title: over.title ?? 'Thread t1',
      machine: 'vm',
      who: null,
      reportState: 'fresh',
      percent: null,
      percentNote: null,
      now: null,
      blocked: over.blocked ?? { text: 'None.', waitingOnYou: false },
      looping: over.looping ?? { answer: 'cantTell', text: "Can't tell.", task: null, rounds: 0 },
      left: [],
      done: [],
      updated: null,
      reportNote: null,
      staleNote: null,
      unreadableNote: null,
      summary: null,
    });
    const panel = {
      asOfMs: NOW,
      head: '3 pinned in T3 · 1 looping · 1 waiting on you',
      counts: { pinned: 3, looping: 1, waitingOnYou: 1 },
      machineProblems: [],
      updates: [
        pin({
          title: 'Build PMConcept7',
          looping: {
            answer: 'yes',
            text: 'Yes. F-07 has gone back for fixes 5 times without a passing review.',
            task: 'F-07',
            rounds: 5,
          },
        }),
        pin({
          title: 'Redesign PM',
          blocked: { text: 'Waiting on you: pick the tray icon.', waitingOnYou: true },
        }),
        pin({ title: 'Quiet thread' }),
      ],
    };
    const view = buildStatusView(await inputs(dirs.make(), { pinned: panel }));
    const line = view.headline.runs.map((run) => run.text).join('');
    expect(line).toContain('No questions for you');
    expect(line).toContain('1 pinned thread is looping (Build PMConcept7, task F-07)');
    expect(line).toContain('1 pinned thread waits on you (Redesign PM)');
    expect(view.headline.counts.loopingPinned).toBe(1);
    expect(view.headline.counts.waitingPinned).toBe(1);
    expect(view.headline.tone).toBe('warn');
    // Two looping pins: the worst (most rounds) is the one named, the count says both.
    const two = {
      ...panel,
      counts: { pinned: 3, looping: 2, waitingOnYou: 0 },
      updates: [
        pin({
          title: 'Small loop',
          looping: { answer: 'yes', text: 'Yes.', task: 'B-1', rounds: 3 },
        }),
        pin({
          title: 'Big loop',
          looping: { answer: 'yes', text: 'Yes.', task: 'C-9', rounds: 6 },
        }),
        pin({ title: 'Quiet thread' }),
      ],
    };
    const next = buildStatusView(await inputs(dirs.make(), { pinned: two }));
    const text = next.headline.runs.map((run) => run.text).join('');
    expect(text).toContain('2 pinned threads are looping (Big loop, task C-9)');
    expect(next.headline.counts.loopingPinned).toBe(2);
    // An unavailable panel adds nothing: the headline never invents a pin.
    const bare = buildStatusView(
      await inputs(dirs.make(), {
        pinned: { unavailable: 'the aac-status native module is disabled' },
      })
    );
    expect(bare.headline.runs.map((run) => run.text).join('')).not.toContain('pinned thread');
    expect(bare.headline.counts.loopingPinned).toBe(0);
  });

  it('counts only question, judgement and action cards as questions for you (D10)', async () => {
    const root = dirs.make();
    writeCard(root, cardDoc('vm.lead', 'Q1'));
    writeCard(root, cardDoc('vm.lead', 'J1', { kind: 'judgement', recommendation: null }));
    writeCard(
      root,
      cardDoc('vm.lead', 'A1', { kind: 'action', options: [], recommendation: null })
    );
    writeCard(
      root,
      cardDoc('vm.lead', 'O1', { kind: 'objection', options: [], recommendation: null })
    );
    writeCard(
      root,
      cardDoc('vm.lead', 'S1', { kind: 'sample', options: [], recommendation: null })
    );
    const view = buildStatusView(await inputs(root));
    expect(view.cards.open).toHaveLength(5);
    // Sample and objection cards never count as questions; the objection is a decision
    // to look at instead.
    expect(view.headline.counts).toMatchObject({ openCards: 5, questions: 3, decisions: 1 });
    const line = view.headline.runs.map((run) => run.text).join('');
    expect(line).toContain('3 questions for you');
    expect(line).toContain('1 decision to look at');
    expect(line).not.toContain('5 questions');
    expect(view.headline.tone).toBe('warn');
    // A sample card alone says nothing at all.
    const onlySample = dirs.make();
    writeCard(
      onlySample,
      cardDoc('vm.lead', 'S1', { kind: 'sample', options: [], recommendation: null })
    );
    const bare = buildStatusView(await inputs(onlySample));
    expect(bare.headline.counts).toMatchObject({ openCards: 1, questions: 0, decisions: 0 });
    expect(bare.headline.runs.map((run) => run.text).join('')).toContain('No questions for you');
  });

  it('null ttl exempts only a thread waiting on the owner (F10)', async () => {
    // The asker state reads the same staleness ("gone" when stale), where the thread
    // rows themselves are quietly hidden after a day.
    const askerOf = async (writer: string, thread: Record<string, unknown>, kind: string) => {
      const root = dirs.make();
      writeReport(root, writer, 'thread.json', threadDoc(writer, thread));
      writeCard(
        root,
        cardDoc(
          writer,
          'C1',
          kind === 'question' ? {} : { kind: kind as never, options: [], recommendation: null }
        )
      );
      const view = buildStatusView(await inputs(root));
      return view.cards.open[0]?.askerState;
    };
    // A crashed working thread with no heartbeat goes stale after the one-day default.
    expect(
      await askerOf(
        'vm.worker',
        { ttlSeconds: null, state: 'working', writtenAt: ago(2 * 86_400_000) },
        'sample'
      )
    ).toBe('gone');
    // Blocked and waiting states with no heartbeat do not.
    expect(
      await askerOf(
        'vm.blocked',
        { ttlSeconds: null, state: 'blocked', writtenAt: ago(2 * 86_400_000) },
        'sample'
      )
    ).toBe('blocked');
    expect(
      await askerOf(
        'vm.waiting',
        { ttlSeconds: null, state: 'waiting', writtenAt: ago(2 * 86_400_000) },
        'sample'
      )
    ).toBe('waiting');
    // Waiting through an open question card does not.
    expect(
      await askerOf(
        'vm.asked',
        { ttlSeconds: null, state: 'working', writtenAt: ago(2 * 86_400_000) },
        'question'
      )
    ).toBe('working');
    // An objection card asks nothing of the owner: still stale.
    expect(
      await askerOf(
        'vm.object',
        { ttlSeconds: null, state: 'working', writtenAt: ago(2 * 86_400_000) },
        'objection'
      )
    ).toBe('gone');
  });

  it('waiting on the owner is never stale, whatever the ttl (N4)', async () => {
    const askerOf = async (writer: string, thread: Record<string, unknown>, kind: string) => {
      const root = dirs.make();
      writeReport(root, writer, 'thread.json', threadDoc(writer, thread));
      writeCard(
        root,
        cardDoc(
          writer,
          'C1',
          kind === 'question' ? {} : { kind: kind as never, options: [], recommendation: null }
        )
      );
      const view = buildStatusView(await inputs(root));
      return view.cards.open[0]?.askerState;
    };
    // The reviewer's case: waiting, ttl set, 3 h past it, one open question card.
    expect(
      await askerOf(
        'vm.asked',
        { ttlSeconds: 1800, state: 'waiting', writtenAt: ago(3 * 3600_000) },
        'question'
      )
    ).toBe('waiting');
    // A blockedOn naming the owner exempts whatever the ttl.
    expect(
      await askerOf(
        'vm.blocked',
        {
          ttlSeconds: 1800,
          state: 'blocked',
          writtenAt: ago(3 * 3600_000),
          blockedOn: { kind: 'owner', what: 'A decision', since: null },
        },
        'sample'
      )
    ).toBe('blocked');
    // Without an owner signal the same report is stale.
    expect(
      await askerOf(
        'vm.plain',
        { ttlSeconds: 1800, state: 'working', writtenAt: ago(3 * 3600_000) },
        'sample'
      )
    ).toBe('gone');
  });

  it('keeps objection cards without a response open, and shows quiet threads by age', async () => {
    const root = dirs.make();
    writeCard(
      root,
      cardDoc('vm.lead', 'O1', {
        kind: 'objection',
        options: [],
        recommendation: null,
        reversible: { yes: true, how: 'Revert the commit.' },
      })
    );
    writeReport(
      root,
      'vm.lead',
      'thread.json',
      threadDoc('vm.lead', { ttlSeconds: null, state: 'done', writtenAt: ago(5 * 3600_000) })
    );
    const view = buildStatusView(await inputs(root));
    expect(view.cards.open.map((c) => [c.id, c.kind])).toEqual([['vm.lead--O1', 'objection']]);
    expect(view.cards.open[0].reversible).toEqual({ yes: true, how: 'Revert the commit.' });
    expect(view.cards.open[0].askerState).toBe('done');
    const thread = view.threads.rows.find((t) => t.name === 'lead');
    expect(thread?.stale).toBe(false);
    expect(thread?.ageSeconds).toBe(5 * 3600);
  });

  it('names unreadable report files in the source and never calls them live', async () => {
    const root = dirs.make();
    writeReport(root, 'vm.lead', 'tasks.json', Buffer.from('{oops'));
    const view = buildStatusView(await inputs(root));
    const source = view.sources.find((s) => s.id === 'agent-reports');
    expect(source?.state).toBe('unreadable');
    expect(source?.detail).toBe(
      '1 file unreadable since 11:30 AM: vm.lead/tasks.json (not valid JSON).'
    );
    expect(view.sources.find((s) => s.id === 'quota-watch')?.state).toBe('unconfigured');
    expect(view.sources.find((s) => s.id === 'usage')?.state).toBe('missing');
  });

  it('shows machine floors, unreachable hosts with their last reading, and browser counts', async () => {
    const root = dirs.make();
    const view = buildStatusView(
      await inputs(root, {
        machines: {
          vm: { data: sample(), asOf: NOW - 10_000, error: null },
          mac: {
            data: sample({
              disk: { mount: '/System/Volumes/Data', freeBytes: 17.7 * GIB, totalBytes: 460 * GIB },
            }),
            asOf: NOW - 10_000,
            error: null,
          },
          windows: {
            data: sample(),
            asOf: NOW - 600_000,
            error: { reason: 'ssh did not answer in time.', since: NOW - 120_000 },
          },
          nas1: { data: sample({ memAvailableBytes: 4.2 * GIB }), asOf: NOW - 10_000, error: null },
          alexis: {data:sample(), asOf:NOW-30_000, error:null},
        },
        browsers: {
          data: [
            {
              machine: 'vm',
              label: 'VM',
              browsers: { running: 6, max: 12 },
              recordings: { running: 1, max: 5 },
              gpuPct: 39,
              memGb: 15.4,
              sessions: [],
            },
          ],
          asOf: NOW - 5000,
          error: null,
        },
      })
    );
    const rows = Object.fromEntries(view.machines.rows.map((r) => [r.id, r]));
    expect(rows.mac.warnings).toEqual(['Disk 17.7 GB free, under the 50 GB floor']);
    expect(rows.nas1.warnings).toEqual(['Memory 4.2 GB available, under the 6 GB floor']);
    expect(rows.windows.state).toBe('unreachable');
    expect(rows.windows.detail).toBe(
      'ssh did not answer in time (since 11:28 AM). Showing the reading from 11:20 AM.'
    );
    expect(rows.windows.memory?.availableGb).toBe(16);
    expect(rows.vm.browsers).toEqual({ running: 6, max: 12 });
    expect(rows.vm.cpu?.load1).toBe(1.2);
    expect(view.sources.find((s) => s.id === 'machines')?.state).toBe('stale');
    expect(machineWarnings(null)).toEqual([]);
  });

  it('warns about accounts projected full before their reset or at their limit, from current readings only', async () => {
    const root = dirs.make();
    const window = (fields: Record<string, unknown>) => ({
      key: 'seven_day',
      label: '7-day',
      kind: 'rate_limit',
      usedPercent: 60,
      remainingPercent: 40,
      resetAt: '2026-11-12T21:00:00.000Z',
      resetPassed: false,
      windowMinutes: 10080,
      used: null,
      limit: null,
      remaining: null,
      unit: null,
      expiresAt: null,
      unlimited: null,
      pool: null,
      poolId: null,
      modelIds: [],
      sampledAt: ago(30_000),
      cached: false,
      burn: null,
      ...fields,
    });
    const account = (label: string, status: string, windows: unknown[], hidden = false) =>
      ({
        id: `claude:${label}`,
        provider: 'claude',
        providerLabel: 'Claude',
        label,
        email: null,
        profile: null,
        source: null,
        plan: null,
        platform: 'ubuntu',
        host: 'ubuntu',
        active: true,
        status,
        sampledAt: ago(30_000),
        ageSeconds: 30,
        hidden,
        windows,
      }) as unknown as SnapshotAccount;
    const burn = (full: number, before: boolean) => ({
      last5Minutes: null,
      last15Minutes: null,
      lastHour: 2,
      last6Hours: null,
      last24Hours: null,
      pace: 2,
      paceMinutes: 60,
      projectedFullAt: new Date(NOW + full).toISOString(),
      runsOutBeforeReset: before,
      periodStartedAt: null,
    });
    const view = buildStatusView(
      await inputs(root, {
        usage: {
          sampledAt: NOW - 30_000,
          staleAfterSeconds: 300,
          accounts: [
            account('main', 'ok', [window({ burn: burn(126_000_000, true) })]),
            account('full', 'ok', [window({ usedPercent: 100 })]),
            account('cached', 'cached', [window({ usedPercent: 100 })]),
            account('later', 'ok', [window({ burn: burn(999_000_000, false) })]),
            account('hidden', 'ok', [window({ usedPercent: 100 })], true),
          ],
        },
      })
    );
    expect(view.usage.warnings).toEqual([
      { tone: 'bad', text: 'claude full (7-day): at its limit until its Nov 12 4:00 PM reset' },
      { tone: 'warn', text: 'claude main (7-day): full in 1d 11h, before its Nov 12 reset' },
    ]);
    expect(view.usage.accounts.map((a) => a.label)).toEqual(['cached', 'full', 'later', 'main']);
    expect(view.usage.accounts.find((a) => a.label === 'cached')?.stale).toBe(true);
    expect(view.usage.accounts.find((a) => a.label === 'full')?.atLimit).toBe(true);
  });
});
