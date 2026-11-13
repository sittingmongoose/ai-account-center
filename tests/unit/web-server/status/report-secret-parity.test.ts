import { expect, it } from 'bun:test';
import { secretPolicyCases as cases } from '../../../fixtures/secret-policy-cases';
import { findSecretIn } from '../../../../src/shared/secret-policy';

it.each(cases)('applies the shared native capture refusal fixture: $name', (fixture) => {
  const { doc, secret } = fixture;
  // One verdict and one reason for both languages: no case may carry a per-language exception.
  expect(Object.keys(fixture).sort()).toEqual(['doc', 'name', 'secret']);
  expect(findSecretIn('', doc)).toBe(secret);
  expect(findSecretIn(JSON.stringify(doc), doc) === null).toBe(secret === null);
});

it('exports an independent shared policy and preserves the Status compatibility API', async () => {
  const shared = await import('../../../../src/shared/secret-policy');
  const status = await import('../../../../src/web-server/status/schema-helpers');
  const policy = shared.loadSecretPolicy();
  expect(shared.SECRET_POLICY_PATH).toBe('src/shared/secret-policy.json');
  expect(policy.version).toBe(3);
  policy.patterns.splice(0);
  expect(shared.loadSecretPolicy().patterns.length).toBeGreaterThan(0);
  expect(shared.findSecret(`sk-${'A'.repeat(20)}`)).toBe('an API key');
  expect(shared.findSecret(`sk-${'A'.repeat(19)}`)).toBeNull();
  expect(status.findSecretIn).toBe(shared.findSecretIn);
});
