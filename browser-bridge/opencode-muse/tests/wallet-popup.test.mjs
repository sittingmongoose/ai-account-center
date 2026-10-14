import test from 'node:test';
import assert from 'node:assert/strict';
import {GO_RATE_LIMIT_MESSAGE} from '../extension/bridge-core.mjs';

const sample = {status: 'ok', fetchedAt: '2026-10-01T13:00:00Z', goUsageStatus: 'rate_limited', windows: [
  {label: 'Zen balance', kind: 'balance', remaining: -1.25, resetAt: null, expiresAt: null},
]};

for (const trigger of ['saved', 'manual', 'automatic']) {
  test(`wallet popup keeps a synced balance visible with its fixed optional Go warning after ${trigger} sync`, async t => {
    const elements = new Map();
    const element = () => ({children: [], handlers: {}, hidden: true,
      replaceChildren() { this.children = []; },
      append(child) { this.children.push(child); },
      addEventListener(event, handler) { this.handlers[event] = handler; },
    });
    const store = trigger === 'saved' ? {lastSample: sample} : trigger === 'automatic'
      ? {lastSample: {...sample, goUsageStatus: undefined, windows: [
        {label: 'Weekly', kind: 'rate_limit', usedPercent: 20, resetAt: null}, ...sample.windows,
      ]}} : {};
    globalThis.document = {
      createElement: element,
      getElementById(id) {
        if (!elements.has(id)) elements.set(id, element());
        return elements.get(id);
      },
    };
    let onStorageChanged;
    globalThis.chrome = {
      storage: {local: {get: async () => store}, onChanged: {addListener(handler) { onStorageChanged = handler; }}},
      tabs: {query: async () => [{url: 'https://opencode.ai/console/org_FIXTURE/billing'}]},
      runtime: {sendMessage: async () => ({ok: true, sample})},
    };
    t.after(() => { delete globalThis.document; delete globalThis.chrome; });
    await import(`../extension/popup.mjs?wallet-trigger=${trigger}`);
    if (trigger === 'manual') {
      await elements.get('sync').handlers.click();
      assert.equal(elements.get('sync').disabled, false);
    } else if (trigger === 'automatic') {
      assert.equal(elements.get('sample').children.length, 3);
      onStorageChanged({lastSample: {newValue: sample}, lastError: {newValue: null}}, 'local');
    }
    assert.equal(elements.get('status').textContent, GO_RATE_LIMIT_MESSAGE);
    const display = elements.get('sample');
    assert.equal(display.children.length, 2);
    assert.match(display.children[0].children[1].textContent, /1\.25/);
  });
}
