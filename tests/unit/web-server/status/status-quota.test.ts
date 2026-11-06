import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import {
  parseQuotaReport,
  QuotaReportReader,
  type QuotaReaderFs,
} from '../../../../src/web-server/status/quota-report';
import {
  amountText,
  buildSpend,
  buildStatusView,
  meterLabel,
  quotaSource,
} from '../../../../src/web-server/status/status-view';
import { ago, item, NOW, tasksDoc, tempDirs, writeReport } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const MIN = 60_000;

function report(fields: Record<string, unknown> = {}) {
  const period = (id: string, coverage: number) => ({
    period: id,
    start: ago(6 * 60 * MIN),
    end: ago(0),
    coverage,
    rows: [
      {
        stream: 'S',
        account: 'codex:account-a',
        meter: 'credits_balance',
        unit: 'credits',
        value: 17.774,
        estimate: true,
        method: 'm',
      },
      {
        stream: 'S',
        account: 'claude:account-b',
        meter: 'seven_day',
        unit: 'percent',
        value: 4.68,
        estimate: true,
        method: 'm',
      },
      {
        stream: '(no stream)',
        account: 'claude:account-b',
        meter: 'five_hour',
        unit: 'percent',
        value: null,
        estimate: true,
        method: 'm',
      },
      {
        stream: 'OTHER',
        account: 'codex:account-a',
        meter: 'credits_balance',
        unit: 'credits',
        value: 0.001,
        estimate: true,
        method: 'm',
      },
    ],
  });
  return {
    schema: 'quota-report@1',
    writer: 'vm.quota-watch',
    seq: 5,
    writtenAt: ago(3 * MIN),
    generatedAt: ago(3 * MIN),
    estimate: true,
    methodId: 'token-share-at-list-price',
    method: 'Each meter is split by each session’s share of tokens at list price.',
    streamEstimates: {
      day: period('day-2026-11-06', 0.67),
      week: period('week-2026-11-02', 0.09),
      window: { ...period('window', 1), period: null },
    },
    budgetRows: [
      {
        stream: 'S',
        desc: 'Codex credits',
        accounts: ['codex:account-a'],
        unit: 'credits',
        meter: 'credits_balance',
        per: 'day',
        limit: null,
        use: 17.77,
        known: true,
        percent: null,
        period: 'day-2026-11-06',
        coverage: 0.67,
      },
      {
        stream: 'S',
        desc: 'Claude weekly',
        accounts: ['claude:account-b'],
        unit: 'percent',
        meter: 'seven_day',
        per: 'week',
        limit: 10,
        use: 10.4,
        known: false,
        percent: 104,
        period: 'week-2026-11-02',
        coverage: 0.09,
      },
      {
        stream: 'OTHER',
        desc: 'Nothing read',
        accounts: ['codex:account-a'],
        unit: 'credits',
        meter: 'credits_balance',
        per: 'day',
        limit: 5,
        use: null,
        known: false,
        percent: null,
        period: 'day-2026-11-06',
        coverage: null,
      },
    ],
    unattributedRows: [
      {
        account: 'codex:account-a',
        meter: 'credits_balance',
        unit: 'credits',
        amount: 470.533,
        reason: '(no stream)',
        why: 'no stream rule claims these threads',
        estimate: true,
      },
      {
        account: 'claude:account-b',
        meter: 'seven_day',
        unit: 'percent',
        amount: null,
        reason: '(no reading)',
        why: null,
        estimate: true,
      },
      {
        account: 'claude:account-c',
        meter: 'five_hour',
        unit: 'percent',
        amount: 2.5,
        reason: '(no session seen)',
        why: 'measured',
        estimate: false,
      },
    ],
    machinesNotScanned: [
      { name: 'mac', thisPass: true, error: 'ssh timed out', runsMissed: 2, runsInWindow: 36 },
    ],
    accounts: { ignored: true },
    ...fields,
  };
}

function writeQuota(root: string, value: unknown): void {
  const file = path.join(root, 'v1', 'quota', 'report.json');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, Buffer.isBuffer(value) ? value : JSON.stringify(value));
}

function reader(root: string, now = () => NOW, fsOverride?: QuotaReaderFs) {
  return new QuotaReportReader({ root: () => root, now, fs: fsOverride });
}

describe('quota-watch report: reading and source states', () => {
  it('is missing without a folder or a file, then ok with its age', async () => {
    const root = dirs.make();
    const r = reader(root);
    expect(quotaSource(r.snapshot(), NOW, 'UTC')).toMatchObject({
      state: 'missing',
      detail: 'Waiting for the first scan.',
    });
    await r.scan();
    expect(quotaSource(r.snapshot(), NOW, 'UTC').state).toBe('missing');
    fs.mkdirSync(path.join(root, 'v1', 'quota'), { recursive: true });
    await r.scan();
    expect(quotaSource(r.snapshot(), NOW, 'UTC')).toMatchObject({
      state: 'missing',
      detail: 'No quota-watch report at v1/quota/report.json.',
    });
    writeQuota(root, report());
    expect(await r.scan()).toBe(true);
    expect(quotaSource(r.snapshot(), NOW, 'UTC')).toMatchObject({
      state: 'ok',
      ageSeconds: 180,
      staleAfterSeconds: 1800,
      detail: 'Report from 4:27 PM. Every number is an estimate.',
    });
    expect(quotaSource(undefined, NOW, 'UTC').state).toBe('unconfigured');
  });

  it('is stale past three missed passes, even when the writer’s clock runs ahead', async () => {
    const root = dirs.make();
    writeQuota(root, report({ writtenAt: ago(31 * MIN) }));
    const r = reader(root);
    await r.scan();
    expect(quotaSource(r.snapshot(), NOW, 'UTC')).toMatchObject({
      state: 'stale',
      ageSeconds: 31 * 60,
    });
    const ahead = dirs.make();
    writeQuota(ahead, report({ writtenAt: new Date(NOW + 3600_000).toISOString() }));
    let now = NOW;
    const r2 = reader(ahead, () => now);
    await r2.scan();
    now += 31 * MIN;
    expect(quotaSource(r2.snapshot(), now, 'UTC').state).toBe('stale');
  });

  it('names an unreadable file and keeps the last good copy; refuses NUL bytes, oversize, bad schema', async () => {
    const root = dirs.make();
    const r = reader(root);
    writeQuota(root, Buffer.from('{"schema": "quota-'));
    await r.scan();
    expect(r.snapshot().good).toBeNull();
    expect(quotaSource(r.snapshot(), NOW, 'UTC')).toMatchObject({
      state: 'unreadable',
      detail: 'quota/report.json unreadable since 4:30 PM (not valid JSON).',
    });
    writeQuota(root, report());
    await r.scan();
    expect(quotaSource(r.snapshot(), NOW, 'UTC').state).toBe('ok');
    writeQuota(
      root,
      Buffer.concat([Buffer.from(JSON.stringify(report({ seq: 6 }))), Buffer.alloc(8)])
    );
    await r.scan();
    expect(r.snapshot().good?.doc.seq).toBe(5);
    expect(quotaSource(r.snapshot(), NOW, 'UTC').detail).toContain(
      'contains NUL bytes from a damaged write'
    );
    expect(quotaSource(r.snapshot(), NOW, 'UTC').detail).toContain('Showing the report from');
    expect(parseQuotaReport(Buffer.alloc(513 * 1024, 32))).toEqual({
      ok: false,
      reason: 'larger than the 512 KB cap',
    });
    expect(
      parseQuotaReport(Buffer.from(JSON.stringify(report({ schema: 'quota-report@2' }))))
    ).toEqual({
      ok: false,
      reason: 'schema is missing or not valid',
    });
    expect(parseQuotaReport(Buffer.from(JSON.stringify(report({ estimate: false }))))).toEqual({
      ok: false,
      reason: 'estimate is missing or not valid',
    });
    expect(
      parseQuotaReport(
        Buffer.from(
          JSON.stringify(report({ method: 'token sk-ant-abcdefghijklmnopqrstuvwxyz0123' }))
        )
      )
    ).toEqual({ ok: false, reason: 'refused: it contains what looks like an API key' });
  });

  it('reports a folder that cannot be listed as an error, and rides out ENOENT and ESTALE', async () => {
    const root = dirs.make();
    writeQuota(root, report());
    let fail: string | null = null;
    const fsOverride: QuotaReaderFs = {
      readdir: async (dir) => {
        if (fail === 'EACCES') throw Object.assign(new Error('denied'), { code: 'EACCES' });
        if (fail === 'ESTALE-dir')
          throw Object.assign(new Error('Unknown system error -116'), { errno: -116 });
        return fs.promises.readdir(dir);
      },
      stat: (file) => fs.promises.stat(file),
      readFile: async (file) => {
        if (fail === 'ESTALE')
          throw Object.assign(new Error('Unknown system error -116'), { errno: -116 });
        if (fail === 'ENOENT') throw Object.assign(new Error('gone'), { code: 'ENOENT' });
        return fs.promises.readFile(file);
      },
    };
    let now = NOW;
    const r = reader(root, () => now, fsOverride);
    await r.scan();
    let seq = 5;
    for (const kind of ['ESTALE', 'ENOENT', 'ESTALE-dir']) {
      writeQuota(root, report({ seq: seq + 1, method: `During ${kind}` }));
      fail = kind;
      await r.scan();
      expect(r.snapshot().good?.doc.seq).toBe(seq);
      expect(r.snapshot().problem).toBeNull();
      expect(quotaSource(r.snapshot(), now, 'UTC').state).toBe('ok');
      fail = null;
      await r.scan();
      seq += 1;
      expect(r.snapshot().good?.doc.seq).toBe(seq);
      expect(r.snapshot().good?.doc.method).toBe(`During ${kind}`);
    }
    fail = 'EACCES';
    await r.scan();
    expect(quotaSource(r.snapshot(), now, 'UTC')).toMatchObject({
      state: 'error',
      detail: expect.stringContaining('The quota folder could not be listed.'),
    });
    fail = null;
    // A file gone from the listing keeps its last good copy through the rename gap, then goes.
    fs.rmSync(path.join(root, 'v1', 'quota', 'report.json'));
    await r.scan();
    expect(r.snapshot().good).not.toBeNull();
    now += 3 * MIN;
    await r.scan();
    expect(r.snapshot().good).toBeNull();
    expect(quotaSource(r.snapshot(), now, 'UTC').state).toBe('missing');
  });

  it('ignores a lower seq and takes a higher one', async () => {
    const root = dirs.make();
    const r = reader(root);
    writeQuota(root, report({ seq: 7, method: 'Seven' }));
    await r.scan();
    writeQuota(root, report({ seq: 6, method: 'Six (late)' }));
    await r.scan();
    expect(r.snapshot().good?.doc.method).toBe('Seven');
    expect(r.snapshot().problem).toBeNull();
    writeQuota(root, report({ seq: 8, method: 'Eight' }));
    await r.scan();
    expect(r.snapshot().good?.doc.method).toBe('Eight');
  });
});

describe('quota-watch report: the spend view', () => {
  async function spend(root: string, labels = new Map([['S', 'Stream S']])) {
    const r = reader(root);
    await r.scan();
    return buildSpend(r.snapshot(), labels);
  }

  it('keeps units apart, rounds per unit, labels estimates, and makes no total', async () => {
    const root = dirs.make();
    writeQuota(root, report());
    const view = await spend(root);
    expect(view.estimate).toBe(true);
    expect(view.method).toBe(
      'Each meter is split by each session’s share of tokens at list price.'
    );
    expect(view.periods.map((p) => [p.id, p.label, p.coverageText])).toEqual([
      ['day', 'Today', null],
      ['week', 'This week', 'covers 9% of the week'],
      ['window', 'Last 6 hours', null],
    ]);
    const day = view.periods[0];
    expect(day.streams.map((s) => [s.stream, s.label])).toEqual([
      ['OTHER', 'OTHER'],
      ['S', 'Stream S'],
      ['(no stream)', '(no stream)'],
    ]);
    const s = day.streams.find((x) => x.stream === 'S');
    expect(s?.rows).toEqual([
      {
        account: 'codex:account-a',
        meter: 'credits_balance',
        meterLabel: 'Credits',
        unit: 'credits',
        value: 17.77,
        valueText: '≈ 17.77 credits',
        estimate: true,
      },
      {
        account: 'claude:account-b',
        meter: 'seven_day',
        meterLabel: '7-day window',
        unit: 'percent',
        value: 4.7,
        valueText: '≈ 4.7 points of the 7-day window',
        estimate: true,
      },
    ]);
    expect(JSON.stringify(view)).not.toMatch(/"total"/);
    expect(day.streams.find((x) => x.stream === 'OTHER')?.rows[0].valueText).toBe(
      '≈ under 0.01 credits'
    );
  });

  it('keeps nulls null and never shows 0 for an unknown', async () => {
    const root = dirs.make();
    writeQuota(root, report());
    const view = await spend(root);
    const unknown = view.periods[0].streams.find((x) => x.stream === '(no stream)')?.rows[0];
    expect(unknown).toMatchObject({ value: null, valueText: 'not read' });
    const [credits, weekly, nothing] = view.budgets;
    expect(credits).toMatchObject({
      limit: null,
      use: 17.77,
      percent: null,
      overLimit: false,
      text: '≈ 17.77 credits today; no limit set.',
    });
    expect(weekly).toMatchObject({
      limit: 10,
      use: 10.4,
      known: false,
      percent: 104,
      overLimit: true,
      coverageText: 'covers 9% of the week',
      text: 'at least ≈ 10.4 of 10.0 points of the 7-day window this week (104%).',
    });
    expect(nothing).toMatchObject({
      limit: 5,
      use: null,
      percent: null,
      overLimit: false,
      text: 'No reading yet.',
      coverageText: 'coverage of the day not known',
    });
    expect(view.unattributed.map((u) => [u.reason, u.amount, u.amountText, u.estimate])).toEqual([
      ['(no stream)', 470.53, '≈ 470.53 credits', true],
      ['(no reading)', null, 'not read', true],
      ['(no session seen)', 2.5, '≈ 2.5 points of the 5-hour window', false],
    ]);
    expect(view.machinesNotScanned).toEqual([
      { name: 'mac', thisPass: true, error: 'ssh timed out', runsMissed: 2, runsInWindow: 36 },
    ]);
    expect(view.overLimitBudgets).toBe(1);
    expect(meterLabel('seven_day_opus')).toBe('7-day Opus window');
    expect(meterLabel('some_new_meter')).toBe('some new meter');
    expect(amountText(null, 'credits', 'credits_balance')).toBe('not read');
  });

  it('adds the budget warning to the headline only from a current report', async () => {
    const root = dirs.make();
    writeReport(
      root,
      'vm.lead',
      'tasks.json',
      tasksDoc('vm.lead', [item('A', 'todo')], { stream: 'S', streamLabel: 'Stream S' })
    );
    writeQuota(root, report());
    const reports = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reports.scan();
    const empty = { data: null, asOf: null, error: null };
    const inputs = async (now: number) => {
      const q = reader(root, () => NOW);
      await q.scan();
      return buildStatusView({
        now,
        timeZone: 'UTC',
        reports: reports.snapshot(),
        registry: empty,
        machines: { vm: empty, mac: empty, windows: empty, nas1: empty },
        browsers: empty,
        simlock: empty,
        usage: null,
        answers: [],
        quota: q.snapshot(),
      });
    };
    const view = await inputs(NOW);
    expect(view.spend.source).toBe('quota-watch');
    expect(view.spend.budgets[1].streamLabel).toBe('Stream S');
    expect(view.headline.runs).toContainEqual({
      text: '1 budget at or over its limit (estimate)',
      tone: 'bad',
    });
    expect(view.headline.tone).toBe('bad');
    expect(view.sources.find((s) => s.id === 'quota-watch')?.state).toBe('ok');
    const later = await inputs(NOW + 40 * MIN);
    expect(later.sources.find((s) => s.id === 'quota-watch')?.state).toBe('stale');
    expect(later.headline.runs.some((r) => r.text.includes('budget'))).toBe(false);
  });

  it('labels a stream two projects name differently by its id', async () => {
    const root = dirs.make();
    writeReport(
      root,
      'vm.a',
      'tasks.json',
      tasksDoc('vm.a', [], { project: 'p1', stream: 'S', streamLabel: 'One' })
    );
    writeReport(
      root,
      'vm.b',
      'tasks.json',
      tasksDoc('vm.b', [], { project: 'p2', stream: 'S', streamLabel: 'Two' })
    );
    writeQuota(root, report());
    const reports = new AgentReportsReader({ root: () => root, now: () => NOW });
    await reports.scan();
    const q = reader(root);
    await q.scan();
    const empty = { data: null, asOf: null, error: null };
    const view = buildStatusView({
      now: NOW,
      timeZone: 'UTC',
      reports: reports.snapshot(),
      registry: empty,
      machines: { vm: empty, mac: empty, windows: empty, nas1: empty },
      browsers: empty,
      simlock: empty,
      usage: null,
      answers: [],
      quota: q.snapshot(),
    });
    expect(view.spend.periods[0].streams.find((s) => s.stream === 'S')?.label).toBe('S');
  });
});
