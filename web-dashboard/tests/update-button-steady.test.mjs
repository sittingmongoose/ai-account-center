import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// Regression test for the Update apps label that vibrated while a job ran (2026-10-06, live 13a93136).
// The pill clipped its content with rounded corners. In the femtovg renderer a clip with a radius draws the
// whole content into an offscreen layer, and that layer's placement follows the angle of the spinning
// ring: the "Updating" text moved by up to a pixel and changed its blur from frame to frame at 1x and 2x.
// Measured on the GPU, the text is pixel-identical frame after frame once the rounded clip is gone, so the
// pill does not clip and a plain rectangle (a scissor, no layer) just inside the border does.

const here = path.dirname(fileURLToPath(import.meta.url));
// The pill moved out of dashboard.slint into its own component (mobile phase 1); the invariants follow it.
const block = fs.readFileSync(path.join(here, '..', 'ui', 'components', 'update-button.slint'), 'utf8');
const lines = block.split('\n');
const comp = lines.findIndex(l => l.startsWith('export component UpdateButton inherits Rectangle {'));

/** The property lines of the element opened on `line`, at its own nesting depth only. */
function ownProperties(openIndex) {
  const indent = lines[openIndex].match(/^ */)[0].length + 4;
  const out = [];
  let depth = 0;
  for (let i = openIndex + 1; i < lines.length; i++) {
    const line = lines[i];
    if (depth === 0 && line.trim() === '}' && line.match(/^ */)[0].length === indent - 4) break;
    if (depth === 0 && line.match(/^ */)[0].length === indent) out.push(line.trim());
    depth += (line.match(/{/g) || []).length - (line.match(/}/g) || []).length;
    if (depth < 0) break;
  }
  return out;
}

test('the Update apps button is its own component with the steady-label rules', () => {
  assert.ok(comp > 0, 'UpdateButton is missing');
});

test('the rounded pill never clips its content (a rounded clip is an offscreen layer per frame)', () => {
  const root = ownProperties(comp);
  assert.ok(root.some(p => p.startsWith('border-radius:')), 'the pill should keep its rounded corners');
  assert.ok(!root.some(p => /^clip:\s*true/.test(p)), 'clip: true on the rounded pill re-renders the label through a layer');
});

test('the ring and the label sit in a plain rectangular clip just inside the border', () => {
  const open = lines.findIndex(l => /^\s*content := Rectangle \{/.test(l));
  assert.ok(open > 0, 'the content clip rectangle is missing');
  const props = ownProperties(open);
  assert.ok(props.includes('clip: true;'), 'content must clip (the width morph shows wider words for a moment)');
  assert.ok(!props.some(p => p.startsWith('border-radius:')), 'a radius here brings the offscreen layer back');
  const inside = block.slice(block.indexOf('content := Rectangle {'));
  assert.match(inside, /transform-rotation: root\.mode == "run"/, 'the spinning ring lives inside the content clip');
  assert.match(inside, /if root\.mode == "run" && root\.show-run-word: Text \{/, 'the running label lives inside the content clip');
  assert.match(inside, /text: "Updating";/, 'the Updating label lives inside the content clip');
});
