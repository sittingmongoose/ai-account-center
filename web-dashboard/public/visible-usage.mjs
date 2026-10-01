// Product display choices only. The source response and observed history stay intact.
const finite = value => typeof value === 'number' && Number.isFinite(value);
const validDate = value => typeof value === 'string' && Number.isFinite(Date.parse(value));
const description = window => `${window?.key || ''} ${window?.label || ''}`;
const subscription = window => /(?:^|[\s_-])(?:plan[\s_-]*)?subscription(?:$|[\s_-])/i.test(description(window));

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
    ? { ...window, expiresAt: expiration }
    : window);
}
