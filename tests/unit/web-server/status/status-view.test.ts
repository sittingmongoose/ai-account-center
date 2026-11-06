import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import type { SnapshotAccount } from '../../../../src/web-server/agent-usage/agent-usage-snapshot';
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
    machines: { vm: ok(sample()), mac: ok(sample()), windows: ok(sample()), nas1: ok(sample()) },
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
          { key: 'A', label: 'First', image: { path: 'images/a-b.png', kind: 'mockup' } },
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
    expect(view.cards.open.map((c) => c.id)).toEqual(['vm.lead--Q2', 'vm.lead--Q3', 'vm.lead--Q1']);
    expect(view.cards.open[0].askerState).toBe('gone');
    expect(view.cards.open[0].cardSha256).toMatch(/^[0-9a-f]{64}$/);
    expect(view.cards.answered[0].card.id).toBe('vm.lead--Q4');
    expect(view.cards.answered[0].answer.delivery.state).toBe('rang');
    expect(view.cards.batches).toEqual([
      { id: 'b1', label: 'b1', cardIds: ['vm.lead--Q2', 'vm.lead--Q1'] },
    ]);
    expect(view.cards.answered).toHaveLength(1);
    expect(view.cards.answered[0].answer.kind).toBe('answer');
    const open1 = view.cards.open.find((c) => c.id === 'vm.lead--Q1');
    expect(open1?.notes).toEqual([
      { note: 'A third way?', at: ago(2000), delivery: expect.objectContaining({ state: 'rang' }) },
    ]);
    expect(open1?.label).toBe('S-Q1');
    expect(open1?.options[0].image).toEqual({
      url: '/api/status/files/vm.lead/images/a-b.png',
      kind: 'mockup',
    });
    expect(open1?.options[1]).toEqual({
      key: 'B',
      label: 'Second',
      gets: null,
      costs: null,
      image: null,
    });
    expect(view.headline.tone).toBe('bad');
    expect(view.headline.runs[0]).toEqual({ text: '3 questions', strong: true, tone: 'warn' });
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
