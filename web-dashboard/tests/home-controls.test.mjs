// Source contracts for Home controls; the parent build verifies the Slint 1.18.1 runtime.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const home = readFileSync(new URL('../ui/pages/home.slint', import.meta.url), 'utf8');
const actions = readFileSync(new URL('../ui/pages/accounts/ac-actions.slint', import.meta.url), 'utf8');

function blocks(source, opening) {
  const found = [];
  for (const match of source.matchAll(opening)) {
    const start = match.index;
    let depth = 1, end = start + match[0].length;
    for (; end < source.length && depth > 0; end++) {
      if (source[end] === '{') depth++;
      if (source[end] === '}') depth--;
    }
    found.push(source.slice(start, end));
  }
  return found;
}

test('both Home auto-switch layouts reassert model state after a successful or failed save', () => {
  const toggles = blocks(home, /\bBoundToggle \{/g);
  assert.equal(toggles.length, 2);
  for (const toggle of toggles) {
    assert.match(toggle, /model-on: root\.section\.auto\.enabled;/);
    assert.match(toggle, /enabled: .*root\.section\.auto\.available.*!root\.busy;/);
    assert.match(toggle, /toggled\(on\) => \{ root\.auto-toggled\(root\.section\.id, on\); \}/);
  }
  const bound = blocks(actions, /export component BoundToggle inherits VerticalLayout \{/g)[0];
  assert.match(bound, /changed model-on => \{ tg\.on = self\.model-on; \}/);
  assert.match(bound, /changed enabled => \{ tg\.on = self\.model-on; \}/);
  assert.doesNotMatch(home, /\bToggle \{/);
});

test('disabled Home Open and Activate controls consume a reason tap without opening Details', () => {
  const open = blocks(home, /component OpenControl inherits TipArea \{/g)[0];
  assert.match(open, /if !root\.can-open && !root\.busy \{ root\.reason\(root\.tip\); \}/);
  assert.match(open, /enabled: root\.can-open && !root\.busy;/);
  assert.match(open, /long-tip: root\.tip;/);
  assert.doesNotMatch(open, /open-details/);
  const slot = blocks(home, /export component SwitchSlot inherits Rectangle \{/g)[0];
  assert.match(slot, /TipArea \{/);
  assert.match(slot, /tip: root\.account\.activate-hint;/);
  assert.match(slot, /if root\.show-button && !root\.account\.can-activate && !root\.busy/);
  assert.match(slot, /root\.reason\(root\.account\.activate-hint\);/);
  assert.match(slot, /enabled: root\.show-button && root\.account\.can-activate && !root\.busy;/);
  assert.doesNotMatch(slot, /open-details/);
  const row = blocks(home, /export component AccountRow inherits Rectangle \{/g)[0];
  const opens = blocks(row, /\bOpenControl \{/g);
  assert.equal(opens.length, 4);
  for (const control of opens) assert.match(control, /reason\(r\) => \{ root\.tap-reason = r; \}/);
  const switches = blocks(row, /\bSwitchSlot \{/g).filter(control => !control.includes('visible: false;'));
  assert.equal(switches.length, 2);
  for (const control of switches) assert.match(control, /reason\(r\) => \{ root\.tap-reason = r; \}/);
  assert.match(row, /if Breakpoints\.below-d && root\.tap-reason != "": VerticalLayout/);
  assert.match(row, /IconNote \{ text: root\.tap-reason; \}/);
});
