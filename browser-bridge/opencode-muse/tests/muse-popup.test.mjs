import test from 'node:test';
import assert from 'node:assert/strict';

const sample = {status: 'ok', sampledAt: '2026-10-01T13:00:00Z', windows: [
  {label: 'Current usage', usedPercent: 20, resetAt: null},
]};

for (const trigger of ['manual', 'automatic']) for (const changedPlan of [true, false]) {
  test(`an open popup ${changedPlan ? 'clears old plan limits' : 'keeps valid cached limits'} after a failed ${trigger} sync`, async t => {
    const elements = new Map();
    const element = () => ({children: [], handlers: {}, hidden: true,
      replaceChildren() { this.children = []; },
      append(child) { this.children.push(child); },
      addEventListener(event, handler) { this.handlers[event] = handler; },
    });
    const store = {lastMuseSample: sample, selectedMuseTeamId: '42'};
    globalThis.document = {
      createElement: element,
      getElementById(id) {
        if (!elements.has(id)) elements.set(id, element());
        return elements.get(id);
      },
    };
    let onStorageChanged;
    globalThis.chrome = {
      storage: {local: {get: async () => store}, onChanged: {addListener: handler => { onStorageChanged = handler; }}},
      runtime: {sendMessage: async () => {
        if (changedPlan) store.lastMuseSample = null;
        return {ok: false, code: 'rate_limited'};
      }},
    };
    t.after(() => { delete globalThis.document; delete globalThis.chrome; });
    await import(`../extension/popup.mjs?changedPlan=${changedPlan}&trigger=${trigger}`);
    const display = elements.get('muse-sample');
    assert.equal(display.children.length, 2);
    if (trigger === 'manual') {
      await elements.get('muse-sync').handlers.click();
      assert.equal(elements.get('muse-sync').disabled, false);
    } else {
      onStorageChanged({lastMuseError: {newValue: 'rate_limited'},
        ...(changedPlan ? {lastMuseSample: {newValue: null}} : {})}, 'local');
    }
    assert.equal(display.children.length, changedPlan ? 0 : 2);
    assert.match(elements.get('muse-status').textContent, /limiting/i);
  });
}
