// Product display choices only. The source response and observed history stay intact.
const finite = value => typeof value === 'number' && Number.isFinite(value);
const validDate = value => typeof value === 'string' && Number.isFinite(Date.parse(value));
const description = window => `${window?.key || ''} ${window?.label || ''}`;
const subscription = window => /(?:^|[\s_-])(?:plan[\s_-]*)?subscription(?:$|[\s_-])/i.test(description(window));

/** The model slug of a weekly-scoped meter (`seven_day_opus` -> `opus`), or null for the aggregate windows. */
export const weeklyScopedKey = (key) => {
  const match = /^seven_day_(.+)$/.exec(String(key || ''));
  return match ? match[1] : null;
};
const fableLike = (window) => window?.key === 'seven_day_fable';
/** A weekly limit scoped to one model family (Opus, Sonnet, ...) or Fable, never the all-models `seven_day`. */
export const isWeeklyScoped = (window) => weeklyScopedKey(window?.key) !== null || fableLike(window);
const wordsOf = (value) => String(value || '').replace(/[_-]+/g, ' ').split(/\s+/).filter(Boolean);
const titleWord = (word) => (word ? word[0].toUpperCase() + word.slice(1) : word);
/** The short model name of a scoped window: from "Weekly Opus usage", else from its key slug ("seven_day_opus" -> "Opus"). */
export function scopedModelLabel(window) {
  const label = String(window?.label || '').trim();
  const match = /^weekly\s+(.+?)\s+usage$/i.exec(label);
  if (match) return wordsOf(match[1]).map(titleWord).join(' ');
  const slug = weeklyScopedKey(window?.key) || String(window?.key || '');
  return wordsOf(slug).map(titleWord).join(' ') || label || 'Usage';
}
const clearWord = (target) => {
  const name = String(target || '');
  if (name === 'five_hour') return 'session';
  if (name === 'seven_day') return 'weekly';
  // The overage bucket is the Fable included-usage allowance, never a bare Fable refill.
  if (name === 'seven_day_overage_included') return 'Fable included usage';
  // No seven_day_fable clears key exists: a grant never refills Fable itself.
  if (name === 'seven_day_fable') return '';
  const slug = weeklyScopedKey(name);
  if (slug !== null) return `${wordsOf(slug).map(titleWord).join(' ')} weekly`;
  return wordsOf(name).join(' ');
};
/** Words for a reset grant's refill targets: "refills session + weekly", "" without `clears`. */
export function clearsText(window) {
  const list = Array.isArray(window?.clears) ? window.clears : [];
  const words = list.map(clearWord).filter(Boolean);
  return words.length ? `refills ${words.join(' + ')}` : '';
}

export function visibleUsageWindows(provider, windows) {
  const rows = Array.isArray(windows) ? windows : [];
  const expiration = provider === 'qwen'
    ? rows.find(window => subscription(window) && validDate(window.expiresAt))?.expiresAt
    : undefined;
  return rows.filter(window => {
    if (provider === 'codex' && /chat[\s_-]*pass/i.test(description(window))) return false;
    if (provider === 'qwen' && subscription(window)) return false;
    if (provider === 'zai' && /packs?|reset[\s_-]*(?:cards?|credits?)/i.test(description(window))) {
      // Empty reset-pack summaries add no usable information. Genuine pack counts
      // and individual records with amounts or dates remain visible.
      const summary = /^reset-packs-(?:5h|weekly)$/.test(window.key || '');
      return ['used', 'limit', 'remaining'].some(key => finite(window[key]) && (summary ? window[key] > 0 : window[key] !== 0))
        || validDate(window.expiresAt) || validDate(window.resetAt);
    }
    return true;
  }).map(window => provider === 'qwen' && expiration && /month/i.test(description(window)) && !validDate(window.expiresAt)
    ? { ...window, expiresAt: expiration, planExpiry: true }
    : window);
}
