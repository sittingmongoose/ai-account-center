// Source contracts for changes whose runtime build is owned by the parent integration task.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
const read = path => readFileSync(new URL(`../ui/${path}`, import.meta.url), 'utf8');

test('server-backed settings retain an independent model input and resync after save settlement', () => {
  for (const path of ['components/segmented.slint', 'components/select.slint', 'pages/accounts/ac-actions.slint']) {
    const source = read(path);
    assert.match(source, /in property <bool> controlled: false/);
    assert.match(source, /changed model-value =>/);
    assert.match(source, /changed enabled => \{ if root.controlled \{ root.value = root.model-value/);
    assert.match(source, /root.value = (item.value|v|t|option)/);
  }
  const source = read('pages/accounts/ac-settings.slint');
  assert.match(source, /in property <bool> model-checked/);
  assert.match(source, /changed enabled => \{ if root.controlled \{ root.checked = root.model-checked/);
  assert.match(source, /model-checked: AcData.cleanup-auto/);
  assert.match(source, /"cleanup-auto", checked \? "true" : "false"/);
  assert.doesNotMatch(source, /theme-controlled:/);
});
test('a segmented row clears its indicator when the selected value belongs to another row', () => {
  const source = read('pages/accounts/ac-actions.slint');
  assert.match(source, /visible: root.selected-index >= 0/);
  assert.match(source, /else if root.selected-index == i \{ root.selected-index = -1/);
});
test('JSONL fields retain drafts and clear only from the successful-add nonce', () => {
  const source = read('pages/accounts/ac-settings.slint');
  for (const field of ['ts', 'model', 'in', 'out', 'cost']) assert.ok(source.includes(`text <=> root.map-${field}-text;`));
  assert.doesNotMatch(source, /mapping-json/);
  assert.match(source, /draft-nonce: AcData.log-sources-nonce/);
  assert.match(source, /changed draft-nonce =>/);
});
test('slider cancellation discards the preview and Settings navigation uses a measured clamped anchor', () => {
  assert.match(read('pages/accounts/ac-settings.slint'), /e.kind == PointerEventKind.cancel \{\s*root.dragging = false;\s*root.pending = 0;/);
  assert.match(read('pages/accounts.slint'), /settings-anchor.absolute-position.y - root.absolute-position.y/);
  assert.match(read('dashboard.slint'), /settings-jump\(anchor\) => \{\s*scroller.content-y = clamp/);
});
