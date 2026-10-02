import type {
  AntigravityAutoSwitchSettings,
  AntigravityAutoSwitchSettingsPatch,
  AntigravityAutoSwitchStoredState,
} from './types';

export const DEFAULT_ANTIGRAVITY_AUTO_SWITCH_SETTINGS: AntigravityAutoSwitchSettings = {
  enabled: false,
  thresholdUsedPercent: 95,
  pollIntervalSeconds: 60,
  maxQuotaAgeSeconds: 300,
  cooldownSeconds: 300,
  selectedHostIds: ['ubuntu'],
  requestedPoolId: null,
};

const SETTING_KEYS = new Set(Object.keys(DEFAULT_ANTIGRAVITY_AUTO_SWITCH_SETTINGS));

export class AntigravityAutoSwitchSettingsError extends Error {
  constructor() {
    super('Invalid Antigravity automatic switching settings.');
    this.name = 'AntigravityAutoSwitchSettingsError';
  }
}

export function isPublicId(value: unknown): value is string {
  return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$/.test(value);
}

function integerRange(value: unknown, minimum: number, maximum: number): boolean {
  return (
    typeof value === 'number' && Number.isInteger(value) && value >= minimum && value <= maximum
  );
}

function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

export function isAntigravityAutoSwitchSettingsPatch(
  value: unknown
): value is AntigravityAutoSwitchSettingsPatch {
  if (!record(value)) return false;
  const keys = Object.keys(value);
  if (!keys.length || keys.some((key) => !SETTING_KEYS.has(key))) return false;
  return (
    (!keys.includes('enabled') || typeof value.enabled === 'boolean') &&
    (!keys.includes('thresholdUsedPercent') || integerRange(value.thresholdUsedPercent, 1, 99)) &&
    (!keys.includes('pollIntervalSeconds') || integerRange(value.pollIntervalSeconds, 15, 3600)) &&
    (!keys.includes('maxQuotaAgeSeconds') || integerRange(value.maxQuotaAgeSeconds, 15, 900)) &&
    (!keys.includes('cooldownSeconds') || integerRange(value.cooldownSeconds, 60, 3600)) &&
    (!keys.includes('requestedPoolId') ||
      value.requestedPoolId === null ||
      isPublicId(value.requestedPoolId)) &&
    (!keys.includes('selectedHostIds') ||
      (Array.isArray(value.selectedHostIds) &&
        value.selectedHostIds.length === 1 &&
        value.selectedHostIds[0] === 'ubuntu'))
  );
}

export function copySettings(value: AntigravityAutoSwitchSettings): AntigravityAutoSwitchSettings {
  return { ...value, selectedHostIds: [...value.selectedHostIds] };
}

export function defaultAntigravityAutoSwitchState(): AntigravityAutoSwitchStoredState {
  return {
    version: 1,
    settings: copySettings(DEFAULT_ANTIGRAVITY_AUTO_SWITCH_SETTINGS),
    lastSwitch: null,
  };
}

export function validateAntigravityAutoSwitchStoredState(
  value: unknown
): AntigravityAutoSwitchStoredState {
  if (
    !record(value) ||
    value.version !== 1 ||
    Object.keys(value).some((key) => !['version', 'settings', 'lastSwitch'].includes(key))
  ) {
    throw new AntigravityAutoSwitchSettingsError();
  }
  if (
    !record(value.settings) ||
    Object.keys(value.settings).length !== SETTING_KEYS.size ||
    !isAntigravityAutoSwitchSettingsPatch(value.settings)
  ) {
    throw new AntigravityAutoSwitchSettingsError();
  }
  const settings = copySettings(value.settings as unknown as AntigravityAutoSwitchSettings);
  if (value.lastSwitch === null) return { version: 1, settings, lastSwitch: null };
  const last = value.lastSwitch;
  if (
    !record(last) ||
    Object.keys(last).length !== 3 ||
    Object.keys(last).some((key) => !['at', 'hostId', 'profileId'].includes(key)) ||
    typeof last.at !== 'string' ||
    !Number.isFinite(Date.parse(last.at)) ||
    last.hostId !== 'ubuntu' ||
    !isPublicId(last.profileId)
  ) {
    throw new AntigravityAutoSwitchSettingsError();
  }
  return {
    version: 1,
    settings,
    lastSwitch: { at: last.at, hostId: 'ubuntu', profileId: last.profileId },
  };
}

export function mergeAntigravityAutoSwitchSettings(
  state: AntigravityAutoSwitchStoredState,
  patch: unknown
): AntigravityAutoSwitchStoredState {
  if (!isAntigravityAutoSwitchSettingsPatch(patch)) throw new AntigravityAutoSwitchSettingsError();
  return validateAntigravityAutoSwitchStoredState({
    ...state,
    settings: { ...state.settings, ...patch },
  });
}
