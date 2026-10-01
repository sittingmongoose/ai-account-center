import {NATIVE_HOST, CONSOLE_URLS, COOKIE_DOMAINS, filterCookies, cookieDiagnostics, projectSample, safeError} from './bridge-core.mjs';
const ALARM = 'ccs-qwen-usage';
let pending = null;
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
        // Read the diagnostic to prevent Chrome's unchecked-error warning; never persist it.
        void chrome.runtime.lastError;
        finish('host_unavailable');
      });
      port.postMessage(message);
    } catch { finish('host_unavailable'); }
  });
}
async function collect() {
  if (pending) return pending;
  pending = (async () => {
    const {region: savedRegion = 'intl'} = await chrome.storage.local.get(['region']);
    const region = Object.hasOwn(CONSOLE_URLS, savedRegion) ? savedRegion : 'intl';
    let diagnostics = null;
    try {
      // The browser owns decryption. Cookie values remain in this local exchange only.
      // Domain queries include all cookie paths. A root URL omits path-specific
      // console cookies; native code later applies each fixed endpoint's scope.
      const groups = await Promise.all(COOKIE_DOMAINS[region].map(domain => chrome.cookies.getAll({domain})));
      const cookies = filterCookies(groups.flat(), region);
      diagnostics = cookieDiagnostics(groups.flat(), cookies, region);
      if (!cookies.length) throw new Error('no_browser_cookie');
      const sample = await nativeExchange({action: 'collect', region, cookies});
      const status = {ok: true, lastSyncAt: new Date().toISOString(), errorCode: null, sample, diagnostics};
      await chrome.storage.local.set({status});
      return status;
    } catch (error) {
      const errorCode = safeError(error instanceof Error ? error.message : 'error');
      const status = {ok: false, lastSyncAt: new Date().toISOString(), errorCode, diagnostics};
      await chrome.storage.local.set({status});
      return status;
    }
  })().finally(() => { pending = null; });
  return pending;
}
async function initialize() {
  await chrome.alarms.create(ALARM, {delayInMinutes: 1, periodInMinutes: 5});
}
chrome.runtime.onInstalled.addListener(() => { void initialize(); });
chrome.runtime.onStartup.addListener(() => { void initialize(); });
chrome.alarms.onAlarm.addListener(alarm => { if (alarm.name === ALARM) void collect(); });
chrome.runtime.onMessage.addListener((message, sender, respond) => {
  if (sender.id !== chrome.runtime.id || !message || typeof message !== 'object') return false;
  if (message.type === 'collect') { collect().then(respond); return true; }
  if (message.type === 'status') { chrome.storage.local.get(['region', 'status']).then(respond); return true; }
  if (message.type === 'region' && Object.hasOwn(CONSOLE_URLS, message.region)) {
    chrome.storage.local.set({region: message.region}).then(() => respond({ok: true}));
    return true;
  }
  return false;
});
