// D7 Home spec: inputs the dashboard's JS tests do not reach, built from the recorded ones (generate.mjs).
//
// - zone/*: the richest recorded inputs in other display zones (half-hour, 45-minute and 30-minute-DST zones), and
//   across a daylight-saving change with every timestamp moved with the clock.
// - edge/*: inputs the server can send that the tests leave out (no update time, other intervals, hidden providers
//   and accounts, offset and date-only timestamps, fractional readings, finished Opens, other computers).
// - decline/*: one input per decline rule. The JS view is still recorded (null where the JS builder throws);
//   declines.json names the word Rust must answer with, and the page then builds its own view.
// - probe/narrow and probe/safari: Node and Chrome write "Jan 5, 12:07 AM" (U+0020 before AM/PM). Other engines write
//   U+202F there (older ICU), or join the date and the time with " at " (iOS Safari: "Jan 5 at 12:07 AM"). Node writes
//   neither, so these cases' expected views are Node's view restyled (`restyle`: exactly the differences the probe
//   proves); their inputs carry no clock text of their own. The device pass checks Safari on a real Safari.

import { homeZoneFacts } from '../../../../web-dashboard/public/view-model.mjs';

const ZONES = ['UTC', 'Asia/Kolkata', 'Australia/Lord_Howe', 'Pacific/Chatham', 'America/Los_Angeles', 'Asia/Kathmandu'];
const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2}(?:\.\d{3})?)?Z$/;

const clone = (value) => JSON.parse(JSON.stringify(value));
const MONTH = '(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)';

/** Node's view (or probe) as a page with another proven formatter style writes it (`narrow` or `safari`). */
export function restyle(json, style) {
  if (style === 'narrow') return json.replace(/(\d{1,2}:\d{2}) ([AP]M)/g, '$1\u202f$2');
  if (style === 'safari') return json.replace(new RegExp(`${MONTH} (\\d{1,2}), (\\d{1,2}:\\d{2} [AP]M)`, 'g'), '$1 $2 at $3');
  throw new Error(`unknown style ${style}`);
}
/** The probe a page with that style sends. */
export const styledProbe = (style) => JSON.parse(restyle(JSON.stringify(probeIn('en-US')), style));
const providers = (input) =>
  new Set((Array.isArray(input.dashboard?.accounts) ? input.dashboard.accounts : []).map((a) => a?.provider));
const size = (input) => JSON.stringify(input.dashboard).length;

/** Every UTC ISO timestamp in `value` moved by `delta` ms (the same spelling `toISOString` writes). */
function shifted(value, delta) {
  if (typeof value === 'string') return ISO.test(value) ? new Date(Date.parse(value) + delta).toISOString() : value;
  if (Array.isArray(value)) return value.map((item) => shifted(item, delta));
  if (value && typeof value === 'object')
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, shifted(item, delta)]));
  return value;
}

/** view-model.mjs `homeFormatProbe` written by another locale's formatters. */
function probeIn(locale) {
  const utc = (options) => new Intl.DateTimeFormat(locale, { ...options, timeZone: 'UTC' });
  const first = Date.UTC(2026, 0, 5, 0, 7);
  const second = Date.UTC(2026, 0, 5, 13, 7);
  return [
    new Intl.NumberFormat(locale, { maximumFractionDigits: 2 }).format(-1234.567),
    new Intl.NumberFormat(locale, { notation: 'compact', maximumFractionDigits: 2 }).format(1234567),
    new Intl.NumberFormat(locale, { style: 'currency', currency: 'USD', maximumFractionDigits: 2 }).format(-1234.5),
    utc({ month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' }).format(first),
    utc({ hour: 'numeric', minute: '2-digit' }).format(second),
    utc({ weekday: 'short', month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' }).format(second),
    utc({ month: 'short', day: 'numeric' }).format(first),
    ['b', 'a', 'B', '_'].sort((a, b) => a.localeCompare(b, locale)).join(''),
  ];
}

/** A copy of `base` named `name`, with `edit` applied to a deep copy of its inputs. */
function variant(base, name, edit = () => {}) {
  const input = { name, source: 'synthetic', ...clone({ dashboard: base.dashboard, profiles: base.profiles, antigravity: base.antigravity, ctx: base.ctx }) };
  edit(input);
  return input;
}

const accountsOf = (input) => input.dashboard.accounts;
const firstOf = (input, provider) => accountsOf(input).find((a) => a?.provider === provider);

export function synthetic(recorded) {
  const usable = recorded.filter((input) => Array.isArray(input.dashboard?.accounts) && input.dashboard.accounts.length);
  // The richest inputs: most providers first, then the biggest answer; one per provider mix.
  const ranked = [...usable].sort(
    (a, b) => providers(b).size - providers(a).size || size(b) - size(a) || a.source.localeCompare(b.source)
  );
  const bases = [];
  for (const input of ranked) {
    const mix = [...providers(input)].sort().join(',');
    if (!bases.some((b) => [...providers(b)].sort().join(',') === mix)) bases.push(input);
    if (bases.length === 4) break;
  }
  const antigravity = [...usable]
    .filter((input) => input.antigravity && providers(input).has('antigravity'))
    .sort((a, b) => size(b) - size(a))[0];
  if (antigravity && !bases.includes(antigravity)) bases.push(antigravity);
  const claude = [...usable].filter((input) => providers(input).has('claude')).sort((a, b) => size(b) - size(a))[0];
  if (claude && !bases.includes(claude)) bases.push(claude);
  const rich = bases[0];
  const out = [];

  bases.forEach((base, i) => {
    for (const zone of ZONES) out.push(variant(base, `zone/${zone}#${i + 1}`, (input) => (input.ctx.timeZone = zone)));
  });
  // Daylight-saving changes, with the whole answer moved so its readings stay as old as they were.
  for (const [zone, at] of [
    ['America/New_York', Date.UTC(2026, 10, 1, 5, 40)],
    ['Europe/London', Date.UTC(2026, 2, 29, 0, 50)],
    ['Australia/Lord_Howe', Date.UTC(2026, 3, 4, 14, 50)],
  ])
    bases.forEach((base, i) =>
      out.push(
        variant(base, `zone/dst-${zone}#${i + 1}`, (input) => {
          const delta = at - input.ctx.now;
          input.dashboard = shifted(input.dashboard, delta);
          input.profiles = shifted(input.profiles, delta);
          input.antigravity = shifted(input.antigravity, delta);
          input.ctx.now = at;
          input.ctx.timeZone = zone;
        })
      )
    );

  out.push(variant(rich, 'edge/no-update-time', (input) => delete input.dashboard.updatedAt));
  out.push(variant(rich, 'edge/update-time-null', (input) => (input.dashboard.updatedAt = null)));
  out.push(variant(rich, 'edge/just-updated', (input) => (input.dashboard.updatedAt = new Date(input.ctx.now - 5000).toISOString())));
  out.push(variant(rich, 'edge/updated-days-ago', (input) => (input.dashboard.updatedAt = new Date(input.ctx.now - 3 * 86_400_000 - 7 * 3_600_000).toISOString())));
  for (const seconds of [30, 45, 90, 3600])
    out.push(variant(rich, `edge/interval-${seconds}`, (input) => (input.ctx.intervalSeconds = seconds)));
  out.push(
    variant(rich, 'edge/user-and-host', (input) => {
      input.ctx.username = 'fixture-user';
      input.ctx.host = 'dashboard.example.test';
    })
  );
  out.push(variant(rich, 'edge/windows-page', (input) => (input.ctx.platform = 'windows')));
  out.push(
    variant(rich, 'edge/hidden', (input) => {
      const [first, second] = accountsOf(input).filter((a) => a && typeof a === 'object');
      input.dashboard.settings = { ...input.dashboard.settings, hiddenProviders: ['cursor', 'not-a-provider', 7], hiddenAccountIds: [first.id] };
      if (second) second.hidden = true;
    })
  );
  out.push(
    variant(rich, 'edge/hidden-all-sections', (input) => {
      input.dashboard.settings = { ...input.dashboard.settings, hiddenProviders: ['claude', 'codex', 'antigravity'] };
    })
  );
  out.push(variant(rich, 'edge/no-accounts', (input) => (input.dashboard.accounts = [])));
  out.push(variant(rich, 'edge/primitive-accounts', (input) => input.dashboard.accounts.push(3, 'x', true)));
  if (antigravity) out.push(variant(antigravity, 'edge/no-inventory', (input) => (input.antigravity = null)));
  out.push(
    variant(rich, 'edge/offset-and-short-times', (input) => {
      accountsOf(input).forEach((a, i) => {
        if (!a || typeof a !== 'object') return;
        const t = Date.parse(a.sampledAt);
        if (Number.isFinite(t) && i % 2 === 0) {
          const local = new Date(t + 5.5 * 3_600_000).toISOString().slice(0, 19);
          a.sampledAt = `${local}+05:30`;
        } else if (Number.isFinite(t)) a.sampledAt = new Date(t).toISOString().slice(0, 19) + 'Z';
        for (const w of Array.isArray(a.windows) ? a.windows : [])
          if (w && typeof w.resetAt === 'string' && Number.isFinite(Date.parse(w.resetAt)))
            w.resetAt = new Date(Date.parse(w.resetAt)).toISOString().slice(0, 16) + 'Z';
      });
    })
  );
  // a date-only time is UTC by the spec, but the envelope has no clock or day words for it
  out.push(
    variant(rich, 'decline/date-only', (input) => {
      const a = accountsOf(input).find((x) => x && typeof x === 'object');
      a.sampledAt = new Date(input.ctx.now - 86_400_000).toISOString().slice(0, 10);
    })
  );
  const FRACTIONS = [33.333333333333336, 0.000001, 99.995, 100.5, 1e-7, 12.345, 79.99999, 95, 0];
  out.push(
    variant(rich, 'edge/fractional-readings', (input) => {
      let n = 0;
      for (const a of accountsOf(input))
        for (const w of Array.isArray(a?.windows) ? a.windows : [])
          if (w && typeof w.usedPercent === 'number') {
            w.usedPercent = FRACTIONS[n++ % FRACTIONS.length];
            w.remainingPercent = 100 - w.usedPercent;
          }
    })
  );
  // Big review M4/L4: amounts whose shortest round-trip digits tie. JS writes 100000000000000.12, 140737488355327.12
  // and 2251799813685247.2 where Rust's own formatter wrote .13, .13 and .3; outside Status's proved number domain the
  // Rust view declines, neighbouring doubles included. A Cursor spend/usage card and a Z.ai balance card show them.
  const neighbours = (x) => {
    const bits = new BigInt64Array(new Float64Array([x]).buffer)[0];
    return [bits - 1n, bits, bits + 1n].map((b) => new Float64Array(new BigInt64Array([b]).buffer)[0]);
  };
  const amountCards = (input, value, limit) => {
    input.dashboard.settings = { ...(input.dashboard.settings ?? {}), hiddenProviders: [], hiddenAccountIds: [] };
    const base = { platform: 'mac', status: 'ok', plan: 'pro', fetchedAt: input.dashboard.updatedAt ?? null };
    input.dashboard.accounts = [
      { ...base, id: 'cursor:fixture', provider: 'cursor', providerLabel: 'Cursor', label: 'Cursor', windows: [
        { key: 'fixture-spend', label: 'Spend', kind: 'spend', used: value, limit, unit: 'USD' },
        { key: 'fixture-rate', label: 'Usage', usedPercent: value },
      ] },
      { ...base, id: 'zai:fixture', provider: 'zai', providerLabel: 'Z.ai Coding Plan', label: 'Z.ai Coding Plan', windows: [
        { key: 'fixture-balance', label: 'Balance', kind: 'balance', remaining: value, used: 0, limit: null, unit: 'USD' },
        { key: 'fixture-credits', label: 'Credits', kind: 'balance', remaining: value, used: 0, limit: null, unit: 'credits' },
      ] },
    ];
  };
  for (const [label, x] of [['1e14', 100000000000000.12], ['2^47', 140737488355327.12], ['2^51', 2251799813685247.2]])
    neighbours(x).forEach((value, i) =>
      out.push(variant(rich, `decline/number-${label}#${i}`, (input) => amountCards(input, value, value * 2)))
    );
  out.push(variant(rich, 'edge/number-control', (input) => amountCards(input, 1234.56, 2469.12)));
  // edge/fractional-readings now declines on 100/3 (outside the proved number domain, big review M4); this
  // companion keeps fractional readings served, with values the domain proves and their exact remainders.
  const PROVED = [[0.000001, 99.999999], [99.995, 0.005], [100.5, 0], [1e-7, 99.9999999], [12.345, 87.655], [79.99999, 20.00001], [95, 5], [0, 100]];
  out.push(
    variant(rich, 'edge/fractional-readings-proved', (input) => {
      let n = 0;
      for (const a of accountsOf(input))
        for (const w of Array.isArray(a?.windows) ? a.windows : [])
          if (w && typeof w.usedPercent === 'number') {
            [w.usedPercent, w.remainingPercent] = PROVED[n++ % PROVED.length];
          }
    })
  );
  out.push(
    variant(rich, 'edge/other-computers', (input) => {
      const words = [
        ['nas1', 'linux'],
        [undefined, 'linux'],
        [undefined, 'freebsd'],
        ['alexis', 'windows'],
        ['', 'mac'],
      ];
      accountsOf(input).forEach((a, i) => {
        if (!a || typeof a !== 'object') return;
        const [host, platform] = words[i % words.length];
        if (host === undefined) delete a.host;
        else a.host = host;
        a.platform = platform;
      });
    })
  );
  if (claude) {
    for (const state of ['opened', 'failed', 'blocked_uncertain'])
      out.push(
        variant(claude, `edge/open-${state}`, (input) => {
          input.profiles = input.profiles.length
            ? input.profiles.map((p, i) => (i === 0 ? { ...p, openOperation: { id: 'op-1', state, platform: 'mac' } } : p))
            : [{ id: firstOf(input, 'claude')?.capabilities?.claudeProfileId || 'a', name: 'Fixture', openOperation: { id: 'op-1', state, platform: 'mac' } }];
        })
      );
  }

  out.push(variant(rich, 'decline/locale', (input) => (input.probe = probeIn('de-DE'))));
  out.push(
    variant(rich, 'decline/date', (input) => {
      const a = accountsOf(input).find((x) => x && typeof x === 'object');
      // V8 reads this spelling (in its own local time); it is not the ECMAScript format, so Rust declines.
      a.sampledAt = 'Oct 1, 2026 11:15 AM';
    })
  );
  if (claude)
    out.push(
      variant(claude, 'decline/open-progress', (input) => {
        const id = firstOf(input, 'claude')?.capabilities?.claudeProfileId || 'a';
        input.profiles = [{ id, name: 'Fixture', openOperation: { id: 'op-2', state: 'copying', platform: 'mac', confirmedCount: 1, totalCount: 4 } }];
      })
    );
  // localeCompare ranks come from the envelope for printable ASCII (every ICU orders those alike): underscore,
  // hyphen and case ties build; a non-ASCII pair is engine-dependent and declines.
  const sameProviderPair = (name, ids) =>
    variant(rich, name, (input) => {
      const base = firstOf(input, 'cursor') ?? accountsOf(input).find((x) => x && typeof x === 'object');
      input.dashboard.accounts.push(...ids.map((id) => ({ ...clone(base), provider: 'cursor', id })));
    });
  out.push(sameProviderPair('edge/ascii-collation', ['cursor:a_1', 'cursor:a-1', 'cursor:A-1', 'cursor:a.1']));
  out.push(sameProviderPair('decline/collation', ['cursor:\u00e9', 'cursor:e']));
  out.push(
    variant(rich, 'decline/proto-key', (input) => {
      const a = accountsOf(input).find((x) => x && typeof x === 'object');
      delete a.host;
      a.platform = 'constructor';
    })
  );
  out.push(
    variant(rich, 'decline/case-map', (input) => {
      const a = accountsOf(input).find((x) => x && typeof x === 'object');
      delete a.host;
      a.platform = 'ñandu';
    })
  );
  out.push(variant(rich, 'decline/type', (input) => (accountsOf(input).find((x) => x && typeof x === 'object').email = 5)));
  out.push(variant(rich, 'decline/shape-null-account', (input) => input.dashboard.accounts.push(null)));
  out.push(variant(rich, 'decline/shape-no-accounts', (input) => delete input.dashboard.accounts));
  // Big review (Fable M2): Node (the page here, tzdata 2026e) has Morocco on +00:00 from 2026-09-20 while Bun (the
  // envelope's words) keeps +01:00. The page's own zone facts decide: those words decline (`zone`). Before that date
  // both engines agree and the view is served. Facts that are not this engine's, or that end before an instant, decline.
  const moved = (base, name, zone, at) =>
    variant(base, name, (input) => {
      const delta = at - input.ctx.now;
      input.dashboard = shifted(input.dashboard, delta);
      input.profiles = shifted(input.profiles, delta);
      input.antigravity = shifted(input.antigravity, delta);
      input.ctx.now = at;
      input.ctx.timeZone = zone;
    });
  for (const zone of ['Africa/Casablanca', 'Africa/El_Aaiun']) {
    out.push(moved(rich, `decline/zone-${zone}`, zone, Date.UTC(2026, 9, 1, 15, 16)));
    out.push(moved(rich, `zone/agree-${zone}`, zone, Date.UTC(2026, 5, 1, 12, 0)));
  }
  out.push(
    variant(rich, 'decline/zone-facts', (input) => {
      const facts = homeZoneFacts(input.ctx.timeZone, input.ctx.now);
      input.ctx.zone = { ...facts, offsets: facts.offsets.map(([start, minutes]) => [start, minutes + 60]) };
    })
  );
  out.push(
    variant(rich, 'decline/zone-window', (input) => {
      const facts = homeZoneFacts(input.ctx.timeZone, input.ctx.now);
      const to = input.ctx.now - 86_400_000;
      input.ctx.zone = { ...facts, to, offsets: facts.offsets.filter(([start]) => start <= to) };
    })
  );
  // the envelope without its display-zone clock and day words: a missing fact refuses
  out.push(variant(rich, 'decline/facts', (input) => (input.facts = 'no-dates')));

  for (const style of ['narrow', 'safari'])
    for (const [base, suffix] of [[rich, ''], [antigravity, '#ag'], [claude, '#claude']].filter(([b]) => b))
      out.push(
        variant(base, `probe/${style}${suffix}`, (input) => {
          input.probe = styledProbe(style);
          input.style = style;
        })
      );
  return out;
}
