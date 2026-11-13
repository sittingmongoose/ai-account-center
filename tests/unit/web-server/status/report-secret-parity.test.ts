import { expect, it } from 'bun:test';
import cases from '../../../../native/aac-store/tests/report-secret-cases.json';
import { findSecretIn } from '../../../../src/web-server/status/schema-helpers';

it.each(cases)('applies the shared native capture refusal fixture: $name', ({ doc, secret }) => {
  expect(findSecretIn('', doc)).toBe(secret);
  expect(findSecretIn(JSON.stringify(doc), doc) === null).toBe(secret === null);
});
