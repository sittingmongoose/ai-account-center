import { describe, expect, it } from 'bun:test';
import {
  findSecret,
  isCardId,
  isWriterName,
  parseReportFile,
} from '../../../../src/web-server/status/report-schema';
import { cardDoc, item, tasksDoc, threadDoc } from './status-test-helpers';

const json = (value: unknown) => Buffer.from(JSON.stringify(value));

describe('agent report documents', () => {
  it('accepts a valid thread, tasks and card file and keeps only known fields', () => {
    const thread = parseReportFile(
      'thread',
      json({ ...threadDoc('vm.lead'), extra: 'ignored' }),
      'vm.lead'
    );
    expect(thread.ok).toBe(true);
    if (thread.ok) {
      expect(thread.value.doc.state).toBe('working');
      expect((thread.value.doc as unknown as Record<string, unknown>).extra).toBeUndefined();
      expect(thread.value.sha256).toMatch(/^[0-9a-f]{64}$/);
    }
    const tasks = parseReportFile(
      'tasks',
      json(tasksDoc('vm.lead', [item('A-1', 'landed'), item('A-2', 'todo')])),
      'vm.lead'
    );
    expect(tasks.ok && tasks.value.doc.items.length).toBe(2);
    const card = parseReportFile('card', json(cardDoc('vm.lead', 'Q1')), 'vm.lead', 'vm.lead--Q1');
    expect(card.ok && card.value.doc.recommendation?.key).toBe('A');
  });

  it('names malformed JSON, NUL bytes, empty files and oversized files', () => {
    expect(parseReportFile('thread', Buffer.from('{"schema":'), 'vm.lead')).toEqual({
      ok: false,
      reason: 'not valid JSON',
    });
    const nul = Buffer.concat([json(threadDoc('vm.lead')), Buffer.alloc(8)]);
    expect(parseReportFile('thread', nul, 'vm.lead')).toEqual({
      ok: false,
      reason: 'contains NUL bytes from a damaged write',
    });
    expect(parseReportFile('thread', Buffer.alloc(0), 'vm.lead')).toEqual({
      ok: false,
      reason: 'empty',
    });
    const big = json({ ...threadDoc('vm.lead'), doing: 'x'.repeat(17 * 1024) });
    expect(parseReportFile('thread', big, 'vm.lead')).toEqual({
      ok: false,
      reason: 'larger than the 16 KB cap',
    });
  });


  it('accepts ttlSeconds null for blocked and every other state (B2)', () => {
    // A thread that ended its turn to wait on the owner cannot heartbeat, whatever state it
    // ended in; refusing the null hid the blocker once the heartbeat lapsed.
    for (const state of ['blocked', 'waiting', 'working', 'paused', 'idle', 'done']) {
      const parsed = parseReportFile(
        'thread',
        json(threadDoc('vm.lead', { state, ttlSeconds: null })),
        'vm.lead'
      );
      expect(parsed.ok).toBe(true);
      if (parsed.ok) expect(parsed.value.doc.ttlSeconds).toBeNull();
    }
  });

  it('refuses a file whose writer or card id does not match its folder', () => {
    expect(parseReportFile('thread', json(threadDoc('vm.other')), 'vm.lead')).toEqual({
      ok: false,
      reason: 'writer does not match its folder',
    });
    const card = parseReportFile('card', json(cardDoc('vm.lead', 'Q1')), 'vm.lead', 'vm.lead--Q2');
    expect(card).toEqual({ ok: false, reason: 'id does not match its file name and writer' });
  });

  it('refuses schema misses with the field name only', () => {
    const bad = parseReportFile(
      'thread',
      json({ ...threadDoc('vm.lead'), state: 'sleeping' }),
      'vm.lead'
    );
    expect(bad).toEqual({ ok: false, reason: 'state is missing or not valid' });
    const rec = parseReportFile(
      'card',
      json(cardDoc('vm.lead', 'Q1', { recommendation: { key: 'Z' } })),
      'vm.lead',
      'vm.lead--Q1'
    );
    expect(rec).toEqual({ ok: false, reason: 'recommendation.key is missing or not valid' });
    const dup = parseReportFile(
      'tasks',
      json(tasksDoc('vm.lead', [item('A', 'todo'), item('A', 'done')])),
      'vm.lead'
    );
    expect(dup).toEqual({ ok: false, reason: 'items repeat an id' });
  });

  it('reads the deck card fields: label, now (context alias), old answer, gets/costs, media, replies', () => {
    const parse = (fields: Record<string, unknown>) =>
      parseReportFile('card', json(cardDoc('vm.lead', 'Q1', fields)), 'vm.lead', 'vm.lead--Q1');
    const full = parse({
      oldAnswer: { text: 'Keep it', at: '2026-11-01T10:00:00Z' },
      whatChanged: 'It shipped',
      options: [
        {
          key: 'A',
          label: 'First',
          gets: 'g',
          costs: 'c',
          media: [
            { kind: 'image', path: 'images/a.png', caption: 'A', label: 'mockup', poster: null },
          ],
        },
        { key: 'B', label: 'Second', gets: null, costs: null },
      ],
      replies: [{ at: '2026-11-06T16:00:00Z', text: 'Done' }],
      allowNote: false,
    });
    expect(full.ok).toBe(true);
    if (full.ok) {
      expect(full.value.doc.label).toBe('S-Q1');
      expect(full.value.doc.oldAnswer?.text).toBe('Keep it');
      expect(full.value.doc.options[0].media).toEqual([
        { kind: 'image', path: 'images/a.png', caption: 'A', label: 'mockup', poster: null },
      ]);
      expect(full.value.doc.options[1].media).toEqual([]);
      expect(full.value.doc.replies).toHaveLength(1);
      expect('allowNote' in full.value.doc).toBe(false);
    }
    const alias = parse({ now: undefined, context: 'Old field', label: undefined });
    expect(alias.ok && alias.value.doc.now).toBe('Old field');
    expect(alias.ok && alias.value.doc.label).toBe('Q1');
    const option = (key: string) => ({ key, label: key });
    expect(parse({ options: [option('A')], recommendation: null })).toEqual({
      ok: false,
      reason: 'a card needs 2 to 4 options',
    });
    expect(parse({ options: ['A', 'B', 'C', 'D', 'E'].map(option), recommendation: null }).ok).toBe(
      false
    );
    for (const bad of [
      '../x.png',
      '/etc/x.png',
      'a/../../x.png',
      'C:\\x.png',
      'a//b.png',
      '.hidden/x.png',
    ])
      expect(
        parse({
          options: [{ ...option('A'), media: [{ kind: 'image', path: bad }] }, option('B')],
          recommendation: null,
        })
      ).toEqual({ ok: false, reason: 'options[0].media[0].path is missing or not valid' });
  });

  it('reads card sections strictly, passes unknown section types through, and keeps blocks as a string', () => {
    const parse = (fields: Record<string, unknown>) =>
      parseReportFile('card', json(cardDoc('vm.lead', 'Q1', fields)), 'vm.lead', 'vm.lead--Q1');
    const sections = [
      { type: 'text', title: null, body: 'One.\n\nTwo.' },
      { type: 'list', title: 'Steps', fold: true, ordered: true, items: ['a', 'b'] },
      { type: 'facts', rows: [{ label: 'Size', value: '2 MB' }] },
      { type: 'table', columns: ['A', 'B'], rows: [['1', '']] },
      {
        type: 'compare',
        items: [
          {
            name: 'X',
            does: 'd',
            problem: null,
            link: { label: 'X', url: 'https://x.test/a' },
            version: '2',
          },
        ],
      },
      { type: 'notCopying', items: [{ what: 'w', why: 'y' }] },
      { type: 'unknowns', items: ['u'] },
      { type: 'terms', items: [{ term: 't', meaning: 'm' }] },
      { type: 'callout', tone: 'crit', body: 'Careful' },
      { type: 'links', items: [{ label: 'Board', url: '/mnt/Cursor/share/x.md' }] },
      {
        type: 'media',
        items: [
          {
            kind: 'video',
            path: 'cards/vm.lead--Q1.assets/a.mp4',
            caption: null,
            label: null,
            poster: 'cards/vm.lead--Q1.assets/a.png',
          },
        ],
      },
      { type: 'timeline', title: 'Later', fold: true, entries: [1, 2, 3] },
    ];
    const ok = parse({
      blocks: 'FO-33 landing',
      sections,
      options: [
        { key: 'A', label: 'A', sections: [{ type: 'text', body: 'Option detail' }] },
        { key: 'B', label: 'B' },
      ],
      recommendation: { key: 'A', why: 'w', confidence: 'high', flip: 'If B gets cheaper' },
    });
    expect(ok.ok).toBe(true);
    if (!ok.ok) return;
    const doc = ok.value.doc;
    expect(doc.blocks).toBe('FO-33 landing');
    expect(doc.sections.map((s) => s.type)).toEqual(sections.map((s) => s.type));
    expect(doc.sections[0]).toEqual({
      type: 'text',
      title: null,
      fold: false,
      body: 'One.\n\nTwo.',
    });
    expect(doc.sections[11]).toEqual({
      type: 'timeline',
      title: 'Later',
      fold: true,
      unknown: true,
    });
    expect(doc.options[0].sections).toEqual([
      { type: 'text', title: null, fold: false, body: 'Option detail' },
    ]);
    expect(doc.recommendation).toEqual({
      key: 'A',
      why: 'w',
      confidence: 'high',
      flip: 'If B gets cheaper',
    });
    const refused = (section: unknown) => parse({ sections: [section] });
    expect(refused({ type: 'callout', tone: 'loud', body: 'x' })).toEqual({
      ok: false,
      reason: 'sections[0].tone is missing or not valid',
    });
    expect(refused({ type: 'links', items: [{ label: 'x', url: 'javascript:alert(1)' }] })).toEqual(
      {
        ok: false,
        reason: 'sections[0].items[0].url is missing or not valid',
      }
    );
    expect(refused({ type: 'table', columns: ['A', 'B'], rows: [['only one']] })).toEqual({
      ok: false,
      reason: 'sections[0].rows[0] is missing or not valid',
    });
    expect(refused({ type: 'media', items: [{ kind: 'audio', path: 'a.mp3' }] })).toEqual({
      ok: false,
      reason: 'sections[0].items[0].kind is missing or not valid',
    });
    expect(refused({ type: '<b>', title: null }).ok).toBe(false);
    expect(parse({ recommendation: { key: 'A', confidence: 'sure' } })).toEqual({
      ok: false,
      reason: 'recommendation.confidence is missing or not valid',
    });
    expect(parse({ sections: Array(31).fill({ type: 'text', body: 'x' }) }).ok).toBe(false);
  });

  it('reads card kinds and the needed-by, exception and reversible fields', () => {
    const parse = (fields: Record<string, unknown>) =>
      parseReportFile('card', json(cardDoc('vm.lead', 'Q1', fields)), 'vm.lead', 'vm.lead--Q1');
    const question = parse({
      neededBy: { at: '2026-11-07T12:00:00Z', ifNoAnswer: 'The landing waits.' },
      exception: { rule: 'No branch installs', text: 'Install this branch once on the VM.' },
      reversible: { yes: true, how: 'Reinstall main.' },
    });
    expect(question.ok && question.value.doc.kind).toBe('question');
    expect(question.ok && question.value.doc.neededBy?.ifNoAnswer).toBe('The landing waits.');
    expect(question.ok && question.value.doc.exception?.rule).toBe('No branch installs');
    expect(question.ok && question.value.doc.reversible).toEqual({
      yes: true,
      how: 'Reinstall main.',
    });
    expect(parse({ kind: 'objection', options: [], recommendation: null }).ok).toBe(true);
    expect(parse({ kind: 'action', options: [], recommendation: null }).ok).toBe(true);
    expect(parse({ kind: 'action' })).toEqual({
      ok: false,
      reason: 'an action card has no options',
    });
    expect(parse({ kind: 'question', options: [], recommendation: null })).toEqual({
      ok: false,
      reason: 'a card needs 2 to 4 options',
    });
    expect(parse({ kind: 'poll' })).toEqual({ ok: false, reason: 'kind is missing or not valid' });
    expect(parse({ reversible: { yes: 'maybe' } })).toEqual({
      ok: false,
      reason: 'reversible.yes is missing or not valid',
    });
  });

  it('refuses files with secrets and says what kind, never the text', () => {
    const cases: Array<[unknown, string]> = [
      [{ doing: 'key sk-ant-abcdefghijklmnopqrstuvwxyz0123' }, 'an API key'],
      [{ doing: 'mail someone@example.com' }, 'an email address'],
      [{ doing: 'see ghp_abcdefghijklmnopqrstuvwxyz0123456789' }, 'a GitHub token'],
      [{ doing: 'https://x.test/cb?access_token=abcdefghijkl' }, 'a credential in a URL'],
      [{ password: 'hunter2hunter2' }, 'a credential field'],
      [{ doing: '-----BEGIN PRIVATE KEY-----' }, 'a private key'],
    ];
    for (const [fields, kind] of cases) {
      const result = parseReportFile(
        'thread',
        json({ ...threadDoc('vm.lead'), ...(fields as object) }),
        'vm.lead'
      );
      expect(result).toEqual({ ok: false, reason: `refused: it contains what looks like ${kind}` });
    }
    expect(findSecret('/mnt/Cursor/share/x 0cbf3355 thread 8ba2e5d2')).toBeNull();
  });

  it('validates writer names and card ids', () => {
    expect(isWriterName('vm.tools-lead')).toBe(true);
    expect(isWriterName('laptop.tools-lead')).toBe(false);
    expect(isWriterName('vm.Tools')).toBe(false);
    expect(isCardId('vm.pm-fo-lead--Q3')).toBe(true);
    expect(isCardId('vm.pm-fo-lead-Q3')).toBe(false);
    expect(isCardId('../x--Q3')).toBe(false);
  });
});
