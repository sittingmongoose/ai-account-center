// Rate-limit resets as the usage reading lists them, one row per grant (or per Z.ai card kind), for the Details
// "Resets and balances" group. These rows only show what the reading saw: they carry no grant key, so nothing can be
// used from them. The frame swaps them for the reset inventory (GET /api/accounts/:id/reset-claims) when it has one.
import { clearsText } from './visible-usage.mjs';

const finite = (value) => typeof value === 'number' && Number.isFinite(value);
const text = (value) => (typeof value === 'string' ? value : '');
const ms = (value) => {
  const time = typeof value === 'string' ? Date.parse(value) : NaN;
  return Number.isFinite(time) ? time : null;
};
const count = (value) => (finite(value) && value >= 0 ? Math.floor(value) : null);

/**
 * How resets work for a provider: 'claim' (Claude and Codex: AAC can use one), 'zai' (listed only, used in Z.ai),
 * 'qwen' (the plan has none) or '' (no resets to speak of).
 */
export function resetsMode(provider) {
  if (provider === 'claude' || provider === 'codex') return 'claim';
  if (provider === 'zai') return 'zai';
  if (provider === 'qwen') return 'qwen';
  return '';
}

/** "Weekly, Fable included usage" from a grant's `clears`; '' when the reading does not say. */
function refillWords(w) {
  const words = clearsText(w).replace(/^refills /, '');
  if (!words) return '';
  const joined = words.split(' + ').join(', ');
  return joined[0].toUpperCase() + joined.slice(1);
}

const CLAUDE_GRANT = /^reset_credit_(used|expired|available|saved)_grant_\d+$/;
const CODEX_BANKED = /^banked[_-]?resets/i;
const ZAI_PACKS = /^reset-packs-(5h|weekly)$/;
const STATE = { used: 'used', expired: 'expired', available: 'ready', saved: 'saved' };

/** True for a window that becomes a reset row (or is the total those rows add up to). */
export function isResetWindow(account, w) {
  const key = text(w?.key);
  if (account?.provider === 'claude') return key === 'reset_credits_available' || CLAUDE_GRANT.test(key);
  if (account?.provider === 'codex') return CODEX_BANKED.test(key);
  if (account?.provider === 'zai') return ZAI_PACKS.test(key);
  return false;
}

/**
 * One row per reset the reading lists: `{key, refills, state, left, count, expiresAtMs}`. `state` is ready, saved
 * (listed but not usable yet), used or expired; `left` is how many uses the row still has, `count` how many the row
 * stands for. A row whose numbers can't be read is left out rather than shown as zero.
 */
export function resetRowsOf(account, windows) {
  const rows = [];
  for (const w of Array.isArray(windows) ? windows : []) {
    const key = text(w?.key);
    if (account?.provider === 'claude') {
      const match = CLAUDE_GRANT.exec(key);
      const left = count(w?.remaining);
      if (!match || left === null) continue;
      rows.push({
        key: `${account.id}|${key}`,
        refills: refillWords(w),
        state: STATE[match[1]],
        left,
        count: Math.max(1, left, count(w?.limit) ?? 0),
        expiresAtMs: ms(w?.expiresAt),
      });
    } else if (account?.provider === 'codex' && CODEX_BANKED.test(key)) {
      const left = count(w?.remaining);
      if (left === null || left === 0) continue;
      rows.push({
        key: `${account.id}|${key}`,
        refills: 'Eligible Codex usage windows',
        state: 'ready',
        left,
        count: left,
        expiresAtMs: ms(w?.expiresAt),
      });
    } else if (account?.provider === 'zai') {
      const match = ZAI_PACKS.exec(key);
      if (!match) continue;
      const refills = match[1] === '5h' ? '5-hour quota' : 'Weekly quota';
      const left = count(w?.remaining);
      const used = count(w?.used);
      if (left)
        rows.push({ key: `${account.id}|${key}`, refills, state: 'ready', left, count: left, expiresAtMs: ms(w?.expiresAt) });
      if (used)
        rows.push({ key: `${account.id}|${key}|used`, refills, state: 'used', left: 0, count: used, expiresAtMs: null });
    }
  }
  return rows;
}
