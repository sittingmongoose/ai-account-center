// The real HTML login fields over the Slint sign-in page (index.html `#aac-login`).
//
// The Slint canvas draws the sign-in card, but password managers cannot see canvas inputs, and they increasingly
// ignore near-invisible, inert or covered fields. So the username and password are real, fully visible <input>s in a
// real <form> with a submit button, laid exactly over the Slint field boxes while the login form is on screen: they
// draw the text, placeholder and caret, the Slint boxes under them draw everything else (border, focus halo, the eye
// button), and the Slint fields stop drawing their own text (ui/components/field.slint `covered`).
//
// - Geometry: the sign-in layer reports where the boxes are whenever the layout moves them (resize, DPI, the short
//   layout, breakpoints, banners, the wrong-password shake), as the `login-overlay` action; `place` puts the inputs
//   there in whole pixels, or hides them (display:none) whenever the form is not on screen (loading, signed in, the
//   success look, the first-run form, the layer fading in or out).
// - Values: typing and manager fills reach Slint through `onFilled` (bridge.js set_login_fields), so the Slint fields
//   show the same text whenever the inputs are hidden. `mirror` sets values silently, so a Slint sign-in never loops
//   back through `onFilled`.
// - Keyboard: Tab goes username, password, then the Remember me checkbox and the Sign in button, which lie over the
//   Slint controls as transparent focus targets (the mouse still reaches the Slint controls under them). Enter in
//   either field submits the form, and a submit (Enter, a manager's auto-submit, the button) runs `onSubmit`.
// - Focus and hover of the inputs go back to Slint through `onPointer`, so the boxes under them answer as before.

/** What the form holds. */
export function readLoginForm(form) {
  const user = form?.querySelector?.('#aac-login-user') ?? null;
  const pass = form?.querySelector?.('#aac-login-pass') ?? null;
  const remember = form?.querySelector?.('#aac-login-remember') ?? null;
  return {
    username: typeof user?.value === 'string' ? user.value : '',
    password: typeof pass?.value === 'string' ? pass.value : '',
    remember: remember ? remember.checked !== false : true,
  };
}

const finite = (value) => typeof value === 'number' && Number.isFinite(value);
const COLOR = /^rgba\(\d{1,3},\d{1,3},\d{1,3},(?:0|1|0?\.\d{1,3}|1\.0{1,3})\)$/;
/** Caret and text keep this much air at the right edge, as the Slint field does. */
const RIGHT_AIR = 2;

/** A reported box as whole-pixel edges, or null when it is missing or empty. */
function edges(box) {
  if (!box || !finite(box.x) || !finite(box.y) || !finite(box.w) || !finite(box.h) || box.w <= 0 || box.h <= 0) return null;
  const left = Math.round(box.x);
  const top = Math.round(box.y);
  const right = Math.round(box.x + box.w);
  const bottom = Math.round(box.y + box.h);
  return right > left && bottom > top ? { left, top, right, bottom } : null;
}
const boxOf = ({ left, top, right, bottom }) => ({ left, top, width: right - left, height: bottom - top });

/**
 * Where each element of the form goes for one `login-overlay` report (lib.rs login_overlay_json), in whole CSS
 * pixels. The username input covers its whole box; the password input stops at the end of its text area, so the
 * Slint eye button beside it stays clickable. Each input's padding puts its text where the Slint text area starts.
 * `on` is false (everything hidden) unless the report says the form is on screen and both boxes are real.
 */
export function overlayLayout(overlay) {
  const o = overlay && typeof overlay === 'object' ? overlay : {};
  const base = { on: false, enabled: false, revealed: o.revealed === true, remember: o.remember !== false };
  if (o.on !== true) return base;
  const user = edges(o.user), userText = edges(o.userText), pass = edges(o.pass), passText = edges(o.passText);
  if (!user || !userText || !pass || !passText) return base;
  const passRight = Math.min(pass.right, Math.max(passText.right, pass.left + 1));
  const color = (value) => (typeof value === 'string' && COLOR.test(value) ? value : null);
  const check = edges(o.check), submit = edges(o.submit);
  return {
    ...base,
    on: true,
    enabled: o.enabled === true,
    user: { ...boxOf(user), paddingLeft: Math.max(0, userText.left - user.left), paddingRight: Math.max(0, user.right - userText.right) + RIGHT_AIR },
    pass: { ...boxOf({ ...pass, right: passRight }), paddingLeft: Math.max(0, passText.left - pass.left), paddingRight: RIGHT_AIR },
    check: check ? boxOf(check) : null,
    submit: submit ? boxOf(submit) : null,
    vars: {
      '--aac-login-size': finite(o.fontSize) && o.fontSize > 0 ? `${o.fontSize}px` : null,
      '--aac-login-ink': color(o.ink),
      '--aac-login-placeholder': color(o.placeholder),
      '--aac-login-selection': color(o.selection),
      '--aac-login-accent': color(o.accent),
    },
  };
}

const FOCUS_REQUEST_MS = 1500;

/**
 * Wire the form. `document` is injected so tests drive a stub; `canvas` gets the keyboard back when a focused input
 * is hidden. Returns a handle with `read()`, `mirror(values)`, `clearPassword()`, `place(overlay)`, `focus(field)`
 * and `shown()`, or null without the form.
 */
export function installLoginBridge({ document, canvas = null, onFilled = () => {}, onSubmit = () => {}, onPointer = () => {}, now = () => Date.now() } = {}) {
  const form = document?.getElementById?.('aac-login') ?? null;
  if (!form) return null;
  const user = document.getElementById('aac-login-user');
  const pass = document.getElementById('aac-login-pass');
  const remember = document.getElementById('aac-login-remember');
  const submit = document.getElementById('aac-login-submit') ?? null;
  if (!user || !pass || !remember) return null;
  const screen = canvas ?? document.getElementById('canvas') ?? null;
  const read = () => ({
    username: typeof user.value === 'string' ? user.value : '',
    password: typeof pass.value === 'string' ? pass.value : '',
    remember: remember.checked !== false,
  });
  const filled = () => onFilled(read());
  user.addEventListener('input', filled);
  pass.addEventListener('input', filled);
  remember.addEventListener('change', filled);
  form.addEventListener('submit', (event) => {
    if (typeof event?.preventDefault === 'function') event.preventDefault();
    onSubmit(read());
  });
  // form.submit() fires no submit event, so a manager that submits that way would make the browser post the
  // form itself and land on the API's JSON answer. requestSubmit() fires the event above instead.
  if (typeof form.requestSubmit === 'function') form.submit = () => form.requestSubmit();

  // focus and hover of the two inputs, for the Slint boxes under them
  const pointer = { focus: '', hover: '' };
  const report = () => { try { onPointer({ ...pointer }); } catch {} };
  for (const [field, input] of [['user', user], ['pass', pass]]) {
    input.addEventListener('focus', () => { pointer.focus = field; report(); });
    input.addEventListener('blur', () => { if (pointer.focus === field) { pointer.focus = ''; report(); } });
    input.addEventListener('pointerenter', () => { pointer.hover = field; report(); });
    input.addEventListener('pointerleave', () => { if (pointer.hover === field) { pointer.hover = ''; report(); } });
  }

  const parts = [user, pass, remember, ...(submit ? [submit] : [])];
  const placed = new Map();
  let on = false;
  let wanted = null;
  const show = (element, box, extra = {}) => {
    const style = element.style;
    if (!style) return;
    const next = box ? { display: 'block', left: `${box.left}px`, top: `${box.top}px`, width: `${box.width}px`, height: `${box.height}px`, ...extra } : { display: 'none' };
    const key = JSON.stringify(next);
    if (placed.get(element) === key) return;
    placed.set(element, key);
    for (const [name, value] of Object.entries(next)) style[name] = value;
  };
  const focusIn = () => parts.includes(document.activeElement);
  const focusField = (field) => {
    const input = field === 'pass' ? pass : field === 'user' ? user : null;
    try { input?.focus?.({ preventScroll: true }); } catch {}
  };

  function place(overlay) {
    const layout = overlayLayout(overlay);
    if (remember.checked !== layout.remember) remember.checked = layout.remember;
    const type = layout.revealed ? 'text' : 'password';
    if (pass.type !== type) pass.type = type;
    if (!layout.on) {
      const hadFocus = focusIn();
      on = false;
      for (const element of parts) show(element, null);
      if (pointer.hover || pointer.focus) { pointer.hover = ''; pointer.focus = ''; report(); }
      // a hidden input drops the keyboard; the canvas takes it back, so the dashboard's keys keep working
      if (hadFocus) { try { screen?.focus?.({ preventScroll: true }); } catch {} }
      return layout;
    }
    on = true;
    for (const [name, value] of Object.entries(layout.vars)) {
      if (value !== null) form.style?.setProperty?.(name, value);
    }
    const readOnly = !layout.enabled;
    if (user.readOnly !== readOnly) user.readOnly = readOnly;
    if (pass.readOnly !== readOnly) pass.readOnly = readOnly;
    show(user, layout.user, { paddingLeft: `${layout.user.paddingLeft}px`, paddingRight: `${layout.user.paddingRight}px` });
    show(pass, layout.pass, { paddingLeft: `${layout.pass.paddingLeft}px`, paddingRight: `${layout.pass.paddingRight}px` });
    show(remember, layout.check);
    if (submit) show(submit, layout.submit);
    if (wanted && now() - wanted.at <= FOCUS_REQUEST_MS) focusField(wanted.field);
    wanted = null;
    return layout;
  }

  return {
    read,
    place,
    shown: () => on,
    /** A click on the part of a Slint field box the input leaves free: focus the input, now or as soon as it shows. */
    focus(field) {
      if (field !== 'user' && field !== 'pass') return;
      if (on) focusField(field);
      else wanted = { field, at: now() };
    },
    clearPassword() { pass.value = ''; },
    mirror({ username = '', password = '', remember: keep = true } = {}) {
      if (user.value !== String(username)) user.value = String(username);
      if (pass.value !== String(password)) pass.value = String(password);
      remember.checked = keep !== false;
    },
  };
}

/**
 * Chrome's autofill heuristics, as a checklist one input must pass to be filled: it is in a form, its autocomplete
 * attribute names the field, and it is rendered (a box on the page, not display:none or visibility:hidden).
 * `styleOf` is getComputedStyle.
 */
export function autofillChecks(input, styleOf) {
  const style = styleOf(input);
  const rect = typeof input.getBoundingClientRect === 'function' ? input.getBoundingClientRect() : { width: 0, height: 0 };
  return {
    inForm: !!input.form,
    autocomplete: input.getAttribute ? input.getAttribute('autocomplete') : null,
    rendered:
      !!rect && rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden',
  };
}
