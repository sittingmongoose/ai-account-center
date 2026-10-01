import {NATIVE_HOST, CONSOLE_URLS, COOKIE_DOMAINS, filterCookies, cookieDiagnostics, projectSample, previousSample, isQwenCookieDomain, safeError} from './bridge-core.mjs';
const ALARM = 'ccs-qwen-usage';
let pending = null;
let generation = 0;
let storageWrites = Promise.resolve();
const contextChanged = () => ({ok: false, errorCode: 'context_changed', lastSyncAt: null});
function queueWrite(action) {
  const operation = storageWrites.then(action);
  storageWrites = operation.catch(() => {});
  return operation;
}
function invalidateContext(update = {}) {
  generation++;
  pending = null;
  return queueWrite(() => chrome.storage.local.set({...update, status: null}));
}
function persistStatus(status, collectionGeneration) {
  return queueWrite(async () => {
    if (collectionGeneration !== generation) return contextChanged();
    await chrome.storage.local.set({status});
    return status;
  });
}
async function nativeExchange(message) {
  return new Promise((resolve, reject) => {
    let settled = false;
    let port;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try { port?.disconnect(); } catch {}
      if (error) reject(new Error(error)); else resolve(value);
    };
    const timer = setTimeout(() => finish('network_error'), 75000);
    try {
      port = chrome.runtime.connectNative(NATIVE_HOST);
      port.onMessage.addListener(value => {
        if (!value || typeof value !== 'object' || JSON.stringify(value).length > 65536) return finish('protocol_error');
        if (value.ok !== true) return finish(safeError(value.errorCode ?? value.error));
        try { finish(null, projectSample(value.sample)); } catch { finish('invalid_response'); }
      });
      port.onDisconnect.addListener(() => {
        void chrome.runtime.lastError;
        finish('host_unavailable');
      });
      port.postMessage(message);
    } catch { finish('host_unavailable'); }
  });
}
function collect() {
  if (pending) return pending;
  const collectionGeneration = generation;
  const operation = (async () => {
    await storageWrites;
    const {region: savedRegion = 'intl', status: storedStatus} = await chrome.storage.local.get(['region', 'status']);
    if (collectionGeneration !== generation) return contextChanged();
    const region = Object.hasOwn(CONSOLE_URLS, savedRegion) ? savedRegion : 'intl';
    let diagnostics = null;
    try {
      // Browser-owned decryption stays within the local native exchange.
      const groups = await Promise.all(COOKIE_DOMAINS[region].map(domain => chrome.cookies.getAll({domain})));
      const cookies = filterCookies(groups.flat(), region);
      diagnostics = cookieDiagnostics(groups.flat(), cookies, region);
      if (!cookies.length) throw new Error('no_browser_cookie');
      const sample = await nativeExchange({action: 'collect', region, cookies});
      return persistStatus({ok: true, region, lastSyncAt: new Date().toISOString(), errorCode: null, sample, diagnostics}, collectionGeneration);
    } catch (error) {
      const errorCode = safeError(error instanceof Error ? error.message : 'error');
      const historical = ['network_error', 'host_unavailable', 'busy'].includes(errorCode)
        ? previousSample(storedStatus, region) : null;
      const status = {ok: false, region, lastAttemptAt: new Date().toISOString(), lastSyncAt: historical?.lastSuccessAt ?? null, errorCode, diagnostics};
      if (historical) status.previousSample = historical;
      return persistStatus(status, collectionGeneration);
    }
  })();
  pending = operation;
  void operation.finally(() => { if (pending === operation) pending = null; }).catch(() => {});
  return operation;
}
async function initialize() {
  await chrome.alarms.create(ALARM, {delayInMinutes: 1, periodInMinutes: 5});
}
chrome.runtime.onInstalled.addListener(() => { void initialize(); });
chrome.runtime.onStartup.addListener(() => { void initialize(); });
chrome.alarms.onAlarm.addListener(alarm => { if (alarm.name === ALARM) void collect(); });
chrome.cookies.onChanged?.addListener(change => {
  if (isQwenCookieDomain(change?.cookie?.domain)) void invalidateContext();
});
chrome.runtime.onMessage.addListener((message, sender, respond) => {
  if (sender.id !== chrome.runtime.id || !message || typeof message !== 'object') return false;
  if (message.type === 'collect') { collect().then(respond); return true; }
  if (message.type === 'status') { storageWrites.then(() => chrome.storage.local.get(['region', 'status'])).then(respond); return true; }
  if (message.type === 'region' && Object.hasOwn(CONSOLE_URLS, message.region)) {
    invalidateContext({region: message.region}).then(() => respond({ok: true}));
    return true;
  }
  return false;
});
