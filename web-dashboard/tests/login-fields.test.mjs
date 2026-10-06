// The login form password managers fill: real, visible, focusable HTML fields (index.html #aac-login) laid over the
// Slint sign-in fields, and the Slint side that reports where those fields are and when they are on screen
// (ui/shell/signin.slint, ui/components/field.slint, src/lib.rs). Source checks; the motion and pixels are checked in
// a GPU browser (status/FW4-LOGIN-FIELDS.md).
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const read = (path) => readFile(new URL(path, import.meta.url), 'utf8');
const tag = (html, id) => {
  const match = new RegExp(`<(input|button|form)\\b[^>]*\\bid="${id}"[^>]*>`).exec(html);
  assert.ok(match, `${id} exists`);
  return match[0];
};

test('index.html: the login fields are real, visible, focusable inputs in a form with a submit button', async () => {
  const html = await read('../public/index.html');
  const form = tag(html, 'aac-login');
  const user = tag(html, 'aac-login-user');
  const pass = tag(html, 'aac-login-pass');
  const remember = tag(html, 'aac-login-remember');
  const submit = tag(html, 'aac-login-submit');
  assert.match(user, /autocomplete="username"/);
  assert.match(user, /type="text"/);
  assert.match(pass, /autocomplete="current-password"/);
  assert.match(pass, /type="password"/);
  assert.match(submit, /type="submit"/);
  for (const element of [form, user, pass, remember, submit]) {
    assert.doesNotMatch(element, /aria-hidden/, `${element} is not hidden from assistive tech or managers`);
    assert.doesNotMatch(element, /tabindex="-1"/, `${element} takes keyboard focus`);
  }
  // the form sits inside the page above the canvas, never transparent; its box lets the mouse through to the Slint
  // card, and the two fields take it back
  const css = /<style>([\s\S]*?)<\/style>/.exec(html)[1];
  const formRule = /#aac-login\{([^}]*)\}/.exec(css)[1];
  assert.match(formRule, /z-index:1/);
  assert.doesNotMatch(formRule, /opacity/);
  const fieldRule = /#aac-login>#aac-login-user,#aac-login>#aac-login-pass\{([^}]*)\}/.exec(css)[1];
  assert.doesNotMatch(fieldRule, /opacity|visibility/);
  assert.match(fieldRule, /pointer-events:auto/);
  assert.match(fieldRule, /"AAC Field"/, 'the inputs use the Slint field font');
  // only the two transparent focus targets over the Slint Remember me and Sign in let the mouse through to Slint
  assert.match(css, /#aac-login>#aac-login-remember,#aac-login>#aac-login-submit\{[^}]*pointer-events:none/);
  // the fonts the inputs draw with are served next to the page: Instrument Sans for the text, and Slint's own password
  // dot (U+25CF from its Inter fallback) for the mask characters browsers use, U+2022 and U+25CF
  assert.match(css, /@font-face\{font-family:"AAC Field";src:url\(\/assets\/InstrumentSans\.ttf\)[^}]*unicode-range:U\+0-2021,U\+2023-25CE,U\+25D0-10FFFF\}/);
  assert.match(css, /@font-face\{font-family:"AAC Field";src:url\(\/assets\/AACMask\.ttf\)[^}]*unicode-range:U\+2022,U\+25CF\}/);
  const mask = await readFile(new URL('../public/assets/AACMask.ttf', import.meta.url));
  assert.ok(mask.length > 0 && mask.length < 4096, 'the mask font holds one glyph');
  const served = await readFile(new URL('../public/assets/InstrumentSans.ttf', import.meta.url));
  const embedded = await readFile(new URL('../ui/fonts/InstrumentSans.ttf', import.meta.url));
  assert.ok(served.equals(embedded), 'the served font is the one Slint embeds (rebuild both together)');
});

test('signin.slint: the inputs show only while the login form is on screen and still', async () => {
  const slint = await read('../ui/shell/signin.slint');
  const rule = /property <bool> overlay-on:([^;]*);/.exec(slint)?.[1] ?? '';
  // asked for and fully faded in, the card's load-in finished
  for (const part of ['root.visible', 'root.shown', 'root.opacity > 0.999', 'card-reveal.level > 0.999']) assert.ok(rule.includes(part), part);
  // never while loading, setting up (the first-run form keeps its Slint fields) or signed in (success and the fade out)
  for (const part of ['root.form', '!root.setup', '!root.signed-in', '!root.loading']) assert.ok(rule.includes(part), part);
  // the report: off (no geometry) unless on, and every change goes out
  assert.match(slint, /property <LoginOverlay> overlay: root\.overlay-on \? root\.overlay-live : root\.overlay-off;/);
  assert.match(slint, /changed overlay => \{ root\.login-overlay\(self\.overlay\); \}/);
  // the two login fields stop drawing their text while covered, and never take typing themselves
  const covered = slint.match(/covered: root\.overlay-on;/g) ?? [];
  const mirror = slint.match(/mirror: !root\.setup;/g) ?? [];
  assert.equal(covered.length, 2);
  assert.equal(mirror.length, 2);
  // the geometry follows every layout change of the conditional form (copied out on init and on each change)
  for (const name of ['user-at', 'user-text-at', 'pass-at', 'pass-text-at']) assert.match(slint, new RegExp(`changed ${name} => \\{ self\\.publish\\(\\); \\}`));
  assert.match(slint, /changed where => \{ root\.check-box = self\.where; \}/);
});

test('field.slint: a covered field draws no text and a mirrored one never takes focus', async () => {
  const field = await read('../ui/components/field.slint');
  assert.match(field, /enabled: root\.enabled && !root\.mirror;/);
  assert.match(field, /visible: !root\.covered;/);
  assert.match(field, /if root\.mirror \{\s*root\.focus-request\(\);/);
  assert.match(field, /out property <length> box-x: box\.absolute-position\.x;/);
  assert.match(field, /out property <length> text-x: viewport\.absolute-position\.x;/);
});

test('lib.rs: the report goes to bridge.js as the login-overlay action, with geometry only while on', async () => {
  const rust = await read('../src/lib.rs');
  assert.match(rust, /ui\.on_login_overlay\(\|overlay\| dispatch_action\("login-overlay", &login_overlay_json\(&overlay\)\)\);/);
  assert.match(rust, /pub fn set_login_pointer\(focus: &str, hover: &str\)/);
  assert.match(rust, /if o\.on \{/);
});

/** The declarations of every rule whose selector list is exactly `selector`, joined. */
const allRules = (css, selector) => {
  const out = [];
  for (let at = css.indexOf(`${selector}{`); at >= 0; at = css.indexOf(`${selector}{`, at + 1)) {
    if (at === 0 || css[at - 1] === '\n' || css[at - 1] === '}') out.push(css.slice(at + selector.length + 1, css.indexOf('}', at)));
  }
  return out.join(';');
};
const ruleOf = (css, selector) => {
  const rules = allRules(css, selector);
  assert.ok(rules, `${selector} has a rule`);
  return rules;
};
/** CSS specificity [ids, classes/attributes/pseudo-classes, types] of a simple selector chain. */
const specificity = (selector) => [
  (selector.match(/#[\w-]+/g) ?? []).length,
  (selector.match(/\[[^\]]*\]|\.[\w-]+|:(?!:)[\w-]+/g) ?? []).length,
  (selector.replace(/\[[^\]]*\]|#[\w-]+|\.[\w-]+|::?[\w-]+(\([^)]*\))?/g, ' ').match(/[a-z][\w-]*/gi) ?? []).length,
];
const beats = (a, b) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2];

test('index.html: every field a manager fills has an offsetParent, so 1Password fills both at once', async () => {
  const html = await read('../public/index.html');
  const css = /<style>([\s\S]*?)<\/style>/.exec(html)[1];
  // 1Password's field collector reads a null offsetParent as "not viewable" and then fills only the focused field.
  // A position:fixed element has no offsetParent; an absolute one inside a positioned form has the form.
  const parts = ruleOf(css, '#aac-login>input,#aac-login>button');
  assert.match(parts, /position:absolute/);
  assert.doesNotMatch(parts, /position:fixed/);
  const form = ruleOf(css, '#aac-login');
  assert.match(form, /position:(fixed|absolute|relative)/, 'the form is the fields’ offsetParent');
  assert.match(form, /pointer-events:none/, 'the form box lets the mouse through to the Slint card');
  // the fields stay one form: the inputs and the submit button are children of #aac-login
  const body = /<form id="aac-login"[^>]*>([\s\S]*?)<\/form>/.exec(html)[1];
  for (const id of ['aac-login-user', 'aac-login-pass', 'aac-login-remember', 'aac-login-submit']) assert.match(body, new RegExp(`id="${id}"`));
});

test('index.html: the inputs paint nothing but their text, whatever autofill or a manager adds', async () => {
  const html = await read('../public/index.html');
  const css = /<style>([\s\S]*?)<\/style>/.exec(html)[1];
  const selector = '#aac-login>#aac-login-user,#aac-login>#aac-login-pass';
  const guard = allRules(css, selector);
  // no background (1Password's filled highlight, Chrome's autofill blue), no border, outline or shadow: the Slint box
  // under the input, with its own focus halo, is all that shows
  for (const rule of ['background-color:transparent!important', 'background-image:none!important', 'border:0!important', 'outline:0!important', 'box-shadow:none!important', 'animation:none!important']) {
    assert.ok(guard.includes(rule), rule);
  }
  // a colour forced from a stronger origin (the browser's own autofill, an inline !important) is clipped to the
  // glyphs, which the text then covers
  assert.ok(guard.includes('-webkit-background-clip:text!important') && guard.includes('background-clip:text!important'));
  // 1Password marks filled fields data-com-onepassword-filled and paints them with an !important background:
  // the guard's selector outranks it
  for (const one of selector.split(',')) assert.ok(beats(specificity(one), specificity('input[data-com-onepassword-filled="light"]')) > 0, `${one} outranks 1Password's rule`);
  // Chrome's and Brave's autofill: text and caret keep the Slint ink, and the UA background never fades in
  for (const pseudo of [':-webkit-autofill', ':autofill']) {
    const rule = ruleOf(css, `#aac-login>input${pseudo}`);
    assert.match(rule, /-webkit-text-fill-color:var\(--aac-login-ink\)!important/);
    assert.match(rule, /caret-color:var\(--aac-login-ink\)!important/);
    assert.match(rule, /transition:background-color 100000s 0s/);
  }
  // the inputs take the Slint box's rounded corners (field.slint), the password's right side ends inside the box
  const field = await read('../ui/components/field.slint');
  const radius = /box := Rectangle \{[^}]*?border-radius: (\d+)px;/.exec(field)[1];
  assert.match(guard, new RegExp(`border-radius:${radius}px`));
  assert.match(ruleOf(css, '#aac-login>#aac-login-pass'), new RegExp(`border-radius:${radius}px 0 0 ${radius}px`));
  // and they never keep a browser focus ring: the Slint halo is the only focus cue
  assert.doesNotMatch(css, /#aac-login>#aac-login-(user|pass):focus/);
});
