import { describe, expect, it } from 'bun:test';
import { readFileSync, readdirSync } from 'node:fs';
import { parseReportFile } from '../../../../src/web-server/status/report-schema';

const refused = new URL('../../../../docs/agent-reports/fixtures/refused/', import.meta.url);

describe('published card section refusal fixtures', () => {
  for (const file of readdirSync(refused)
    .filter((name) => name.endsWith('.json'))
    .sort()) {
    it(file, () => {
      const { document, reason } = JSON.parse(readFileSync(new URL(file, refused), 'utf8'));
      const result = parseReportFile(
        'card',
        Buffer.from(JSON.stringify(document)),
        document.writer,
        document.id
      );
      expect(result).toEqual({ ok: false, reason });
    });
  }
});

it('accepts quotes, plan changes, and future section types', () => {
  const { document } = JSON.parse(readFileSync(new URL('card-media-label.json', refused), 'utf8'));
  document.sections = [
    { type: 'quote', items: [{ text: 'Run the check.', context: ['Before the landing.'] }] },
    { type: 'planChange', doc: 'Plan', unit: 'Rollout', now: 'One test.', proposed: 'Two tests.' },
    { type: 'future-content', title: 'Future display', payload: {} },
  ];
  const result = parseReportFile(
    'card',
    Buffer.from(JSON.stringify(document)),
    document.writer,
    document.id
  );
  expect(result).toMatchObject({
    ok: true,
    value: {
      doc: {
        sections: [
          { type: 'quote', items: [{ text: 'Run the check.', context: ['Before the landing.'] }] },
          { type: 'planChange', unit: 'Rollout', proposed: 'Two tests.' },
          { type: 'future-content', title: 'Future display', unknown: true },
        ],
      },
    },
  });
});
