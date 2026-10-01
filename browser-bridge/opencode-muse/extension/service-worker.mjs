import {NATIVE_HOST, filterCookies, projectSample, safeCode, workspaceFromURL, filterMuseCookies, projectMuseSample, projectMuseTeams, safeMuseCode} from './bridge-core.mjs';
let running = false;
let museRunning = false;
async function museSync(teamId = null) {
  if (museRunning) return {ok: false, code: 'busy'};
  museRunning = true;
  try {
    if (teamId !== null && (typeof teamId !== 'string' || !/^[0-9]{1,32}$/.test(teamId))) throw new Error('invalid_request');
    const cookies = filterMuseCookies(await chrome.cookies.getAll({url: 'https://dev.meta.ai/'}));
    const response = await chrome.runtime.sendNativeMessage(NATIVE_HOST, {schemaVersion: 1, action: 'museSync', cookies, teamId});
    if (!response || response.schemaVersion !== 1 || response.ok !== true) {
      const teams = response?.code === 'choose_team' ? projectMuseTeams(response.teams) : [];
      const code = safeMuseCode(response?.code);
      await chrome.storage.local.set({lastMuseError: code, museTeams: teams});
      return {ok: false, code, teams};
    }
    const sample = projectMuseSample(response.sample);
    if (typeof response.teamId !== 'string' || !/^[0-9]{1,32}$/.test(response.teamId)) throw new Error('protocol_error');
    await chrome.storage.local.set({lastMuseSample: sample, lastMuseError: null, selectedMuseTeamId: response.teamId, museTeams: []});
    await chrome.alarms.create('ccs-muse-refresh', {periodInMinutes: 120});
    return {ok: true, sample};
  } catch (error) {
    const code = /native messaging host|specified native|not found|exited/i.test(error?.message ?? '') ? 'host_unavailable' : safeMuseCode(error?.message);
    await chrome.storage.local.set({lastMuseError: code});
    return {ok: false, code};
  } finally { museRunning = false; }
}
async function sync(tabURL = null) {
  if (running) return {ok: false, code: 'busy'};
  running = true;
  try {
    const values = await chrome.cookies.getAll({url: 'https://opencode.ai/console/'});
    const cookies = filterCookies(values);
    const workspaceId = workspaceFromURL(tabURL);
    const response = await chrome.runtime.sendNativeMessage(NATIVE_HOST, {schemaVersion: 1, action: 'sync', cookies, workspaceId});
    if (!response || response.schemaVersion !== 1 || response.ok !== true) throw new Error(safeCode(response?.code));
    const sample = projectSample(response.sample);
    await chrome.storage.local.set({lastSample: sample, lastError: null, selectedWorkspaceId: workspaceId});
    await chrome.alarms.create('ccs-opencode-refresh', {periodInMinutes: 120});
    return {ok: true, sample};
  } catch (error) {
    const known = safeCode(error?.message);
    const code = known === 'error' && /native messaging host|specified native|not found|exited/i.test(error?.message ?? '') ? 'host_unavailable' : known;
    await chrome.storage.local.set({lastError: code});
    return {ok: false, code};
  } finally { running = false; }
}
chrome.runtime.onMessage.addListener((request, sender, respond) => {
  if (sender.id !== chrome.runtime.id) return false;
  if (request?.action === 'museSync') { museSync(request.teamId ?? null).then(respond); return true; }
  if (request?.action !== 'sync') return false;
  sync(request.tabURL).then(respond);
  return true;
});
chrome.alarms.onAlarm.addListener(async alarm => {
  if (alarm.name === 'ccs-muse-refresh') {
    const {selectedMuseTeamId} = await chrome.storage.local.get('selectedMuseTeamId');
    await museSync(selectedMuseTeamId ?? null); return;
  }
  if (alarm.name !== 'ccs-opencode-refresh') return;
  const {selectedWorkspaceId} = await chrome.storage.local.get('selectedWorkspaceId');
  await sync(selectedWorkspaceId ? `https://opencode.ai/console/${selectedWorkspaceId}/billing` : null);
});
