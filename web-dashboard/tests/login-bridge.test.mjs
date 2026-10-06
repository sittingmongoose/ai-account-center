// The HTML login form bridge against a stub document: a manager fill (values set, input
// events dispatched, the 1Password shape) reaches `onFilled`; a submit runs `onSubmit` with the same
// values and never navigates; a Slint sign-in mirrors silently without looping back.
import test from 'node:test';
import assert from 'node:assert/strict';
import { installLoginBridge, readLoginForm, autofillChecks, overlayLayout } from '../public/login-bridge.mjs';

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

test('form.submit() from a manager runs the same sign-in instead of the browser posting the form', () => {
  // As a browser does it: submit() posts the form with no submit event, requestSubmit() fires the event first
  // and posts only when no listener prevented it.
  const { document, form, user, pass } = stubDocument();
  const listeners = [];
  let posted = 0;
  form.addEventListener = (event, fn) => { if (event === 'submit') listeners.push(fn); };
  form.submit = () => { posted++; };
  form.requestSubmit = () => {
    let prevented = false;
    for (const fn of listeners) fn({ preventDefault: () => { prevented = true; } });
    if (!prevented) posted++;
  };
  const submitted = [];
  installLoginBridge({ document, onSubmit: (v) => submitted.push(v) });
  user.value = 'owner';
  pass.value = 's3cret-password';
  form.submit();
  assert.equal(posted, 0, 'the browser never posts the hidden form');
  assert.deepEqual(submitted, [{ username: 'owner', password: 's3cret-password', remember: true }]);
});

// ---- the real login fields laid over the Slint sign-in page ----

/** One `login-overlay` report as lib.rs sends it: the form on screen at the 1920 x 1080 layout, fractional pixels. */
function report(over = {}) {
  return {
    on: true, enabled: true, revealed: false, remember: true,
    user: { x: 176.5, y: 401.25, w: 340, h: 42 },
    userText: { x: 188.5, y: 401.25, w: 316, h: 42 },
    pass: { x: 176.5, y: 476.25, w: 340, h: 42 },
    passText: { x: 188.5, y: 476.25, w: 286, h: 42 },
    check: { x: 176.5, y: 590.75, w: 16, h: 16 },
    submit: { x: 176.5, y: 627, w: 340, h: 44 },
    fontSize: 13.5, ink: 'rgba(21,32,43,1.000)', placeholder: 'rgba(149,162,174,1.000)', selection: 'rgba(37,82,204,0.251)', accent: 'rgba(37,82,204,1.000)',
    ...over,
  };
}

/** A browser-shaped form: style objects, focus that moves document.activeElement, focus/blur/pointer events. */
function liveDocument() {
  const doc = { activeElement: null };
  const element = (id, props = {}) => {
    const listeners = {};
    const el = {
      id, style: {}, readOnly: false, ...props,
      addEventListener: (type, fn) => { (listeners[type] ||= []).push(fn); },
      fire: (type, event = {}) => { for (const fn of listeners[type] || []) fn(event); },
      focus: () => {
        if (el.style.display === 'none') return;
        const before = doc.activeElement;
        if (before && before !== el) { doc.activeElement = null; before.fire?.('blur'); }
        doc.activeElement = el; el.fire('focus');
      },
    };
    return el;
  };
  const vars = {};
  const form = element('aac-login', { style: { setProperty: (name, value) => { vars[name] = value; } } });
  const user = element('aac-login-user', { value: '', type: 'text' });
  const pass = element('aac-login-pass', { value: '', type: 'password' });
  const remember = element('aac-login-remember', { checked: true, type: 'checkbox' });
  const submit = element('aac-login-submit', { type: 'submit' });
  const canvas = element('canvas');
  // display:none drops focus, as a browser does
  for (const el of [user, pass, remember, submit]) {
    let display = '';
    Object.defineProperty(el.style, 'display', {
      get: () => display,
      set: (value) => { display = value; if (value === 'none' && doc.activeElement === el) { doc.activeElement = null; el.fire('blur'); } },
      enumerable: true,
    });
  }
  const byId = { 'aac-login': form, 'aac-login-user': user, 'aac-login-pass': pass, 'aac-login-remember': remember, 'aac-login-submit': submit, canvas };
  doc.getElementById = (id) => byId[id] || null;
  return { document: doc, form, user, pass, remember, submit, canvas, vars };
}

test('overlayLayout puts the inputs over the Slint boxes in whole pixels, with the text where Slint draws it', () => {
  const layout = overlayLayout(report());
  assert.equal(layout.on, true);
  // 176.5 -> 177 (rounded edges, so the box never grows or shrinks by more than a pixel)
  assert.deepEqual(layout.user, { left: 177, top: 401, width: 340, height: 42, paddingLeft: 12, paddingRight: 14 });
  // the password input stops where its text area ends, so the Slint eye button beside it stays clickable
  assert.deepEqual(layout.pass, { left: 177, top: 476, width: 298, height: 42, paddingLeft: 12, paddingRight: 2 });
  assert.deepEqual(layout.check, { left: 177, top: 591, width: 16, height: 16 });
  assert.deepEqual(layout.submit, { left: 177, top: 627, width: 340, height: 44 });
  for (const box of [layout.user, layout.pass, layout.check, layout.submit]) {
    for (const value of Object.values(box)) assert.ok(Number.isInteger(value), `${value} is a whole pixel`);
  }
  assert.equal(layout.vars['--aac-login-size'], '13.5px');
  assert.equal(layout.vars['--aac-login-ink'], 'rgba(21,32,43,1.000)');
});

test('overlayLayout hides everything unless the form is on screen with real boxes, and refuses odd colours', () => {
  assert.equal(overlayLayout({ on: false, remember: false, revealed: true }).on, false);
  assert.deepEqual(overlayLayout({ on: false, remember: false, revealed: true }), { on: false, enabled: false, revealed: true, remember: false });
  assert.equal(overlayLayout(null).on, false);
  assert.equal(overlayLayout(report({ user: { x: 0, y: 0, w: 0, h: 42 } })).on, false, 'a zero-width box hides the inputs');
  assert.equal(overlayLayout(report({ passText: { x: NaN, y: 1, w: 2, h: 3 } })).on, false);
  assert.equal(overlayLayout(report({ check: null })).check, null, 'no Remember me box: only that target hides');
  assert.equal(overlayLayout(report({ ink: 'red;background:url(x)' })).vars['--aac-login-ink'], null);
});

test('place shows, moves and hides the real inputs; Remember me and the eye button stay in step', () => {
  const live = liveDocument();
  const pointer = [];
  const fills = [];
  const bridge = installLoginBridge({ document: live.document, onFilled: (v) => fills.push(v), onPointer: (p) => pointer.push(p) });
  bridge.place(report());
  assert.equal(bridge.shown(), true);
  assert.deepEqual(
    ['display', 'left', 'top', 'width', 'height', 'paddingLeft', 'paddingRight'].map((k) => live.user.style[k]),
    ['block', '177px', '401px', '340px', '42px', '12px', '14px'],
  );
  assert.deepEqual([live.pass.style.display, live.pass.style.width], ['block', '298px']);
  assert.deepEqual([live.remember.style.display, live.remember.style.left], ['block', '177px']);
  assert.deepEqual([live.submit.style.display, live.submit.style.top], ['block', '627px']);
  assert.equal(live.vars['--aac-login-accent'], 'rgba(37,82,204,1.000)');
  // the shake and a resize move the boxes: the inputs follow
  bridge.place(report({ user: { x: 183.4, y: 401.25, w: 340, h: 42 }, userText: { x: 195.4, y: 401.25, w: 316, h: 42 } }));
  assert.equal(live.user.style.left, '183px');
  // connecting: the fields are disabled, so the inputs keep focus but take no typing
  bridge.place(report({ enabled: false }));
  assert.deepEqual([live.user.readOnly, live.pass.readOnly], [true, true]);
  bridge.place(report());
  assert.deepEqual([live.user.readOnly, live.pass.readOnly], [false, false]);
  // Slint's Remember me and eye button change: the checkbox and the input type follow, silently
  bridge.place(report({ remember: false, revealed: true }));
  assert.deepEqual([live.remember.checked, live.pass.type], [false, 'text']);
  assert.equal(fills.length, 0, 'Slint-side changes never loop back as fills');
  bridge.place(report({ revealed: false }));
  assert.equal(live.pass.type, 'password');
  // focus and hover reach the Slint boxes
  live.user.fire('pointerenter');
  live.user.focus();
  assert.deepEqual(pointer.at(-1), { focus: 'user', hover: 'user' });
  live.pass.focus();
  assert.deepEqual(pointer.at(-1), { focus: 'pass', hover: 'user' });
  live.user.fire('pointerleave');
  assert.deepEqual(pointer.at(-1), { focus: 'pass', hover: '' });
  // the form leaves the screen (success, loading, sign-in done): every element hides and the canvas takes the keyboard
  bridge.place({ on: false, remember: true, revealed: false });
  assert.equal(bridge.shown(), false);
  for (const el of [live.user, live.pass, live.remember, live.submit]) assert.equal(el.style.display, 'none');
  assert.equal(live.document.activeElement, live.canvas);
  assert.deepEqual(pointer.at(-1), { focus: '', hover: '' });
});

test('a click on the free part of a Slint field box focuses the real input, now or once it shows', () => {
  let clock = 1000;
  const live = liveDocument();
  const bridge = installLoginBridge({ document: live.document, now: () => clock });
  bridge.focus('pass');
  assert.equal(live.document.activeElement, null, 'nothing to focus while hidden');
  bridge.place(report());
  assert.equal(live.document.activeElement, live.pass, 'the remembered request lands when the inputs show');
  bridge.focus('user');
  assert.equal(live.document.activeElement, live.user);
  // a request that waited too long is dropped, so focus never jumps later
  bridge.place({ on: false });
  bridge.focus('pass');
  clock += 5000;
  bridge.place(report());
  assert.notEqual(live.document.activeElement, live.pass);
  bridge.focus('nonsense');
});

test('clearPassword empties the real password input and mirror skips unchanged values', () => {
  const live = liveDocument();
  const bridge = installLoginBridge({ document: live.document });
  live.user.value = 'owner';
  live.pass.value = 'typed';
  bridge.clearPassword();
  assert.deepEqual([live.user.value, live.pass.value], ['owner', '']);
  bridge.mirror({ username: 'owner', password: 'x', remember: false });
  assert.deepEqual([live.user.value, live.pass.value, live.remember.checked], ['owner', 'x', false]);
});
