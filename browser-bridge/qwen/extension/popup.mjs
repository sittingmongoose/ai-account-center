import {ERROR_MESSAGES, CONSOLE_URLS, PREVIOUS_SAMPLE_MAX_AGE_MS, previousSample, safeError} from './bridge-core.mjs';
const region = document.getElementById('region');
const sync = document.getElementById('sync');
const statusElement = document.getElementById('status');
const windowsElement = document.getElementById('windows');
let expiryTimer;
function date(value) { const parsed = new Date(value); return Number.isFinite(parsed.getTime()) ? parsed.toLocaleString(undefined, {dateStyle: 'short', timeStyle: 'short'}) : null; }
function render(status) {
  clearTimeout(expiryTimer);
  windowsElement.replaceChildren();
  if (!status) { statusElement.textContent = 'Ready to read your saved Qwen sign-in.'; return; }
  let sample;
  if (!status.ok) {
    statusElement.textContent = ERROR_MESSAGES[safeError(status.errorCode)];
    const historical = previousSample(status, region.value);
    if (!historical) return;
    const heading = document.createElement('div'); heading.className = 'window-note';
    const consoleLabel = historical.region === 'cn' ? 'China · Alibaba Model Studio' : 'International · QwenCloud';
    heading.textContent = `Previous sample — account identity unknown. ${consoleLabel}. Sampled ${date(historical.sample.sampledAt) ?? historical.sample.sampledAt}. Historical usage; the current account has not been verified.`;
    windowsElement.append(heading);
    sample = historical.sample;
    const age = Math.max(0, Date.now() - Math.min(Date.parse(sample.sampledAt), Date.parse(sample.fetchedAt)));
    expiryTimer = setTimeout(() => render({...status, previousSample: null}), Math.max(0, PREVIOUS_SAMPLE_MAX_AGE_MS - age) + 1);
    expiryTimer?.unref?.();
  } else {
    if ((status.region ?? status.diagnostics?.region) !== region.value) {
      statusElement.textContent = ERROR_MESSAGES.context_changed;
      return;
    }
    statusElement.textContent = `Updated ${date(status.lastSyncAt) ?? 'recently'}`;
    sample = status.sample;
  }
  for (const window of sample.windows) {
    const row = document.createElement('div'); row.className = 'window';
    const heading = document.createElement('div'); heading.className = 'window-head';
    const name = document.createElement('span'); name.textContent = window.label;
    const percent = document.createElement('span'); percent.textContent = window.usedPercent === null ? '—' : `${window.usedPercent}% used`;
    heading.append(name, percent); row.append(heading);
    const parts = [];
    if (window.resetAt) parts.push(`Resets ${date(window.resetAt)}`);
    if (window.expiresAt) parts.push(`Expires ${date(window.expiresAt)}`);
    if (typeof window.remaining === 'number' && window.unit) parts.push(`${window.remaining.toLocaleString(undefined, {maximumFractionDigits: 20})} ${window.unit} left`);
    if (parts.length) { const note = document.createElement('div'); note.className = 'window-note'; note.textContent = parts.join(' · '); row.append(note); }
    if (window.usedPercent !== null) { const track = document.createElement('div'); track.className = 'track'; const fill = document.createElement('div'); fill.className = 'fill'; fill.style.width = `${Math.min(100, Math.max(0, window.usedPercent))}%`; track.append(fill); row.append(track); }
    windowsElement.append(row);
  }
}
const current = await chrome.runtime.sendMessage({type: 'status'});
region.value = current.region === 'cn' ? 'cn' : 'intl';
render(current.status);
chrome.storage.onChanged?.addListener((changes, area) => {
  if (area !== 'local') return;
  if (changes.region && Object.hasOwn(CONSOLE_URLS, changes.region.newValue)) region.value = changes.region.newValue;
  if (changes.status) render(changes.status.newValue);
});
region.addEventListener('change', async () => { await chrome.runtime.sendMessage({type: 'region', region: region.value}); render(null); });
sync.addEventListener('click', async () => {
  sync.disabled = region.disabled = true;
  statusElement.textContent = 'Reading Qwen usage…';
  try { render(await chrome.runtime.sendMessage({type: 'collect'})); }
  catch { render({ok: false, errorCode: 'host_unavailable'}); }
  finally { sync.disabled = region.disabled = false; }
});
