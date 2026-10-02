import test from 'node:test';
import assert from 'node:assert/strict';

process.env.TZ = 'UTC';
const { quotaView, agendaView, focusChart, quotaAccounts, project, historyOf } = await import('../public/analytics-quota.mjs');

const now = Date.parse('2026-10-01T12:00:00Z');
const iso = t => new Date(t).toISOString();
const H = 3_600_000;
const point = (t, usedPercent, extra = {}) => ({ sampledAt: iso(t), observedAt: iso(t), usedPercent, remainingPercent: usedPercent === null ? null : 100 - usedPercent, used: null, limit: null, remaining: null, resetAt: iso(now + 48 * H), expiresAt: null, status: 'ok', source: 'Native quota', platform: 'ubuntu', isActive: false, ...extra });
const win = (key, label, usedPercent, extra = {}) => ({ key, label, kind: 'rate_limit', usedPercent, remainingPercent: usedPercent === null ? null : 100 - usedPercent, used: null, limit: null, remaining: null, unit: null, windowMinutes: key === 'five_hour' ? 300 : 10080, resetAt: iso(now + 48 * H), expiresAt: null, ...extra });
const account = (id, provider, email, windows, extra = {}) => ({ id, provider, providerLabel: provider, email, label: email, plan: 'Pro', platform: 'ubuntu', status: 'ok', source: 'Native quota', isActive: false, windows, ...extra });

// history: codex-b climbs 40 -> 60, has a missing sample, then the weekly window resets and starts again at 5
const resetAt = now - 6 * H;
const codexBHistory = [point(now - 10 * H, 40, { resetAt: iso(resetAt) }), point(now - 9 * H, null, { status: 'unavailable', resetAt: iso(resetAt) }), point(now - 8 * H, 60, { resetAt: iso(resetAt), isActive: true }), point(now - 5 * H, 5, { isActive: true }), point(now - 2 * H, 8, { isActive: true })];
const analytics = {
  history: { oldestSampleAt: iso(now - 12 * H) },
  accounts: [
    account('codex:b', 'codex', 'b@example.test', [{ ...win('seven_day', 'Weekly usage', 9), points: codexBHistory }]),
    account('codex:a', 'codex', 'a@example.test', [{ ...win('seven_day', 'Weekly usage', 97), points: [point(now - 6 * H, 96), point(now - 1 * H, 97)] }]),
    account('claude:max', 'claude', 'm@example.test', [{ ...win('seven_day', 'Weekly usage', 20), points: [point(now - 3 * H, 20)] }, { ...win('seven_day_fable', 'Weekly Fable usage', 90), points: [point(now - 3 * H, 90)] }], { plan: 'max' }),
    account('kimi-code:k', 'kimi-code', 'k@example.test', [{ ...win('weekly', 'Weekly', 1), points: [point(now - 3 * H, 1)] }]),
  ],
};
const dashboard = {
  accounts: [
    account('codex:a', 'codex', 'a@example.test', [win('seven_day', 'Weekly usage', 97)]),
    account('codex:b', 'codex', 'b@example.test', [win('seven_day', 'Weekly usage', 9, { resetAt: iso(now + 100 * H) }), win('five_hour', '5-hour', null)], { isActive: true }),
    account('claude:max', 'claude', 'm@example.test', [win('seven_day', 'Weekly usage', 20, { resetAt: iso(now + 30 * H) }), win('seven_day_fable', 'Weekly Fable usage', 90, { resetAt: iso(now + 30 * H) }),
      { key: 'prepaid', label: 'Additional credit pack', kind: 'balance', unit: 'credits', remaining: 20000, limit: 20000, used: 0, expiresAt: iso(now + 26 * H) },
      { key: 'spent', label: 'Additional credit pack 2', kind: 'balance', unit: 'credits', remaining: 0, limit: 100, used: 100, expiresAt: iso(now + 26 * H) }], { plan: 'max' }),
    account('kimi-code:k', 'kimi-code', 'k@example.test', [win('weekly', 'Weekly', null, { resetAt: iso(now + 3 * H) })]),
  ],
};

test('quota history: one row per account by identity, the main window never Fable, missing readings unavailable', () => {
  const view = quotaView(analytics, { now, dashboard });
  assert.deepEqual(view.groups.map(g => g.provider), ['claude', 'codex', 'kimi-code']);
  const codex = view.groups.find(g => g.provider === 'codex');
  // ordered by identity: the active account is not moved first
  assert.deepEqual(codex.rows.map(r => r.id), ['codex:a', 'codex:b']);
  assert.equal(codex.rows[1].active, true);
  assert.equal(codex.meta, '2 accounts · weekly window');
  const claude = view.groups.find(g => g.provider === 'claude').rows[0];
  assert.equal(claude.valueText, '20');
  const kimi = view.groups.find(g => g.provider === 'kimi-code').rows[0];
  assert.equal(kimi.hasValue, false);
  assert.equal(kimi.valueText, '');
  assert.equal(kimi.next, 'in 3h 0m');
  assert.equal(codex.rows[0].sev, 2);
});

test('sparklines are the window\'s own steps: gaps stay gaps and a reset starts a fresh run', () => {
  const row = quotaView(analytics, { now, dashboard }).groups.find(g => g.provider === 'codex').rows.find(r => r.id === 'codex:b');
  assert.equal(row.points, 4);
  // 40 then a gap: the first run ends at the unavailable sample and the next run starts afresh
  const calmRuns = row.calmLine.match(/M/g).length;
  assert.ok(calmRuns >= 3);
  assert.equal(row.na, '');
  // history is merged by key but never across accounts
  assert.equal(historyOf(analytics.accounts[0], 'seven_day').filter(p => p.v !== null).length, 4);
});

test('hidden providers are left out of the quota history and the agenda', () => {
  const hidden = new Set(['kimi-code']);
  assert.equal(quotaView(analytics, { now, dashboard, hidden }).groups.some(g => g.provider === 'kimi-code'), false);
  const agenda = agendaView(analytics, { now, dashboard, hidden });
  assert.equal([...agenda.a, ...agenda.b].some(r => r.provider === 'kimi-code'), false);
});

test('focus chart labels never overlap each other and stay inside the chart', () => {
  const accounts = quotaAccounts(analytics, { now, dashboard });
  for (const [id, compare] of [['codex:b', false], ['codex:b', true], ['claude:max', false]]) {
    const f = focusChart(accounts.find(a => a.id === id), analytics, { now, width: 900, height: 270, compare, thresholds: { codex: 95 }, accounts });
    for (const l of f.labels) {
      assert.ok(l.px >= -0.5 && l.px + l.pw <= f.w + 0.5 && l.py >= -0.5 && l.py + l.ph <= f.h + 0.5, `${l.text} inside`);
    }
    for (let i = 0; i < f.labels.length; i++) for (let j = i + 1; j < f.labels.length; j++) {
      const a = f.labels[i], b = f.labels[j];
      const overlap = a.px < b.px + b.pw && a.px + a.pw > b.px && a.py < b.py + b.ph && a.py + a.ph > b.py;
      assert.equal(overlap, false, `${a.text} / ${b.text}`);
    }
    assert.ok(f.lut.every(i => i >= 0 && i < f.stops.length));
  }
});

test('focus charts: the threshold only for switchable providers, compare overlays without adding, Fable stays its own series', () => {
  const accounts = quotaAccounts(analytics, { now, dashboard });
  const codexB = accounts.find(a => a.id === 'codex:b');
  const one = focusChart(codexB, analytics, { now, width: 900, height: 270, compare: false, thresholds: { codex: 95 }, accounts });
  assert.ok(one.labels.some(l => l.text === 'AUTO-SWITCH 95% USED'));
  assert.ok(one.legend.some(l => l.kind === 'active'));
  const cmp = focusChart(codexB, analytics, { now, width: 900, height: 270, compare: true, thresholds: { codex: 95 }, accounts });
  assert.equal(cmp.legend.filter(l => l.kind === 'series').length, 2);
  assert.match(cmp.foot, /never added together/);
  const claude = focusChart(accounts.find(a => a.id === 'claude:max'), analytics, { now, width: 900, height: 270, thresholds: { codex: 95 }, accounts });
  assert.equal(claude.labels.some(l => l.text.startsWith('AUTO-SWITCH')), false);
  assert.deepEqual(claude.legend.filter(l => l.kind === 'series').map(l => l.name), ['Fable weekly', 'Weekly usage']);
});

test('projections come only from readings of the current reset period', () => {
  const accounts = quotaAccounts(analytics, { now, dashboard });
  const b = accounts.find(a => a.id === 'codex:b');
  const p = project(b, b.windows.find(w => w.key === 'seven_day'), now);
  // only two post-reset readings in the same reset period: too little data
  assert.equal(p.kind, 'thin');
  assert.equal(project(b, b.windows.find(w => w.key === 'five_hour'), now).kind, 'na');
});

test('the agenda merges same-minute resets, names expiries with what is left and drops spent packs', () => {
  const agenda = agendaView(analytics, { now, dashboard });
  const rows = [...agenda.a, ...agenda.b];
  const lines = rows.filter(r => r.kind === 'line');
  const merged = lines.find(r => r.who === 'm@example.test' && /limits reset/.test(r.what));
  assert.equal(merged.what, 'Weekly and Fable limits reset');
  assert.equal(merged.now.map(r => r.text).join(''), 'now 20% and 90% used');
  const pack = lines.find(r => r.what === 'Credit pack expires');
  assert.equal(pack.now.map(r => r.text).join(''), '20,000 credits left');
  assert.equal(lines.filter(r => r.what === 'Credit pack expires').length, 1);
  assert.deepEqual(rows.filter(r => r.kind === 'day').map(r => r.day), ['Today', 'Tomorrow', 'Sat, Oct 3', 'Mon, Oct 5']);
  const kimi = lines.find(r => r.who === 'k@example.test');
  assert.equal(kimi.rel, 'in 3h 0m');
  assert.deepEqual(kimi.now, []);
});

test('F6: a current reading from before a passed reset is no current reading in the quota history', () => {
  const stale = account('codex:stale', 'codex', 'stale@example.test', [win('seven_day', 'Weekly usage', 97, { resetAt: iso(now - H) })], { sampledAt: iso(now - 2 * H) });
  const fresh = account('codex:fresh', 'codex', 'fresh@example.test', [win('seven_day', 'Weekly usage', 4, { resetAt: iso(now - H) })], { sampledAt: iso(now - 0.5 * H) });
  const [a, b] = quotaAccounts({ accounts: [] }, { now, dashboard: { accounts: [stale, fresh] } });
  assert.equal(a.windows[0].used, null);
  assert.equal(b.windows[0].used, 4);
  assert.equal(project(a, a.windows[0], now).kind, 'na');
});
