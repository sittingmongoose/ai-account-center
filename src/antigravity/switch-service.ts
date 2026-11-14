import { AntigravityError } from './errors';
import {
  AntigravityProfileRegistry,
  PrivateStorageError,
  assertUbuntu,
  checkedIdentity,
  credentialFingerprint,
  identityKey,
} from './registry';
import type {
  AntigravitySwitchDriver,
  NativeCredential,
  ProfileDto,
  VerifiedIdentity,
} from './types';

const IDENTITY_MAX_AGE = 5 * 60_000;

export type AntigravityRecoveryStatus =
  'completed' | 'aborted' | 'restored-previous' | 'recovery-required' | 'no-recovery-pending';

export interface AntigravityRecoveryResult {
  status: AntigravityRecoveryStatus;
  hostId: 'ubuntu';
  profileId?: string;
  email?: string;
}

interface ServiceOptions {
  registry: AntigravityProfileRegistry;
  driver: AntigravitySwitchDriver;
  now?: () => number;
}

/**
 * Saved Antigravity profiles on Ubuntu: the inventory, explicit import, and recovery of a switch an older version
 * left unfinished. Account switching itself was removed (2026-11-14). No error message or tool output from a driver
 * is forwarded to callers.
 */
export class AntigravitySwitchService {
  private readonly registry: AntigravityProfileRegistry;
  private readonly driver: AntigravitySwitchDriver;
  private readonly now: () => number;

  constructor(options: ServiceOptions) {
    assertUbuntu(options.driver.hostId);
    this.registry = options.registry;
    this.driver = options.driver;
    this.now = options.now ?? Date.now;
  }

  listProfiles(): ProfileDto[] {
    return this.registry.listProfiles();
  }

  /** Selected native credential and proven running account are deliberately distinct. */
  async readInventory(): Promise<ProfileDto[]> {
    const profiles = this.listProfiles();
    if (profiles.length === 0) return profiles;
    try {
      const identity = this.driver.readInventoryIdentity
        ? checkedIdentity(await this.driver.readInventoryIdentity())
        : await this.verified(await this.driver.readCurrentCredential());
      const age = this.now() - Date.parse(identity.verifiedAt);
      if (age > IDENTITY_MAX_AGE || age < -60_000)
        throw new AntigravityError('Identity proof is stale.');
      const selectedKey = identityKey(identity);
      return profiles.map((profile) => ({
        ...profile,
        hosts: profile.hosts.map((host) => ({
          ...host,
          selected: profile.identityKey === selectedKey,
          // A stored runtime proof never labels a different current native account active.
          active: host.active && profile.identityKey === selectedKey,
        })),
      }));
    } catch {
      return profiles.map((profile) => ({
        ...profile,
        hosts: profile.hosts.map((host) => ({
          ...host,
          selected: false,
          active: false,
        })),
      }));
    }
  }

  /** Import is explicit, identity-deduplicated and never changes the live native login. */
  async importNativeProfile(profileId: string, credential: NativeCredential): Promise<ProfileDto> {
    return this.registry.withLock(async () => {
      if (this.registry.hasRecovery()) throw new PrivateStorageError('recovery-required');
      const identity = await this.verified(credential);
      this.registry.saveCredential(profileId, 'ubuntu', credential, identity, this.now());
      const profile = this.listProfiles().find((entry) => entry.id === profileId);
      if (!profile) throw new PrivateStorageError('corrupt');
      return profile;
    });
  }

  private async verified(credential: NativeCredential): Promise<VerifiedIdentity> {
    const identity = checkedIdentity(await this.driver.validateCredential(credential));
    const age = this.now() - Date.parse(identity.verifiedAt);
    if (age > IDENTITY_MAX_AGE || age < -60_000)
      throw new AntigravityError('Identity proof is stale.');
    return identity;
  }

  /**
   * Finish or undo a stuck switch. The live login is re-proved through the
   * same live verification the switcher uses, then the transaction either
   * completes (live is the target) or aborts (live is the previous, or the
   * previous saved revision is restored and re-proved). Anything unproven
   * leaves recovery-required in place. Saved profiles are never rewritten.
   */
  async recover(): Promise<AntigravityRecoveryResult> {
    const result = (
      status: AntigravityRecoveryStatus,
      extra: Partial<AntigravityRecoveryResult> = {}
    ): AntigravityRecoveryResult => ({ status, hostId: 'ubuntu', ...extra });
    try {
      return await this.registry.withLock(async () => {
        const tx = this.registry.pendingTransaction();
        if (!tx) return result('no-recovery-pending');
        // Hygiene first: no owned writer may move the live login under
        // the proof. Any failure here keeps the transaction stuck.
        await this.driver.stopOwnedRestarts();
        const live = await this.driver.readCurrentCredential();
        const proven = await this.verified(live);
        const liveKey = identityKey(proven);
        const target = this.registry.readCredential(tx.targetId, 'ubuntu');
        const previous = this.registry.readCredential(tx.previousId, 'ubuntu');
        if (liveKey === identityKey(target.identity)) {
          this.registry.completeTransaction(tx.targetId, proven.verifiedAt);
          return result('completed', {
            profileId: tx.targetId,
            email: target.identity.email,
          });
        }
        if (liveKey === identityKey(previous.identity)) {
          this.registry.abortTransaction();
          return result('aborted', {
            profileId: tx.previousId,
            email: previous.identity.email,
          });
        }
        // Unknown live login: restore the previous saved revision only
        // when it still verifies, then re-prove before clearing anything.
        const previousIdentity = await this.verified(previous.credential);
        if (identityKey(previousIdentity) !== identityKey(previous.identity))
          return result('recovery-required');
        const receipt = await this.driver.installCredential(
          previous.credential,
          credentialFingerprint(live)
        );
        if (receipt.installedFingerprint !== credentialFingerprint(previous.credential))
          return result('recovery-required');
        const restored = await this.driver.readCurrentCredential();
        const reproven = await this.verified(restored);
        if (identityKey(reproven) !== identityKey(previous.identity))
          return result('recovery-required');
        this.registry.abortTransaction();
        return result('restored-previous', {
          profileId: tx.previousId,
          email: previous.identity.email,
        });
      });
    } catch (error) {
      if (error instanceof PrivateStorageError && error.code === 'busy') throw error;
      return result('recovery-required');
    }
  }
}
