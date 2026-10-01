import {ERROR_MESSAGES, safeCode, MUSE_ERROR_MESSAGES, safeMuseCode, projectMuseTeams} from './bridge-core.mjs';
const status = document.getElementById('status');
const display = document.getElementById('sample');
const button = document.getElementById('sync');
function render(sample) {
  display.replaceChildren();
  if (!sample) return;
  for (const window of sample.windows) {
    const row = document.createElement('div'); row.className = 'row';
    const label = document.createElement('div'); label.textContent = window.label; row.append(label);
    const value = document.createElement('div'); value.className = 'amount';
    value.textContent = window.kind === 'balance' ? new Intl.NumberFormat(undefined, {style: 'currency', currency: 'USD', maximumFractionDigits: 6}).format(window.remaining) : `${window.usedPercent.toFixed(1)}% used`;
    row.append(value);
    const timestamp = window.expiresAt ?? window.resetAt;
    if (timestamp) { const date = document.createElement('small'); date.textContent = `${window.expiresAt ? 'Expires' : 'Resets'} ${new Date(timestamp).toLocaleString()}`; row.append(date); }
    display.append(row);
  }
  const updated = document.createElement('p'); updated.textContent = `Updated ${new Date(sample.fetchedAt).toLocaleString()}`; display.append(updated);
}
const saved = await chrome.storage.local.get(['lastSample', 'lastError']);
render(saved.lastSample);
if (saved.lastError) status.textContent = ERROR_MESSAGES[safeCode(saved.lastError)];
button.addEventListener('click', async () => {
  button.disabled = true; status.textContent = 'Reading console usage…';
  try {
    const [tab] = await chrome.tabs.query({active: true, currentWindow: true});
    const result = await chrome.runtime.sendMessage({action: 'sync', tabURL: tab?.url ?? null});
    if (result?.ok) { render(result.sample); status.textContent = 'Usage synced for AI Account Center.'; }
    else status.textContent = ERROR_MESSAGES[safeCode(result?.code)];
  } catch { status.textContent = ERROR_MESSAGES.host_unavailable; }
  finally { button.disabled = false; }
});

const museButton = document.getElementById('muse-sync');
const museStatus = document.getElementById('muse-status');
const museDisplay = document.getElementById('muse-sample');
const museTeamLabel = document.getElementById('muse-team-label');
const museTeamSelect = document.getElementById('muse-team');
function renderMuse(sample) {
  museDisplay.replaceChildren();
  if (!sample) return;
  for (const window of sample.windows) {
    const row = document.createElement('div'); row.className = 'row';
    const label = document.createElement('div'); label.textContent = window.label; row.append(label);
    const value = document.createElement('div'); value.className = 'amount';
    value.textContent = window.usedPercent === null ? 'Usage unavailable' : `${window.usedPercent.toFixed(1)}% used`; row.append(value);
    if (window.resetAt) { const date = document.createElement('small'); date.textContent = `Resets ${new Date(window.resetAt).toLocaleString()}`; row.append(date); }
    museDisplay.append(row);
  }
  const observed = document.createElement('p');
  observed.textContent = `${sample.status === 'cached' ? 'Last successful reading' : 'Updated'} ${new Date(sample.sampledAt).toLocaleString()}`;
  museDisplay.append(observed);
}
function renderTeams(teams) {
  museTeamSelect.replaceChildren();
  const empty = document.createElement('option'); empty.value = ''; empty.textContent = 'Choose your subscription team'; museTeamSelect.append(empty);
  for (const team of projectMuseTeams(teams)) { const option = document.createElement('option'); option.value = team.id; option.textContent = team.name; museTeamSelect.append(option); }
  museTeamLabel.hidden = !teams.length;
}
const museSaved = await chrome.storage.local.get(['lastMuseSample', 'lastMuseError', 'museTeams', 'selectedMuseTeamId']);
renderMuse(museSaved.lastMuseSample); renderTeams(museSaved.museTeams ?? []);
if (museSaved.lastMuseError) museStatus.textContent = MUSE_ERROR_MESSAGES[safeMuseCode(museSaved.lastMuseError)];
else if (museSaved.lastMuseSample?.status === 'cached') museStatus.textContent = museSaved.lastMuseSample.message;
museButton.addEventListener('click', async () => {
  museButton.disabled = true; museStatus.textContent = 'Reading Muse usage…';
  try {
    const teamId = museTeamLabel.hidden ? museSaved.selectedMuseTeamId ?? null : museTeamSelect.value || null;
    const result = await chrome.runtime.sendMessage({action: 'museSync', teamId});
    if (result?.ok) { renderMuse(result.sample); museTeamLabel.hidden = true; museStatus.textContent = result.sample.status === 'cached' ? result.sample.message : 'Muse usage synced for AI Account Center.'; }
    else { if (result?.teams) renderTeams(result.teams); museStatus.textContent = MUSE_ERROR_MESSAGES[safeMuseCode(result?.code)]; }
  } catch { museStatus.textContent = MUSE_ERROR_MESSAGES.host_unavailable; }
  finally { museButton.disabled = false; }
});
