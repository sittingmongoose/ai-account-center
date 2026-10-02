import { AntigravityError } from './errors';
import type { AntigravitySwitchDriver, StopReceipt } from './types';
import { checkedPlan, planFingerprint } from './confirmation';
import { AntigravityProfileRegistry, credentialFingerprint, identityKey } from './registry';

export interface ManagedNativeUpdate {
  version(): Promise<string>;
  backup(): Promise<void>;
  update(): Promise<void>;
  /** Refuses every native executable replacement not owned by this update. */
  rollback(): Promise<boolean>;
}

/** Uses the account lock and the same proved resident PTY as account switching. */
export async function updateManagedAntigravity(deps: {
  registry: AntigravityProfileRegistry;
  driver: AntigravitySwitchDriver;
  native: ManagedNativeUpdate;
  now?: () => number;
}): Promise<{
  status: 'current' | 'updated' | 'failed' | 'restart_failed';
  previousVersion: string;
  version: string;
  restartedProcesses: number;
  updateAttempted: boolean;
}> {
  const { driver, registry, native } = deps;
  const before = await native.version();
  return registry.withLock(async () => {
    const failed = {
      status: 'failed' as const,
      previousVersion: before,
      version: before,
      restartedProcesses: 0,
      updateAttempted: false,
    };
    if (registry.hasRecovery()) return failed;
    const plan = checkedPlan(await driver.inspectProcesses());
    if (!plan.complete) return failed;
    if (!plan.processes.length) {
      let attempted = false;
      try {
        await native.backup();
        const latest = checkedPlan(await driver.inspectProcesses());
        if (
          !latest.complete ||
          latest.processes.length ||
          planFingerprint(latest) !== planFingerprint(plan)
        )
          return failed;
        attempted = true;
        await native.update();
        const after = await native.version();
        // No runtime was stopped or restarted. A changed native pin disables
        // managed launch/switching; ordinary native commands keep working.
        return {
          status: after === before ? 'current' : 'updated',
          previousVersion: before,
          version: after,
          restartedProcesses: 0,
          updateAttempted: true,
        };
      } catch {
        return { ...failed, updateAttempted: attempted };
      }
    }
    if (
      !(await driver.canProveRuntimeIdentity()) ||
      !driver.approveOwnedIdlePlan ||
      !driver.revalidateQuiescedStop ||
      !driver.completeActivation ||
      !driver.canRestartSupportedNative
    )
      return failed;
    const credential = await driver.readCurrentCredential();
    const current = await driver.validateCredential(credential);
    const id = registry.findIdentity(current);
    if (!id) return failed;
    if (
      !plan.continuity?.restorable ||
      !(await driver.approveOwnedIdlePlan(plan, current, credentialFingerprint(credential)))
    )
      return failed;
    let receipt: StopReceipt | undefined;
    let updated = false;
    let durable = false;
    let stopAttempted = false;
    registry.beginTransaction(id, id, (deps.now ?? Date.now)());
    try {
      await native.backup();
      const latest = checkedPlan(await driver.inspectProcesses());
      if (
        planFingerprint(latest) !== planFingerprint(plan) ||
        !(await driver.approveOwnedIdlePlan(latest, current, credentialFingerprint(credential)))
      )
        throw new AntigravityError('update-deferred');
      stopAttempted = true;
      receipt = await driver.stopProcesses(latest);
      if (
        !receipt.complete ||
        JSON.stringify(receipt.stopped.map((x) => JSON.stringify(x)).sort()) !==
          JSON.stringify(latest.processes.map((x) => JSON.stringify(x.identity)).sort())
      )
        throw new AntigravityError('update-stop-incomplete');
      const stoppedCredential = await driver.readCurrentCredential();
      if (
        identityKey(await driver.validateCredential(stoppedCredential)) !== identityKey(current) ||
        !(await driver.revalidateQuiescedStop(
          receipt,
          current,
          credentialFingerprint(stoppedCredential)
        ))
      )
        throw new AntigravityError('update-stop-unproved');
      updated = true;
      await native.update();
      const after = await native.version();
      // A changed binary invalidates its old proof/pin. Without verifiable
      // publication ownership, an unsupported update retains recovery instead
      // of overwriting the executable or claiming the old PTY was restored.
      if (!(await driver.canRestartSupportedNative()))
        throw new AntigravityError('update-native-proof-pending');
      await driver.restartProcesses(receipt);
      const proof = await driver.proveRuntimeIdentity(current);
      const live = await driver.readCurrentCredential();
      if (
        !proof.runtimeStarted ||
        !proof.sessionRestored ||
        identityKey(proof.identity) !== identityKey(current) ||
        proof.credentialFingerprint !== credentialFingerprint(live) ||
        identityKey(await driver.validateCredential(live)) !== identityKey(current)
      )
        throw new AntigravityError('update-runtime-unproved');
      registry.completeTransaction(id, proof.identity.verifiedAt);
      durable = true;
      await driver.completeActivation(current);
      return {
        status: after === before ? 'current' : 'updated',
        previousVersion: before,
        version: after,
        restartedProcesses: plan.processes.length,
        updateAttempted: true,
      };
    } catch {
      if (!stopAttempted) {
        registry.abortTransaction();
        return failed;
      }
      if (durable) {
        try {
          registry.beginTransaction(id, id, (deps.now ?? Date.now)());
          registry.markRecovery();
        } catch {
          /* Hold driver state. */
        }
        return { ...failed, status: 'restart_failed', updateAttempted: updated };
      }
      try {
        await driver.stopOwnedRestarts();
        if (updated && !(await native.rollback()))
          throw new AntigravityError('update-foreign-native');
        if (!receipt?.complete) throw new AntigravityError('update-stop-unavailable');
        await driver.restartProcesses(receipt);
        const proof = await driver.proveRuntimeIdentity(current);
        const live = await driver.readCurrentCredential();
        if (
          !proof.runtimeStarted ||
          !proof.sessionRestored ||
          identityKey(proof.identity) !== identityKey(current) ||
          proof.credentialFingerprint !== credentialFingerprint(live) ||
          identityKey(await driver.validateCredential(live)) !== identityKey(current)
        )
          throw new AntigravityError('update-recovery-unproved');
        registry.completeTransaction(id, proof.identity.verifiedAt);
        durable = true;
        await driver.completeActivation(current);
        return { ...failed, updateAttempted: updated };
      } catch {
        try {
          if (durable) registry.beginTransaction(id, id, (deps.now ?? Date.now)());
          registry.markRecovery();
        } catch {
          /* Existing intent/driver state remains blocked. */
        }
        return { ...failed, status: 'restart_failed', updateAttempted: updated };
      }
    }
  });
}
