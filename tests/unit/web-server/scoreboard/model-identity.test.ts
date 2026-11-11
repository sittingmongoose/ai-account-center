import { expect, test } from 'bun:test';
import { normalizeModel } from '../../../../src/web-server/scoreboard/model-identity';
const models = [{ model: 'openai/gpt-6.1-sol' }, { model: 'google/gemini-4-argon', efforts: ['high'] }];
test('catalog resolves bare ids and effort suffixes without guessing unknowns', () => {
  expect(normalizeModel('gpt-6.1-sol', null, models)).toEqual({ id: 'openai/gpt-6.1-sol', effort: null });
  expect(normalizeModel('gemini-4-argon-high', null, models)).toEqual({ id: 'google/gemini-4-argon', effort: 'high' });
  expect(normalizeModel('future-model', null, models)).toEqual({ id: 'future-model', effort: null });
});
test('all catalog providers resolve without a reported provider', async () => {
  const { models } = await import('../../../../docs/agent-reports/catalog/models.json');
  for (const id of ['gpt-6.1-sol', 'claude-opus-5-5', 'muse-ember-1.0', 'glm-5.3', 'grok-4.7']) {
    const model = models.find(model => model.model.endsWith(`/${id}`));
    expect(model).toBeDefined();
    expect(normalizeModel(id, null, models).id).toBe(model!.model);
  }
  expect(normalizeModel('gemini-4-argon-high', 'max', models)).toEqual({ id: 'google/gemini-4-argon', effort: 'max' });
});
