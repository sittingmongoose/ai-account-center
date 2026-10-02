import { AntigravityError } from './errors';
import { credentialFingerprint } from './registry';
import type {
  AntigravitySwitchDriver,
  InstallReceipt,
  NativeCredential,
  StopReceipt,
  VerifiedIdentity,
} from './types';
import type { UbuntuRuntimeBridge } from './ubuntu-runtime-bridge';
import type { createAntigravityQuotaWorker } from './quota-worker-transport';

/** Credential bytes and rollback handles are private application dependencies. */
export interface UbuntuNativeCredentialStore {
  read(): Promise<NativeCredential>;
  install(credential: NativeCredential, expectedFingerprint: string): Promise<InstallReceipt>;
  rollback(receipt: InstallReceipt, previous: NativeCredential): Promise<boolean>;
}

export interface UbuntuAntigravityDriverDependencies {
  nativeStore: UbuntuNativeCredentialStore;
  quotaWorker: Pick<ReturnType<typeof createAntigravityQuotaWorker>, 'validateCredential'>;
  bridge: UbuntuRuntimeBridge;
  /** A trusted installed-source gate; never a dashboard field or saved user setting. */
  releaseGate?: () => Promise<boolean>;
}

/** Construction touches no credential, file, provider, IPC or process. */
export function createUbuntuAntigravityDriver(
  deps: UbuntuAntigravityDriverDependencies
): AntigravitySwitchDriver {
  let quiescedApproval: {
    fingerprint: string;
    sampledAt: number;
    receipt: StopReceipt;
    expected: VerifiedIdentity;
  } | null = null;
  let ownedStopPending = false;
  const released = async () => {
    try {
      return deps.releaseGate ? (await deps.releaseGate()) === true : false;
    } catch {
      return false;
    }
  };
  const canProve = async () => {
    if (!(await released())) return false;
    try {
      return (await deps.bridge.canProveRuntimeIdentity()) === true;
    } catch {
      return false;
    }
  };
  const requireReleased = async () => {
    if (!(await canProve())) throw new AntigravityError('antigravity-runtime-unavailable');
  };
  return {
    hostId: 'ubuntu',
    canProveRuntimeIdentity: canProve,
    canRestartSupportedNative: released,
    readCurrentCredential: () => deps.nativeStore.read(),
    validateCredential: (credential) => deps.quotaWorker.validateCredential(credential),
    inspectProcesses: () => deps.bridge.inspectProcesses(),
    approveOwnedIdlePlan: async (plan, expected, fingerprint) =>
      (await canProve()) &&
      deps.bridge.approveOwnedIdlePlan !== undefined &&
      (await deps.bridge.approveOwnedIdlePlan(plan, expected, fingerprint)) === true,
    revalidateQuiescedStop: async (receipt, expected, fingerprint) => {
      quiescedApproval = null;
      if (!(await released()) || !deps.bridge.revalidateQuiescedStop) return null;
      const proof = await deps.bridge.revalidateQuiescedStop(receipt, expected, fingerprint);
      if (
        !proof ||
        proof.available !== true ||
        proof.complete !== true ||
        proof.busy !== false ||
        proof.manualActivationInProgress !== false ||
        proof.hostId !== 'ubuntu'
      )
        return null;
      const sampledAt = Date.parse(proof.sampledAt);
      const age = Date.now() - sampledAt;
      if (!Number.isFinite(age) || age < -1000 || age > 5000) return null;
      quiescedApproval = { fingerprint, sampledAt, receipt, expected };
      return proof;
    },
    completeActivation: async (expected) => {
      if (!deps.bridge.completeActivation)
        throw new AntigravityError('antigravity-runtime-unavailable');
      await deps.bridge.completeActivation(expected);
      quiescedApproval = null;
      ownedStopPending = false;
    },
    stopProcesses: async (plan) => {
      await requireReleased();
      const receipt = await deps.bridge.stopProcesses(plan);
      ownedStopPending = true;
      return receipt;
    },
    installCredential: async (credential, expectedFingerprint) => {
      const approval = quiescedApproval;
      quiescedApproval = null;
      let approved =
        approval &&
        approval.fingerprint === expectedFingerprint &&
        Date.now() - approval.sampledAt <= 5000 &&
        (await released());
      if (approved && approval && deps.bridge.revalidateQuiescedStop) {
        const latest = await deps.bridge.revalidateQuiescedStop(
          approval.receipt,
          approval.expected,
          expectedFingerprint
        );
        const age = latest ? Date.now() - Date.parse(latest.sampledAt) : NaN;
        approved =
          !!latest &&
          latest.available === true &&
          latest.complete === true &&
          latest.busy === false &&
          latest.manualActivationInProgress === false &&
          latest.hostId === 'ubuntu' &&
          Number.isFinite(age) &&
          age >= -1000 &&
          age <= 5000;
      }
      if (!approved) {
        if (ownedStopPending) throw new AntigravityError('antigravity-runtime-unavailable');
        await requireReleased();
      }
      return deps.nativeStore.install(credential, expectedFingerprint);
    },
    readStoredIdentity: async () =>
      deps.quotaWorker.validateCredential(await deps.nativeStore.read()),
    restartProcesses: (receipt) => deps.bridge.restartProcesses(receipt),
    proveRuntimeIdentity: async (expected) => {
      await requireReleased();
      const proof = await deps.bridge.proveRuntimeIdentity(expected);
      // Native refresh may publish new bytes; bind the exact current selection.
      const current = await deps.nativeStore.read();
      const actual = await deps.quotaWorker.validateCredential(current);
      if (
        actual.email !== expected.email ||
        actual.subject !== expected.subject ||
        proof.identity.email !== actual.email ||
        proof.identity.subject !== actual.subject ||
        proof.credentialFingerprint !== credentialFingerprint(current) ||
        proof.runtimeStarted !== true ||
        proof.sessionRestored !== true
      )
        throw new AntigravityError('antigravity-runtime-unavailable');
      return proof;
    },
    rollbackCredential: (receipt, previous) => deps.nativeStore.rollback(receipt, previous),
    stopOwnedRestarts: () => deps.bridge.stopOwnedRestarts(),
  };
}
