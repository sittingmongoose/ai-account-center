import { usageView, timeLabel } from './accounts-data.mjs';
import { visibleUsageWindows } from './visible-usage.mjs';

const profileId = value => typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value);
const publicId = value => typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$/.test(value);
const date = value => typeof value === 'string' ? Date.parse(value) : NaN;
const percent = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 100;
const range = (value, min, max) => Number.isInteger(value) && value >= min && value <= max;
const outcomes = new Set(['disabled', 'scheduled', 'setup_required', 'healthy', 'no_fresh_quota', 'no_candidate', 'waiting_idle', 'cooldown', 'switching', 'switched', 'deferred', 'error']);
export function validAntigravityAuto(status) {
  return !!status && typeof status.enabled === 'boolean' && range(status.thresholdUsedPercent, 1, 99)
    && range(status.pollIntervalSeconds, 15, 3600) && range(status.maxQuotaAgeSeconds, 15, 900)
    && range(status.cooldownSeconds, 60, 3600) && Array.isArray(status.selectedHostIds)
    && status.selectedHostIds.length === 1 && status.selectedHostIds[0] === 'ubuntu'
    && (status.requestedPoolId === null || publicId(status.requestedPoolId))
    && outcomes.has(status.outcome) && typeof status.activationInProgress === 'boolean';
}
const moduleName = value => typeof value === 'string' && /^[A-Za-z_][A-Za-z0-9_.]{0,63}$/.test(value);
const minor = value => typeof value === 'string' && /^\d{1,2}\.\d{1,3}$/.test(value);
/** Why the Ubuntu runtime service is not running (server read-only check), in plain words; null when unknown. */
export function antigravityServiceProblem(inventory) {
  const problem = inventory?.runtimeServiceProblem;
  if (!problem || typeof problem !== 'object') return null;
  const name = moduleName(problem.module) ? problem.module : 'unknown';
  const moved = minor(problem.python) && minor(problem.builtFor) && problem.python !== problem.builtFor
    ? ` (Python ${problem.python}; runtime built for ${problem.builtFor})` : '';
  const rebuild = '; switching is off until the runtime bundle is rebuilt';
  if (problem.reason === 'missing-python-module') return `Runtime service failed: missing Python module ${name}${moved}${rebuild}`;
  if (problem.reason === 'parser-mismatch') return `Runtime service failed: Python module ${name} is not the pinned version${moved}${rebuild}`;
  if (problem.reason === 'parser-import-failed') return `Runtime service failed: Python module ${name} does not load${moved}${rebuild}`;
  if (problem.reason === 'service-failed') return `Runtime service failed${range(problem.exitStatus, 1, 255) ? ` (exit status ${problem.exitStatus})` : ''}; switching is off until its cause is fixed`;
  return null;
}
function boundProfiles(data, inventory) {
  if (inventory?.schemaVersion !== 1 || inventory.hostId !== 'ubuntu' || !Array.isArray(inventory.profiles) || inventory.profiles.length > 16) return [];
  const counts = new Map();
  for (const p of inventory.profiles) counts.set(p?.id, (counts.get(p?.id) || 0) + 1);
  const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
  return inventory.profiles.flatMap(profile => {
    if (!profileId(profile?.id) || counts.get(profile.id) !== 1 || profile.hostId !== 'ubuntu' || typeof profile.email !== 'string'
      || typeof profile.available !== 'boolean' || typeof profile.selected !== 'boolean' || typeof profile.runtimeVerified !== 'boolean') return [];
    const matches = accounts.filter(row => row.provider === 'antigravity' && row.id === `antigravity:profile:${profile.id}`
      && row.capabilities?.antigravityProfileId === profile.id && row.platform === 'ubuntu'
      && row.email === profile.email);
    if (matches.length !== 1) return [];
    return [{ profile, account: matches[0] }];
  });
}
function nativeAvailable(row, inventory) {
  return inventory?.activationSupported === true && row.profile.available === true && Number.isFinite(date(row.profile.verifiedAt))
    && row.account.status !== 'needs_sign_in' && row.account.capabilities?.antigravityCanActivate === true
    && Array.isArray(row.account.capabilities.antigravityHostIds) && row.account.capabilities.antigravityHostIds.length === 1
    && row.account.capabilities.antigravityHostIds[0] === 'ubuntu';
}
function reportedPools(account) {
  const pools = new Map();
  for (const window of visibleUsageWindows('antigravity', account.windows)) {
    if (!publicId(window.poolId) || !['provider-id', 'provider-bucket-membership'].includes(window.poolIdSource)) continue;
    if (!pools.has(window.poolId)) pools.set(window.poolId, []);
    pools.get(window.poolId).push(window);
  }
  return pools;
}
function freshPool(row, windows, status, now) {
  const sample = date(row.account.sampledAt);
  return ['ok', 'fresh'].includes(row.account.status) && Number.isFinite(sample) && sample <= now && now - sample <= status.maxQuotaAgeSeconds * 1000
    && windows.length >= 2 && windows.some(w => w.windowMinutes === 300) && windows.some(w => w.windowMinutes === 10080)
    && new Set(windows.map(w => w.key)).size === windows.length
    && windows.every(w => typeof w.key === 'string' && w.key && w.kind === 'rate_limit' && w.status !== 'cached'
      && w.enabled !== false && (w.enabled === undefined || typeof w.enabled === 'boolean')
      && w.unlimited !== true && (w.unlimited === undefined || typeof w.unlimited === 'boolean')
      && (percent(w.usedPercent) || percent(w.remainingPercent)) && Number.isFinite(date(w.resetAt)) && date(w.resetAt) > now);
}
const signature = windows => windows.map(w => `${w.key}|${w.kind}|${w.windowMinutes}`).sort().join('\n');
/** UI capability checks only gate controls. Every write is revalidated by the native transaction. */
export function antigravityView(data, inventory, autoStatus, now = Date.now()) {
  const known = validAntigravityAuto(autoStatus);
  const status = known ? autoStatus : null;
  const rows = boundProfiles(data, inventory);
  const ready = rows.length >= 2 && rows.length === inventory?.profiles?.length && rows.every(row => nativeAvailable(row, inventory))
    && rows.filter(row => row.profile.selected).length === 1 && new Set(rows.map(row => row.profile.email.toLowerCase())).size === rows.length;
  const allPools = rows.map(row => reportedPools(row.account));
  const shared = ready && known ? [...(allPools[0]?.keys() || [])].filter(id => allPools.every((pools, index) => pools.has(id)
    && freshPool(rows[index], pools.get(id), status, now) && signature(pools.get(id)) === signature(allPools[0].get(id)))) .sort() : [];
  const choices = [{ id: null, label: 'Choose quota pool' }, ...shared.map((id, index) => {
    const window = allPools[0].get(id)[0];
    return { id, label: `${window.poolLabel || `Quota pool ${index + 1}`}${window.poolIdSource === 'provider-bucket-membership' ? ' (reported group)' : ''}` };
  })];
  // Preserve a configured but currently unavailable pool without making it selectable as proof.
  if (known && status.requestedPoolId !== null && !shared.includes(status.requestedPoolId)) choices.push({ id: status.requestedPoolId, label: 'Configured pool · unavailable' });
  // Duplicate actual display names stay distinct; identifiers are not inferred from labels.
  const usedLabels = new Set();
  for (const choice of choices) {
    const base = choice.label;
    let suffix = 1;
    while (usedLabels.has(choice.label)) choice.label = `${base} · ${suffix++}`;
    usedLabels.add(choice.label);
  }
  const selected = choices.find(choice => choice.id === status?.requestedPoolId) || choices[0];
  const available = ready && known && status.activationInProgress !== true;
  const canEnable = available && shared.includes(status.requestedPoolId);
  const pausedVersion = inventory?.nativeUpdatePaused && typeof inventory.nativeUpdatePaused.installedVersion === 'string'
    && inventory.nativeUpdatePaused.installedVersion.length <= 128
    && /^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(inventory.nativeUpdatePaused.installedVersion)
    ? inventory.nativeUpdatePaused.installedVersion : null;
  const updatePaused = inventory?.nativeUpdatePaused
    ? `Antigravity updated${pausedVersion ? ` to ${pausedVersion}` : ''}; switching paused until reviewed` : null;
  const previewId = status?.requestedPoolId && allPools.some(p => p.has(status.requestedPoolId)) ? status.requestedPoolId : [...(allPools[0]?.keys() || [])].sort()[0];
  const previewLabel = rows.length && previewId ? allPools.find(p => p.has(previewId))?.get(previewId)?.[0]?.poolLabel || 'Reported quota pool' : '';
  return {
    antigravityAccounts: rows.map((row, index) => {
      const windows = allPools[index].get(previewId) || [];
      const five = windows.find(window => window.windowMinutes === 300), weekly = windows.find(window => window.windowMinutes === 10080);
      return { id: row.account.id, profile: row.profile.id, email: row.profile.email, plan: row.account.plan || row.profile.plan || '',
        selected: row.profile.selected, runtimeVerified: row.profile.runtimeVerified,
        canActivate: nativeAvailable(row, inventory) && !row.profile.selected && status?.activationInProgress !== true,
        status: (row.profile.selected ? row.profile.runtimeVerified ? 'Selected · running verified' : 'Selected · runtime unverified' : row.account.status === 'needs_sign_in' ? 'Sign-in needed' : row.profile.available ? 'Saved login' : 'Login unavailable') + ({ cached: ' · Cached', needs_sign_in: ' · Sign-in needed', unavailable: ' · Usage unavailable', error: ' · Refresh failed' }[row.account.status] || ''),
        five: usageView(five, true), weekly: usageView(weekly, true),
        note: [row.account.source, row.account.status, timeLabel(row.account.sampledAt, 'Sampled'), row.account.message].filter(Boolean).join(' · ') };
    }),
    antigravityAutoKnown: known, antigravityAutoEnabled: status?.enabled === true,
    antigravityCanEnable: canEnable, antigravityAutoAvailable: available && (status.enabled === true || canEnable), antigravitySettingsAvailable: available,
    antigravityPoolAvailable: available && shared.length > 0,
    antigravityThresholdLabel: known ? `${status.thresholdUsedPercent}%` : '—',
    antigravityPoolOptions: choices.map(choice => choice.label), antigravityPoolLabel: selected.label,
    antigravityPoolChoices: choices, antigravitySharedPoolIds: shared,
    antigravityPreview: previewLabel ? `${previewLabel} · 5-hour / weekly` : 'Quota samples unavailable',
    antigravityAutoMessage: status?.message || 'Ubuntu account controls unavailable until the native runtime is verified.',
    antigravityAutoSetting: known ? `${status.thresholdUsedPercent}% used · ${status.pollIntervalSeconds}s · Ubuntu` : 'Automatic switching status unavailable',
    antigravityUpdatePaused: updatePaused,
    antigravityServiceProblem: antigravityServiceProblem(inventory),
  };
}
export function antigravitySettingsPatch(view, action, value) {
  if (action === 'antigravity-automatic') {
    if (!view.antigravityAutoAvailable || !['true', 'false'].includes(value) || (value === 'true' && !view.antigravityCanEnable)) return null;
    return { enabled: value === 'true' };
  }
  if (action === 'antigravity-threshold') {
    const used = Number(String(value).replace(/%$/, ''));
    return view.antigravitySettingsAvailable && range(used, 1, 99) ? { thresholdUsedPercent: used } : null;
  }
  if (action === 'antigravity-pool') {
    if (!view.antigravityPoolAvailable) return null;
    const choice = view.antigravityPoolChoices.find(row => row.label === value);
    if (!choice || (choice.id !== null && !view.antigravitySharedPoolIds.includes(choice.id))) return null;
    return { requestedPoolId: choice.id };
  }
  return null;
}
