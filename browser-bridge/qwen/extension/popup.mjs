import {ERROR_MESSAGES, safeError} from './bridge-core.mjs';
const region = document.getElementById('region');
const sync = document.getElementById('sync');
const statusElement = document.getElementById('status');
const windowsElement = document.getElementById('windows');
function date(value) { const parsed = new Date(value); return Number.isFinite(parsed.getTime()) ? parsed.toLocaleString(undefined, {dateStyle: 'short', timeStyle: 'short'}) : null; }
function render(status) {
  windowsElement.replaceChildren();
  if (!status) { statusElement.textContent = 'Ready to read your saved Qwen sign-in.'; return; }
  if (!status.ok) { statusElement.textContent = ERROR_MESSAGES[safeError(status.errorCode)]; return; }
  statusElement.textContent = `Updated ${date(status.lastSyncAt) ?? 'recently'}`;
  for (const window of status.sample.windows) {
    const row = document.createElement('div'); row.className = 'window';
    const heading = document.createElement('div'); heading.className = 'window-head';
    const name = document.createElement('span'); name.textContent = window.label;
    const percent = document.createElement('span'); percent.textContent = window.usedPercent === null ? '—' : `${Math.round(window.usedPercent * 10) / 10}% used`;
    heading.append(name, percent); row.append(heading);
    const parts = [];
    if (window.resetAt) parts.push(`Resets ${date(window.resetAt)}`);
    if (window.expiresAt) parts.push(`Expires ${date(window.expiresAt)}`);
    if (typeof window.remaining === 'number' && window.unit) parts.push(`${window.remaining.toLocaleString()} ${window.unit} left`);
    if (parts.length) { const note = document.createElement('div'); note.className = 'window-note'; note.textContent = parts.join(' · '); row.append(note); }
    if (window.usedPercent !== null) { const track = document.createElement('div'); track.className = 'track'; const fill = document.createElement('div'); fill.className = 'fill'; fill.style.width = `${window.usedPercent}%`; track.append(fill); row.append(track); }
    windowsElement.append(row);
  }
}
const current = await chrome.runtime.sendMessage({type: 'status'});
region.value = current.region === 'cn' ? 'cn' : 'intl';
render(current.status);
region.addEventListener('change', async () => { await chrome.runtime.sendMessage({type: 'region', region: region.value}); render(null); });
sync.addEventListener('click', async () => {
  sync.disabled = region.disabled = true;
  statusElement.textContent = 'Reading Qwen usage…';
  try { render(await chrome.runtime.sendMessage({type: 'collect'})); }
  catch { render({ok: false, errorCode: 'host_unavailable'}); }
  finally { sync.disabled = region.disabled = false; }
});
