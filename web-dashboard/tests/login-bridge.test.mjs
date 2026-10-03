// The hidden HTML login form bridge against a stub document: a manager fill (values set, input
// events dispatched, the 1Password shape) reaches `onFilled`; a submit runs `onSubmit` with the same
// values and never navigates; a Slint sign-in mirrors silently without looping back.
import test from 'node:test';
import assert from 'node:assert/strict';
import { installLoginBridge, readLoginForm, autofillChecks } from '../public/login-bridge.mjs';

function stubInput(id, { value = '', checked = true, autocomplete = null, form = null } = {}) {
  const listeners = {};
  return {
    id,
    value,
    checked,
    form,
    getAttribute: (name) => (name === 'autocomplete' ? autocomplete : null),
    addEventListener: (event, fn) => { (listeners[event] ||= []).push(fn); },
    dispatch: (event) => { for (const fn of listeners[event] || []) fn({}); },
    getBoundingClientRect: () => ({ width: 200, height: 28 }),
  };
}

function stubDocument() {
  const listeners = {};
  const form = {
    addEventListener: (event, fn) => { (listeners[event] ||= []).push(fn); },
    submit: () => { for (const fn of listeners.submit || []) fn({ preventDefault: () => { form.stopped = true; } }); },
    stopped: false,
    querySelector: (sel) => byId[sel.slice(1)] || null,
  };
  const user = stubInput('aac-login-user', { autocomplete: 'username', form });
  const pass = stubInput('aac-login-pass', { autocomplete: 'current-password', form });
  const remember = stubInput('aac-login-remember', { form });
  const byId = { 'aac-login': form, 'aac-login-user': user, 'aac-login-pass': pass, 'aac-login-remember': remember };
  return { document: { getElementById: (id) => byId[id] || null }, form, user, pass, remember };
}

test('a 1Password-shaped fill reaches onFilled and a submit runs onSubmit without navigating', () => {
  const { document, form, user, pass, remember } = stubDocument();
  const filled = [], submitted = [];
  const bridge = installLoginBridge({ document, onFilled: (v) => filled.push(v), onSubmit: (v) => submitted.push(v) });
  assert.ok(bridge);
  // 1Password sets the values and dispatches input events
  user.value = 'owner';
  user.dispatch('input');
  pass.value = 's3cret-password';
  pass.dispatch('input');
  remember.checked = false;
  remember.dispatch('change');
  assert.deepEqual(filled.at(-1), { username: 'owner', password: 's3cret-password', remember: false });
  form.submit();
  assert.equal(form.stopped, true);
  assert.deepEqual(submitted, [{ username: 'owner', password: 's3cret-password', remember: false }]);
  assert.deepEqual(bridge.read(), { username: 'owner', password: 's3cret-password', remember: false });
});

test('a Slint sign-in mirrors silently: no fill event loops back', () => {
  const { document, user, pass, remember } = stubDocument();
  let fills = 0;
  const bridge = installLoginBridge({ document, onFilled: () => { fills++; }, onSubmit: () => {} });
  bridge.mirror({ username: 'owner', password: 'typed-in-slint', remember: true });
  assert.equal(user.value, 'owner');
  assert.equal(pass.value, 'typed-in-slint');
  assert.equal(remember.checked, true);
  assert.equal(fills, 0);
  bridge.mirror({ username: 'owner', password: 'x', remember: false });
  assert.equal(remember.checked, false);
});

test('readLoginForm reads the three fields; missing form installs to null', () => {
  const { document } = stubDocument();
  const form = document.getElementById('aac-login');
  document.getElementById('aac-login-user').value = 'u';
  assert.deepEqual(readLoginForm(form), { username: 'u', password: '', remember: true });
  assert.equal(installLoginBridge({ document: { getElementById: () => null } }), null);
});

test("Chrome's autofill heuristics checklist: in a form, named, and rendered", () => {
  const { user, pass } = stubDocument();
  const styleOf = () => ({ display: 'block', visibility: 'visible' });
  assert.deepEqual(autofillChecks(user, styleOf), { inForm: true, autocomplete: 'username', rendered: true });
  assert.deepEqual(autofillChecks(pass, styleOf), { inForm: true, autocomplete: 'current-password', rendered: true });
  // display:none fails the checklist, as it fails Chrome
  assert.equal(autofillChecks(user, () => ({ display: 'none', visibility: 'visible' })).rendered, false);
  const orphan = stubInput('x', { autocomplete: 'username', form: null });
  assert.equal(autofillChecks(orphan, styleOf).inForm, false);
});
