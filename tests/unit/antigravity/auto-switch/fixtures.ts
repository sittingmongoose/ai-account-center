import { defaultAntigravityAutoSwitchState } from '../../../../src/antigravity/auto-switch/settings';
import type {
  AntigravityAutoObservation,
  AntigravityAutoProfile,
  AntigravityAutoSwitchStoredState,
} from '../../../../src/antigravity/auto-switch/types';

export const NOW = Date.parse('2026-10-01T17:00:00.000Z');
export const futureReset = new Date(NOW + 3_600_000).toISOString();

export function state(): AntigravityAutoSwitchStoredState {
  const value = defaultAntigravityAutoSwitchState();
  value.settings.enabled = true;
  value.settings.requestedPoolId = 'gemini-pool';
  return value;
}

export function profile(id: string, active = false): AntigravityAutoProfile {
  return {
    id,
    hostId: 'ubuntu',
    identityKey: `verified-google-subject:${id}`,
    credentialRevision: `private-revision:${id}`,
    authValid: true,
    nativeConsumerCompatible: true,
    isActive: active,
  };
}

export function observation(): AntigravityAutoObservation {
  const profiles = [profile('gmail', true), profile('party')];
  return {
    profiles,
    quotas: profiles.map((item, index) => ({
      profileId: item.id,
      hostId: 'ubuntu',
      identityKey: item.identityKey,
      credentialRevision: item.credentialRevision,
      source: 'native-consumer',
      status: 'fresh',
      sampledAt: new Date(NOW).toISOString(),
      pools: [
        {
          id: 'gemini-pool',
          idSource: 'provider-id',
          eligibility: 'reported-quota',
          complete: true,
          windows: [
            {
              key: 'native-model-bucket',
              kind: 'rate_limit',
              remainingPercent: index === 0 ? 5 : 80,
              resetAt: futureReset,
            },
          ],
        },
      ],
    })),
    hosts: [
      {
        hostId: 'ubuntu',
        available: true,
        complete: true,
        busy: false,
        manualActivationInProgress: false,
        sampledAt: new Date(NOW).toISOString(),
      },
    ],
  };
}

export function copy<T>(value: T): T {
  return structuredClone(value);
}
