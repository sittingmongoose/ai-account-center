// The hidden HTML login form behind the Slint sign-in page (index.html `#aac-login`).
//
// The Slint canvas draws its own inputs, which password managers cannot see. This real HTML form is
// what they detect and fill: a username input with autocomplete="username" and a password input with
// autocomplete="current-password", inside a form with a submit button, rendered (never display:none)
// but transparent and inert. A fill lands in the visible Slint fields through `onFilled`; a submit
// (1Password's auto-submit, or Enter where the manager focuses) runs the same login as Sign in.
//
// The bridge never dispatches events itself: `mirror` sets values silently, so a Slint sign-in never
// loops back through `onFilled`.

/** What the hidden form holds. */
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

/**
 * Wire the hidden form. `document` is injected so tests drive a stub.
 * Returns a handle with `mirror(values)` and `read()`, or null without the form.
 */
export function installLoginBridge({ document, onFilled = () => {}, onSubmit = () => {} } = {}) {
  const form = document?.getElementById?.('aac-login') ?? null;
  if (!form) return null;
  const user = document.getElementById('aac-login-user');
  const pass = document.getElementById('aac-login-pass');
  const remember = document.getElementById('aac-login-remember');
  if (!user || !pass || !remember) return null;
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
  return {
    read,
    mirror({ username = '', password = '', remember: keep = true } = {}) {
      user.value = String(username);
      pass.value = String(password);
      remember.checked = keep !== false;
    },
  };
}

/**
 * Chrome's autofill heuristics, as a checklist one input must pass to be filled: it is in a form,
 * its autocomplete attribute names the field, and it is rendered (a box on the page, not
 * display:none or visibility:hidden; transparency is fine). `styleOf` is getComputedStyle.
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
