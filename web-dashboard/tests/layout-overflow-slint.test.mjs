// Source contracts for the layout overflows found by rendering the dashboard at the edges of its tiers.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
const read = path => readFileSync(new URL(`../ui/${path}`, import.meta.url), 'utf8');

test('Accounts & Settings folds the source column in the first 120 px of the two-column tier', () => {
  const source = read('pages/accounts.slint');
  assert.match(source, /show-src: Breakpoints.width < 1280px \|\| \(Breakpoints.width >= 1440px && !\(Breakpoints.ultra && Breakpoints.width < 2320px\)\)/);
});

test('a focused page field is scrolled above the on-screen keyboard', () => {
  const field = read('components/field.slint');
  assert.match(field, /export global FocusedField/);
  // the sign-in page's mirror fields scroll their own page
  assert.match(field, /has-keyboard: input.has-focus && !root.mirror/);
  assert.match(field, /else if FocusedField.serial == root.focus-serial \{\s*FocusedField.on = false;/);
  assert.match(field, /changed box-y => \{\s*if root.has-keyboard && FocusedField.serial == root.focus-serial/);
  const shell = read('dashboard.slint');
  assert.match(shell, /changed kb-h => \{ root.reveal-field\(\); \}/);
  assert.match(shell, /changed field-serial => \{ root.reveal-field\(\); \}/);
  assert.match(shell, /FocusedField.bottom - \(root.height - Device.keyboard - 12px\)/);
  assert.match(shell, /FocusedField.top - \(root.chrome-h \+ 12px\)/);
  assert.match(shell, /scroller.content-y = clamp\(scroller.content-y - delta, -max\(0px, scroller.content-height - scroller.height\), 0px\)/);
});

test('the sign-in pages grow by the keyboard so the Sign in button can scroll above it', () => {
  const source = read('shell/signin.slint');
  assert.match(source, /Rectangle \{ height: 16px \+ max\(Device.safe-bottom, Device.keyboard\); \}/);
  assert.match(source, /padding-bottom: 24px \+ max\(Device.safe-bottom, Device.keyboard\);/);
  assert.match(source, /property <length> max-s: SigninScroll.max-scroll;\s*changed max-s => \{ root.retarget\(\); \}/);
});

test('the Analytics desktop header keeps its tools on the title row only while both fit', () => {
  const source = read('pages/analytics/ax-head.slint');
  assert.match(source, /property <bool> wide: Breakpoints.width >= 1360px\s*&& root.title-w \+ 56px \+ root.tools-w <= min\(Breakpoints.width, Breakpoints.max-content\) - 2 \* Breakpoints.gutter;/);
  assert.equal((source.match(/root.title-w = self.natural/g) || []).length, 2);
  // the wide row and the second row both report the same controls' width
  assert.equal((source.match(/root.tools-w = self.natural/g) || []).length, 4);
  assert.match(source, /property <length> natural: self.preferred-width - 14px;/);
});

test('below D the Included usage label wraps inside the column instead of widening the page', () => {
  const source = read('pages/analytics/ax-head.slint');
  assert.match(source, /in property <length> max-w: 100000px;/);
  assert.match(source, /height: Breakpoints.tier-d \? 24px : max\(44px, label.preferred-height \+ 12px\);/);
  assert.match(source, /width: Breakpoints.tier-d \? m-label.preferred-width : min\(m-label.preferred-width, root.label-max\);/);
  assert.match(source, /wrap: Breakpoints.tier-d \? TextWrap.no-wrap : TextWrap.word-wrap;/);
  assert.match(source, /inc-m := IncludedButton \{\s*max-w: mlay.cw;/);
  // the subline reserves the 44 px info button and its two 6 px gaps
  assert.match(source, /max\(48px, mlay.cw - 56px - \(AxData.head.updating \? mlay.upd-w \+ 6px : 0px\)\)/);
});

test('phone readout lines give way before the Close button', () => {
  const trend = read('pages/analytics/ax-trend.slint');
  assert.match(trend, /Text \{ text: chart.b.time; [^}]*horizontal-stretch: 1; overflow: elide; min-width: 0px;/);
  const quota = read('pages/analytics/ax-quota.slint');
  assert.match(quota, /Text \{ text: chart.st.time; [^}]*overflow: elide; min-width: 0px; \}/);
  assert.match(quota, /Text \{ text: chart.st.rows\[0\].name; [^}]*overflow: elide; min-width: 0px; \}/);
  assert.match(quota, /Rectangle \{\s*clip: true;\s*min-width: 0px;/);
});

test('the quota focus panel moves the compare switch and Details to a second row when they do not fit', () => {
  const quota = read('pages/analytics/ax-quota.slint');
  assert.match(quota, /property <bool> one-row: root.legend-w \+ 12px \+ root.tools-w <= AxQuotaCols.card-w - 68px;/);
  assert.match(quota, /if root.one-row: HorizontalLayout \{/);
  assert.match(quota, /if Breakpoints.tier-d && !root.one-row: HorizontalLayout \{/);
  const common = read('pages/analytics/ax-common.slint');
  // a long switch label elides (the tip carries it below D) instead of widening its row
  assert.match(common, /min-width: min\(row.preferred-width, root.track-w \+ 18px \+ 64px\);/);
  assert.match(common, /Text \{ text: root.label; [^}]*overflow: elide; \}/);
  assert.equal((quota.match(/tip: root.qr.focus.cmp-label;/g) || []).length, 2);
});

test('the quota table leaves room for "No reset reported" at 1360 px and below', () => {
  const quota = read('pages/analytics/ax-quota.slint');
  assert.match(quota, /out property <length> next: tight \? 106px : 110px;/);
});

test('Home paired plates fit the identity column and the header to the plate', () => {
  const home = read('pages/home.slint');
  assert.match(home, /fit-w: root.pair && s < 2 \? root.pair-fit\(s\) : 0px;/);
  assert.match(home, /pure function pair-fit\(s: int\) -> length \{\s*let w = min\(Breakpoints.width, Breakpoints.max-content\) - 2 \* Breakpoints.gutter - root.gap;/);
  assert.match(home, /ident-w: clamp\(root.fit-w > 0px \? min\(root.ident-max, root.ident-room\) : root.ident-max,/);
  assert.match(home, /- root.section.columns.length \* 108px - root.col-gap \* \(root.section.columns.length \+ 1\);/);
  assert.match(home, /overflow: self.yields \? TextOverflow.elide : TextOverflow.clip;/);
  assert.match(home, /if !root.touch: Runs \{\s*min-height: 18px;\s*elide-good: true;/);
});
