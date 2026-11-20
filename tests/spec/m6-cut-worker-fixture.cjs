// The original M0 fixture inputs for the private worker in the native CLI replay.
// This file is loaded only through NODE_OPTIONS on the worker fixture subprocess.
'use strict';

const Module = require('node:module');
const fixture = require('./fixture.json');
const noop = () => {};
const stop = () => noop;
const replacements = {
  '/utils/managed-process.js': {
    startProcessBroker: async () => {},
    stopProcessBroker: async () => {},
    runManagedCommand: async () => ({ stdout: '', stderr: '', exitCode: 1, timedOut: false }),
  },
  '/web-server/services/account-lifecycle-runtime.js': {
    startAccountLifecycleMaintenance: noop,
    stopAccountLifecycleMaintenance: noop,
  },
  '/web-server/status/status-service.js': {
    startStatusService: stop,
    getStatusService: () => ({
      view: () => fixture.statusView,
      answers: { sendList: () => [], history: () => [] },
    }),
  },
  '/web-server/scoreboard/scoreboard-service.js': {
    startScoreboardService: noop,
    getScoreboardService: () => ({ view: async () => fixture.scoreboardView }),
  },
  '/web-server/agent-usage/agent-usage-snapshot.js': { startAgentUsageSnapshot: stop },
  '/antigravity/production-runtime.js': { getInstalledAntigravityRuntimeFactory: () => null },
  '/codex-auth/codex-activation-lock.js': { getActivationCodexHome: () => process.env.CODEX_HOME },
  '/web-server/services/app-update-service.js': {
    getAppUpdateService: () => ({
      getStatus: () => ({ job: null }),
      start: () => ({ job: { id: 'fixture-update', state: 'running', hosts: {}, results: [] } }),
      cancel: () => ({ job: null }),
    }),
  },
  '/store/store-service.js': {
    openAacStore: async () => ({ store: null, reason: 'disabled' }),
    getAacStore: () => ({
      status: async () => fixture.storeStatus,
      seriesPoints: async () => fixture.seriesPoints,
      seriesList: async () => [],
      usageAt: async () => fixture.usageAt,
      burnLong: async () => [],
      periods: async () => [],
    }),
    aacStoreState: () => ({ state: 'unavailable', reason: 'disabled' }),
  },
};

const load = Module._load;
const configured = new WeakSet();
Module._load = function loadFixture(request, parent, main) {
  const exports = load.call(this, request, parent, main);
  if (typeof request !== 'string' || !request.startsWith('.')) return exports;
  const candidate = Module._resolveFilename(request, parent, main).replaceAll('\\', '/');
  const dist = candidate.lastIndexOf('/dist/');
  if (dist === -1) return exports;
  const file = candidate.slice(dist + '/dist'.length);
  if (configured.has(exports)) return exports;
  if (file === '/web-server/services/account-lifecycle-runtime.js') {
    const facts = exports.lifecycleProviderFacts;
    exports.lifecycleProviderFacts = (context, sources) =>
      facts(
        context,
        sources ?? {
          codexCliAvailable: () => false,
          claudeEnabled: () => false,
          antigravityFlow: () => 'tool_missing',
          museEnabled: () => false,
        }
      );
  }
  if (file === '/web-server/services/codex-profile-renewal-service.js') {
    exports.CodexProfileRenewalService.prototype.start = noop;
  }
  if (Object.hasOwn(replacements, file)) Object.assign(exports, replacements[file]);
  if (exports && (typeof exports === 'object' || typeof exports === 'function'))
    configured.add(exports);
  return exports;
};
