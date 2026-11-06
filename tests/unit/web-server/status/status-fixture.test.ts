import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { verifySignedAnswer } from '../../../../src/web-server/status/answer-signing';
import { openCardFile } from '../../../../src/web-server/status/status-files';
import { StatusAnswers } from '../../../../src/web-server/status/answers';
import { PmMailRegistryReader } from '../../../../src/web-server/status/status-collectors';
import { buildStatusView } from '../../../../src/web-server/status/status-view';
import { tempDirs } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

// The fixture writer the page and phone tests use (scripts/status-fixture.mjs).
const { writeStatusFixture } = (await import(
  path.resolve(import.meta.dir, '../../../../scripts/status-fixture.mjs')
)) as {
  writeStatusFixture: (
    root: string,
    options: { ccsDir?: string | null; withProblems?: boolean; withKinds?: boolean; now?: number }
  ) => { root: string; storeDir: string | null; cards: string[]; answered: string[] };
};

describe('status fixture', () => {
  it('writes the agreed shape and every file passes the reader and the signature check', async () => {
    const root = path.join(dirs.make(), 'agent-reports');
    const ccsDir = path.join(dirs.make(), '.ccs');
    const now = Date.now();
    writeStatusFixture(root, { ccsDir, now });
    const reader = new AgentReportsReader({ root: () => root, now: () => now });
    await reader.scan();
    const reports = reader.snapshot();
    expect(reports.problems).toEqual([]);
    expect(reports.threads).toHaveLength(8);
    const answers = new StatusAnswers({
      readCard: (id) => reader.readCardNow(id),
      storeDir: () => path.join(ccsDir, 'status'),
      reportsRoot: () => root,
    });
    const registry = await new PmMailRegistryReader(() =>
      path.join(root, 'pm-mail-registry')
    ).scan();
    const empty = { data: null, asOf: null, error: null };
    const view = buildStatusView({
      now,
      timeZone: 'America/New_York',
      reports,
      registry: { data: registry.threads, asOf: now, error: null },
      machines: { vm: empty, mac: empty, windows: empty, nas1: empty },
      browsers: empty,
      simlock: empty,
      usage: null,
      answers: answers.list(),
    });
    expect(view.projects.map((p) => p.id)).toEqual(['ai-account-center', 'puppet-master']);
    const streams = view.projects.flatMap((p) => p.streams);
    expect(streams.filter((s) => s.source === 'tasks')).toHaveLength(4);
    expect(streams.filter((s) => s.source === 'board-only')).toHaveLength(1);
    expect(view.cards.open).toHaveLength(5);
    expect(view.cards.answered).toHaveLength(2);
    expect(view.cards.batches).toHaveLength(1);
    expect(view.cards.batches[0].cardIds).toHaveLength(3);
    expect(view.cards.open.filter((c) => c.recommendation)).toHaveLength(4);
    const noted = view.cards.open.find((c) => c.id === 'vm.aac-status-lead--Q2');
    expect(noted?.notes.map((n) => n.note)).toEqual(['Could you show both side by side first?']);
    expect(noted?.replies).toHaveLength(1);
    const q3 = view.cards.open.find((c) => c.id === 'vm.pm-fo-lead--Q3');
    expect(q3?.options.map((o) => o.media[0]?.url)).toEqual([
      '/api/status/files/vm.pm-fo-lead/images/fo33-inside.png',
      '/api/status/files/vm.pm-fo-lead/images/fo33-under-axis.png',
    ]);
    expect(q3?.recommendation?.confidence).toBe('medium');
    expect(q3?.sections.at(-1)).toEqual({
      type: 'timeline',
      title: 'A section type this page does not know',
      fold: true,
      unknown: true,
    });
    const image = await openCardFile(root, 'vm.pm-fo-lead', 'images/fo33-inside.png');
    expect(image.ok && image.contentType).toBe('image/png');
    expect(view.cards.answered.map((a) => a.answer.via).sort()).toEqual(['deck', 'recommended']);
    expect(view.threads.rows.filter((t) => t.source === 'report')).toHaveLength(8);
    expect(
      view.threads.rows
        .filter((t) => t.source === 'pm-mail')
        .map((t) => t.name)
        .sort()
    ).toEqual(['aac-mac-bar', 'pm-docs-sweep']);
    expect(new Set(view.threads.rows.map((t) => t.state))).toEqual(
      new Set(['working', 'waiting', 'blocked', 'paused', 'idle', 'done', null])
    );
    const pub = fs.readFileSync(path.join(root, 'v1', 'keys', 'aac-answers.pub'), 'utf8');
    for (const file of fs.readdirSync(path.join(root, 'v1', 'answers')))
      expect(
        verifySignedAnswer(fs.readFileSync(path.join(root, 'v1', 'answers', file)), pub).ok
      ).toBe(true);
    expect(() => writeStatusFixture(root, { ccsDir })).toThrow('already holds');
  });

  it('adds an objection card and an action card on request', async () => {
    const root = dirs.make();
    writeStatusFixture(root, { withKinds: true });
    const reader = new AgentReportsReader({ root: () => root });
    await reader.scan();
    expect(reader.snapshot().problems).toEqual([]);
    const kinds = reader
      .snapshot()
      .cards.map((c) => c.doc.kind)
      .sort();
    expect(kinds.filter((k) => k !== 'question')).toEqual(['action', 'objection']);
  });

  it('adds named problems on request', async () => {
    const root = dirs.make();
    writeStatusFixture(root, { withProblems: true });
    const reader = new AgentReportsReader({ root: () => root });
    await reader.scan();
    expect(
      reader
        .snapshot()
        .problems.map((p) => p.reason)
        .sort()
    ).toEqual(['contains NUL bytes from a damaged write', 'not valid JSON']);
  });
});
