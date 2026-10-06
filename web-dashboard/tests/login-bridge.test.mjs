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
    getBoundingClientRect: () => ({ left: 100, top: 50, width: 200, height: 28 }),
    clientWidth: 200,
    clientHeight: 28,
    offsetParent: form,
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

test("a manager's fill checklist: Chrome's (in a form, named, rendered) and 1Password's viewable rule", () => {
  const { user, pass } = stubDocument();
  const styleOf = () => ({ display: 'block', visibility: 'visible' });
  const at = (el) => () => el;
  const all = { rendered: true, positioned: true, viewable: true };
  assert.deepEqual(autofillChecks(user, styleOf, at(user)), { inForm: true, autocomplete: 'username', ...all });
  assert.deepEqual(autofillChecks(pass, styleOf, at(pass)), { inForm: true, autocomplete: 'current-password', ...all });
  // display:none fails the checklist, as it fails Chrome
  assert.equal(autofillChecks(user, () => ({ display: 'none', visibility: 'visible' })).rendered, false);
  const orphan = stubInput('x', { autocomplete: 'username', form: null });
  assert.equal(autofillChecks(orphan, styleOf).inForm, false);
  // a position:fixed field has no offsetParent: 1Password's collector calls it not viewable and fills only the
  // field that has focus, one field per fill (the 13a93136 behaviour)
  const fixed = { ...user, offsetParent: null };
  assert.deepEqual([autofillChecks(fixed, styleOf, at(fixed)).positioned, autofillChecks(fixed, styleOf, at(fixed)).viewable], [false, false]);
  // covered by the canvas, or too small, is not viewable either
  assert.equal(autofillChecks(user, styleOf, () => ({ id: 'canvas' })).viewable, false);
  assert.equal(autofillChecks({ ...user, clientHeight: 8 }, styleOf, at(user)).viewable, false);
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
      blur: () => { if (doc.activeElement === el) { doc.activeElement = null; el.fire('blur'); } },
      click: () => el.fire('click'),
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
  // the form's own box holds everything it places: the fields, Remember me and Sign in
  assert.deepEqual(layout.frame, { left: 177, top: 401, width: 340, height: 270 });
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
  // the form is a box around the fields in window pixels; each field sits inside it, relative to its corner
  assert.deepEqual(['left', 'top', 'width', 'height'].map((k) => live.form.style[k]), ['177px', '401px', '340px', '270px']);
  assert.deepEqual(
    ['display', 'left', 'top', 'width', 'height', 'paddingLeft', 'paddingRight'].map((k) => live.user.style[k]),
    ['block', '0px', '0px', '340px', '42px', '12px', '14px'],
  );
  assert.deepEqual([live.pass.style.display, live.pass.style.top, live.pass.style.width], ['block', '75px', '298px']);
  assert.deepEqual([live.remember.style.display, live.remember.style.left, live.remember.style.top], ['block', '0px', '190px']);
  assert.deepEqual([live.submit.style.display, live.submit.style.top], ['block', '226px']);
  assert.equal(live.vars['--aac-login-accent'], 'rgba(37,82,204,1.000)');
  // the shake and a resize move the boxes: the inputs follow
  bridge.place(report({ user: { x: 183.4, y: 401.25, w: 340, h: 42 }, userText: { x: 195.4, y: 401.25, w: 316, h: 42 } }));
  assert.deepEqual([live.form.style.left, live.form.style.width, live.user.style.left], ['177px', '346px', '6px']);
  bridge.place(report({ user: { x: 183.4, y: 401.25, w: 340, h: 42 }, userText: { x: 195.4, y: 401.25, w: 316, h: 42 }, pass: { x: 183.4, y: 476.25, w: 340, h: 42 }, passText: { x: 195.4, y: 476.25, w: 286, h: 42 }, check: { x: 183.4, y: 590.75, w: 16, h: 16 }, submit: { x: 183.4, y: 627, w: 340, h: 44 } }));
  assert.deepEqual([live.form.style.left, live.user.style.left, live.pass.style.left], ['183px', '0px', '0px'], 'the shake moves the form');
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
  assert.deepEqual([live.form.style.width, live.form.style.height], ['0px', '0px'], 'an empty form keeps no box');
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

/**
 * 1Password's own fill operations for one field, as its fill script runs them (agilebits onepassword-app-extension,
 * fill `G`: click and focus, keydown/keypress/keyup, the value, keydown/keypress/keyup, input, change, blur).
 */
function onePasswordFill(el, value) {
  el.click();
  el.focus();
  for (const type of ['keydown', 'keypress', 'keyup']) el.fire(type);
  el.value = value;
  for (const type of ['keydown', 'keypress', 'keyup', 'input', 'change']) el.fire(type);
  el.blur();
}

test('a 1Password fill sets the username and the password in one go, then auto-submits with both', () => {
  const live = liveDocument();
  const fills = [], submitted = [];
  const snapshot = () => ({ display: live.pass.style.display, left: live.pass.style.left, top: live.pass.style.top, readOnly: live.pass.readOnly, type: live.pass.type });
  let bridge;
  // Slint answers every focus and hover change and every value as it does in the page: the pointer report redraws
  // the boxes' halo and border and re-reports the same geometry; the values land in the Slint fields
  bridge = installLoginBridge({
    document: live.document,
    canvas: live.canvas,
    onFilled: (v) => { fills.push(v); bridge.place(report()); },
    onPointer: () => bridge.place(report()),
    onSubmit: (v) => submitted.push(v),
  });
  bridge.place(report());
  const before = snapshot();
  onePasswordFill(live.user, 'owner');
  // nothing the username fill set off moved, hid, disabled or retyped the password input, or took the keyboard
  assert.deepEqual(snapshot(), before);
  assert.notEqual(live.document.activeElement, live.canvas);
  assert.equal(bridge.shown(), true);
  onePasswordFill(live.pass, 's3cret-password');
  assert.deepEqual(fills.at(-1), { username: 'owner', password: 's3cret-password', remember: true }, 'both values reach Slint');
  assert.deepEqual([live.user.value, live.pass.value], ['owner', 's3cret-password']);
  // 1Password's auto-submit: a click on the form's submit button fires the form's submit event
  live.form.fire('submit', { preventDefault: () => {} });
  assert.deepEqual(submitted, [{ username: 'owner', password: 's3cret-password', remember: true }]);
});

test('a fill while the username has focus (the inline menu) fills both too', () => {
  const live = liveDocument();
  const fills = [];
  let bridge;
  bridge = installLoginBridge({ document: live.document, canvas: live.canvas, onFilled: (v) => fills.push(v), onPointer: () => bridge.place(report()) });
  bridge.place(report());
  live.user.fire('pointerenter');
  live.user.focus();
  onePasswordFill(live.user, 'owner');
  onePasswordFill(live.pass, 'pw-2');
  // and with a manager that never focuses: values and input events only
  live.user.value = 'owner'; live.user.fire('input');
  live.pass.value = 'pw-3'; live.pass.fire('input');
  assert.deepEqual(fills.map((f) => f.password), ['', 'pw-2', 'pw-2', 'pw-3']);
  assert.deepEqual([live.user.style.display, live.pass.style.display], ['block', 'block']);
});

test("a manager's auto-submit by a synthetic Enter signs in; a real Enter is left to the browser", () => {
  const { document, form, user, pass } = stubDocument();
  const listeners = [];
  form.addEventListener = (event, fn) => { if (event === 'submit') listeners.push(fn); };
  form.requestSubmit = () => { for (const fn of listeners) fn({ preventDefault: () => {} }); };
  const keys = {};
  for (const input of [user, pass]) {
    input.addEventListener = (event, fn) => { (keys[`${input.id}:${event}`] ||= []).push(fn); };
  }
  const submitted = [];
  installLoginBridge({ document, onSubmit: (v) => submitted.push(v) });
  const press = (input, event) => { for (const fn of keys[`${input.id}:keydown`] || []) fn(event); };
  user.value = 'owner';
  pass.value = 's3cret-password';
  // the fill's own key events carry no key, and other keys do nothing
  press(pass, { isTrusted: false });
  press(pass, { key: 'a', isTrusted: false });
  // a real Enter submits through the browser's implicit submission, so the bridge must not submit a second time
  press(pass, { key: 'Enter', isTrusted: true });
  assert.equal(submitted.length, 0);
  let stopped = false;
  press(pass, { key: 'Enter', isTrusted: false, preventDefault: () => { stopped = true; } });
  assert.deepEqual(submitted, [{ username: 'owner', password: 's3cret-password', remember: true }]);
  assert.equal(stopped, true);
});
