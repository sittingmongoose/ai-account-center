#!/usr/bin/env node
// D7 Home fuzz: seeded random edits of every case in cases.json, each run through the real view-model.mjs. Not a
// committed table: it writes a temporary one that the Rust spec test reads when HOME_SPEC_FUZZ names it, and there
// any decline is allowed but a built view must still be byte-equal to the JS view (or the JS must not throw).
//
//   node tests/spec/views/home/fuzz.mjs <out.json> [--seed N] [--per-case N]
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

process.env.TZ = 'UTC';
const here = path.dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const out = args.find((a) => !a.startsWith('--') && !/^\d+$/.test(a));
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? Number(args[i + 1]) : fallback;
};
const seed = flag('--seed', 1);
const perCase = flag('--per-case', 12);
if (!out) throw new Error('usage: fuzz.mjs <out.json> [--seed N] [--per-case N]');

// mulberry32
let state = seed >>> 0;
const random = () => {
  state = (state + 0x6d2b79f5) >>> 0;
  let t = state;
  t = Math.imul(t ^ (t >>> 15), t | 1);
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
};
const pick = (list) => list[Math.floor(random() * list.length)];

const { cases } = JSON.parse(readFileSync(path.join(here, 'cases.json'), 'utf8'));
process.removeAllListeners('warning');
const { presentations } = await import('./generate.mjs');
const { restyle } = await import('./synthetic.mjs');
const { dashboardViewModel, homeZoneFacts } = await import('../../../../web-dashboard/public/view-model.mjs');
const { setDisplayTimeZone } = await import('../../../../web-dashboard/public/time-format.mjs');

const ACCOUNT_FIELDS = ['id', 'provider', 'providerLabel', 'plan', 'email', 'platform', 'status', 'fetchedAt', 'sampledAt', 'windows', 'antigravityPlan', 'capabilities', 'label', 'source', 'message', 'isActive', 'hidden', 'signInNeeded', 'host'];
const WINDOW_FIELDS = ['key', 'label', 'kind', 'usedPercent', 'remainingPercent', 'windowMinutes', 'resetAt', 'status', 'cached', 'used', 'limit', 'remaining', 'unit', 'clears', 'expiresAt', 'enabled', 'sampledAt'];
const VOCAB = {
  provider: ['claude', 'codex', 'antigravity', 'cursor', 'muse', 'kimi-code', 'qwen', 'zai', 'opencode-go', 'other'],
  status: ['ok', 'unavailable', 'cached', 'needs_sign_in', 'error', 'stale', 'signed_out', 'rate_limited', ''],
  plan: ['pro', 'plus', 'max', 'max_20x', 'max_5x', 'free', 'team', 'enterprise', 'ultra', 'pro_plus', 'Google AI Pro', 'Reported plan', 'MAX', 'Pro', ''],
  key: ['five_hour', 'seven_day', 'seven_day_opus', 'seven_day_sonnet', 'seven_day_fable', 'seven_day_fable_code', 'weekly', 'monthly', '5h', 'plan-reported', 'autoPercentUsed', 'apiPercentUsed', 'credits_balance', 'banked_resets', 'banked_resets_0', 'reset-packs-5h', 'reset-packs-weekly', 'addon-pack-1', 'gemini-weekly', 'gemini-5h', '3p-weekly', 'third-party-spend', 'subscription', 'session_reset_allowance', 'extra_additional_1', 'usage-2', ''],
  label: ['Weekly usage', '5-hour', 'Monthly', 'Included usage', 'Gemini Models · Weekly', 'Credits', 'Reset packs', 'Chat pass', 'Add-on pack', 'Spend', 'Weekly Opus usage', ''],
  kind: ['rate_limit', 'spend', 'balance', 'other'],
  unit: ['USD', 'credits', 'resets', 'packs', 'requests', 'tokens', ''],
  platform: ['ubuntu', 'mac', 'windows', 'linux', 'nas1', 'alexis', 'freebsd', ''],
  source: ['Native quota', 'Cursor desktop', 'Browser extension', ''],
};
const NUMBERS = [0, -0, 1, -1, 0.5, 1.5, 2, 5, 10, 33.333333333333336, 50, 79.99, 80, 94.999, 95, 99.995, 100, 100.5, 120, 1000, 1234.5678, 62500, 999999, 1234567, 1e9, 1e21, 1e-7, -3.25];

function near(now) {
  const unit = pick([1000, 60_000, 3_600_000, 86_400_000]);
  const t = now + Math.round((random() * 2 - 1) * pick([1, 3, 10, 40, 400])) * unit;
  return pick([
    new Date(t).toISOString(),
    new Date(t).toISOString().slice(0, 16) + 'Z',
    new Date(t).toISOString().slice(0, 19) + 'Z',
    new Date(t).toISOString().slice(0, 10),
    `${new Date(t + 19_800_000).toISOString().slice(0, 19)}+05:30`,
  ]);
}

function value(field, now) {
  const roll = random();
  if (roll < 0.08) return undefined;
  if (roll < 0.14) return null;
  if (roll < 0.17) return pick([true, false, [], {}, 'x', 7]);
  if (/At$/.test(field) || field === 'expiresAt' || field === 'clears') return roll < 0.25 ? 'garbage' : near(now);
  if (VOCAB[field]) return pick(VOCAB[field]);
  if (['usedPercent', 'remainingPercent', 'used', 'limit', 'remaining', 'windowMinutes'].includes(field)) return pick(NUMBERS);
  if (['isActive', 'hidden', 'cached', 'enabled', 'signInNeeded'].includes(field)) return random() < 0.5;
  if (field === 'email') return pick(['a@example.test', 'B.Name@Example.test', 'no-at-sign', '']);
  if (field === 'id') return pick(['claude:z', 'codex:z', 'cursor:b', 'cursor:B', 'cursor:a1', 'cursor:a10', 'x']);
  return pick(['', 'x', 'Some words', '0']);
}

function set(object, field, v) {
  if (v === undefined) delete object[field];
  else object[field] = v;
}

// Display zones whose rules this engine (the page) and Bun (the envelope) may set differently: Africa/Casablanca and
// Africa/El_Aaiun differ from 2026-09-20 (big review, Fable M2); the page's zone facts always come from this engine.
const FUZZ_ZONES = ['Africa/Casablanca', 'Africa/El_Aaiun', 'America/New_York', 'Australia/Lord_Howe', 'Asia/Kolkata', 'UTC'];

function mutate(input) {
  const accounts = Array.isArray(input.dashboard.accounts) ? input.dashboard.accounts : [];
  const objects = accounts.filter((a) => a && typeof a === 'object');
  const now = input.ctx.now;
  if (random() < 0.15) {
    input.ctx.timeZone = pick(FUZZ_ZONES);
    input.ctx.zone = homeZoneFacts(input.ctx.timeZone, now);
  }
  const edits = 1 + Math.floor(random() * 3);
  for (let n = 0; n < edits; n++) {
    const roll = random();
    const a = objects.length ? pick(objects) : null;
    const windows = a && Array.isArray(a.windows) ? a.windows.filter((w) => w && typeof w === 'object') : [];
    if (roll < 0.45 && windows.length) {
      const field = pick(WINDOW_FIELDS);
      set(pick(windows), field, value(field, now));
    } else if (roll < 0.8 && a) {
      const field = pick(ACCOUNT_FIELDS.filter((f) => f !== 'windows' && f !== 'capabilities'));
      set(a, field, value(field, now));
    } else if (roll < 0.86 && a) {
      a.windows = [...windows, { ...structuredClone(windows[0] ?? {}), key: pick(VOCAB.key), label: pick(VOCAB.label) }];
    } else if (roll < 0.9 && a) {
      accounts.push({ ...structuredClone(a), id: `${a.id}-copy`, provider: pick(VOCAB.provider) });
    } else if (roll < 0.94) {
      input.dashboard.updatedAt = value('updatedAt', now);
    } else if (roll < 0.97) {
      input.dashboard.settings = { ...(input.dashboard.settings ?? {}), hiddenProviders: [pick(VOCAB.provider)] };
    } else {
      input.ctx.intervalSeconds = pick([30, 59, 61, 600, 3599]);
    }
  }
}

const fuzzed = [];
for (const base of cases) {
  for (let k = 0; k < perCase; k++) {
    const edited = structuredClone({ dashboard: base.dashboard, profiles: base.profiles, antigravity: base.antigravity, ctx: base.ctx });
    if (!edited.dashboard || typeof edited.dashboard !== 'object') continue;
    mutate(edited);
    // Both builders read the server's JSON (which never writes -0 or undefined): the JS view is built from that too.
    const input = JSON.parse(JSON.stringify(edited));
    if (!setDisplayTimeZone(input.ctx.timeZone)) continue;
    let expected = null;
    try {
      expected = JSON.stringify(
        dashboardViewModel(input.dashboard, {
          now: input.ctx.now,
          profiles: input.profiles,
          platform: input.ctx.platform,
          antigravityInventory: input.antigravity,
          intervalSeconds: input.ctx.intervalSeconds,
          username: input.ctx.username,
          host: input.ctx.host,
        })
      );
    } catch {
      /* the JS throws: Rust must decline */
    }
    if (expected !== null && base.style) expected = restyle(expected, base.style);
    fuzzed.push({
      name: `fuzz/${base.name}/${k}`,
      ...input,
      presentation: null,
      expected,
    });
  }
}
// the front door's envelope facts for every fuzzed input, in one Bun call
const facts = presentations(fuzzed.map(({ ctx, dashboard, profiles, antigravity }) => ({ ctx, dashboard, profiles, antigravity })));
fuzzed.forEach((c, i) => (c.presentation = facts[i]));
writeFileSync(out, JSON.stringify({ version: 1, cases: fuzzed }));
console.log(`[OK] ${fuzzed.length} fuzzed Home cases (seed ${seed}) -> ${out}`);
