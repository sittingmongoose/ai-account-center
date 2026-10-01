import test from 'node:test';
import assert from 'node:assert/strict';
import { webGlAvailable, requireWebGL, WEBGL_REQUIRED_MESSAGE, startSlintDashboard } from '../public/renderer.mjs';

test('denied or unavailable WebGL yields a readable startup requirement', () => {
  for (const createCanvas of [() => ({ getContext: () => null }), () => ({ getContext: () => { throw new Error('blocked'); } })]) {
    assert.equal(webGlAvailable(createCanvas), false);
    assert.throws(() => requireWebGL(createCanvas), error => error.code === 'webgl_required' && error.message === WEBGL_REQUIRED_MESSAGE);
  }
});

test('WebGL is probed on a separate canvas and its temporary context is released', () => {
  let creations = 0;
  let released = false;
  assert.equal(webGlAvailable(() => {
    creations++;
    return { getContext: kind => {
      assert.equal(kind, 'webgl2');
      return { getExtension: name => {
        assert.equal(name, 'WEBGL_lose_context');
        return { loseContext: () => { released = true; } };
      } };
    } };
  }), true);
  assert.equal(creations, 1);
  assert.equal(released, true);
});

test('winit browser event-loop takeover continues the dashboard bootstrap', () => {
  const signal = "Using exceptions for control flow, don't mind me. This isn't actually an error!";
  for (const error of [new Error(signal), signal]) {
    let connected = false;
    startSlintDashboard(() => {
      throw error;
    });
    connected = true;
    assert.equal(connected, true);
  }
  let started = false;
  startSlintDashboard(() => { started = true; });
  assert.equal(started, true);
});

test('real startup errors and near-match event-loop errors still propagate unchanged', () => {
  for (const error of [new Error('WebGL unavailable'), new Error("Using exceptions for control flow, don't mind me."), new Error('Invalid dashboard view data'), null]) {
    let caught = false;
    try { startSlintDashboard(() => { throw error; }); }
    catch (actual) { caught = true; assert.equal(actual, error); }
    assert.equal(caught, true);
  }
});
