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

  it('reads the deck card fields: label, now (context alias), old answer, gets/costs, images, replies', () => {
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
          image: { path: 'images/a.png', kind: 'mockup' },
        },
        { key: 'B', label: 'Second', gets: null, costs: null, image: null },
      ],
      replies: [{ at: '2026-11-06T16:00:00Z', text: 'Done' }],
      allowNote: false,
    });
    expect(full.ok).toBe(true);
    if (full.ok) {
      expect(full.value.doc.label).toBe('S-Q1');
      expect(full.value.doc.oldAnswer?.text).toBe('Keep it');
      expect(full.value.doc.options[0].image).toEqual({ path: 'images/a.png', kind: 'mockup' });
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
          options: [{ ...option('A'), image: { path: bad, kind: 'built' } }, option('B')],
          recommendation: null,
        })
      ).toEqual({ ok: false, reason: 'options[0].image.path is missing or not valid' });
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
