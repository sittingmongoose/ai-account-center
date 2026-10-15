import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

test('Slint interaction fixes preserve popup-local detail and bounded row animation', () => {
  const source = name => readFileSync(new URL(`../ui/pages/analytics/${name}.slint`, import.meta.url), 'utf8');
  assert.match(source('ax-head'), /IncludedTip[\s\S]*root.detail = t/);
  assert.match(source('ax-head'), /root.hovered\(self.has-hover \? root.day.n : -1\)/);
  assert.match(source('ax-common'), /Hover.leave\(root.issued-owner\)/);
  assert.match(source('ax-common'), /root.ind-value == root.value/);
  assert.match(source('ax-time'), /layout\("heat-cell", self.cw, root.ch\)/);
  assert.doesNotMatch(source('ax-time'), /mouse-y > self.pressed-y - 8px/);
  assert.match(source('ax-time'), /if root.sb.total != "": AxRoVal/);
  assert.match(source('ax-time'), /root.sb.foot/);
  assert.match(source('ax-stats'), /50-session sample/);
  assert.match(source('ax-stats'), /if root.r.tool == "": Text \{ text: "Other"/);
  assert.match(source('ax-stats'), /if t.tok-share != "": ValueUnit/);
  assert.equal((source('ax-models').match(/root.m.idx \* min\(70ms, 800ms/g) || []).length, 4);
  assert.match(source('ax-quota'), /padding-left: Breakpoints.tier-d \? 48px : 20px/);
  assert.match(source('ax-trend'), /visible: k != "cr" \|\| AxData.cache-on/);
  const rust = readFileSync(new URL('../src/analytics.rs', import.meta.url), 'utf8');
  assert.match(rust, /model_types: Nested<AxModelType>/);
  assert.match(rust, /m.model_types.retain\(&live_models\)/);
});
