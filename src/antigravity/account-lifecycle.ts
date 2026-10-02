import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';
import { createUbuntuNativeCredentialStore } from './native-credential-transport';
import { createAntigravityQuotaWorker } from './quota-worker-transport';
import { antigravitySignInRunning } from './signin-marker';
import {
  AntigravityProfileRegistry,
  PrivateStorageError,
  checkedIdentity,
  credentialFingerprint,
  identityKey,
} from './registry';
import type { NativeCredential, ProfileDto, VerifiedIdentity } from './types';

/**
 * Antigravity Add, Sign in again and Remove (CONTRACT-registry-lifecycle 6.2,
 * 6.3 and 6.7) over the private saved-profile registry.
 *
 * - A sign-in never touches the live native login: the official CLI signs in
 *   inside an isolated home (signin-sandbox.ts), and only the credential it
 *   created is imported here, after a fresh provider identity check.
 * - Add refuses an identity already saved in another profile; Sign in again
 *   keeps the same Google account (email and subject) and refuses the profile
 *   that is the live native login, because a later switch saves the live login
 *   back into that profile.
 * - Remove deletes the registry snapshot only, under the registry's
 *   transaction lock. It never logs out and never touches shared history. The
 *   live native login's profile and the registry's runtime-verified active
 *   profile are refused, so a switch always has the current account saved.
 */
export const ANTIGRAVITY_PROFILE_NAME = /^[a-z][a-z0-9_-]{0,47}$/;
export const ANTIGRAVITY_MAX_PROFILES = 16;
/** The native fallback credential file under the user's home. */
export const ANTIGRAVITY_NATIVE_TOKEN = path.join(
  '.gemini',
  'antigravity-cli',
  'antigravity-oauth-token'
);
const IDENTITY_MAX_AGE_MS = 5 * 60_000;

export type AntigravityLifecycleCode =
  | 'account_active'
  | 'activation_running'
  | 'signin_running'
  | 'unknown_account'
  | 'remove_failed'
  | 'id_in_use'
  | 'too_many_accounts'
  | 'duplicate_identity'
  | 'identity_mismatch'
  | 'write_failed';

export class AntigravityLifecycleError extends Error {
  constructor(readonly code: AntigravityLifecycleCode) {
    super(code);
    this.name = 'AntigravityLifecycleError';
  }
}

export type AntigravityRemoveRefusal = 'account_active' | 'activation_running' | 'signin_running';

export interface AntigravityLifecycleDeps {
  ccsDir: () => string;
  /** The home whose `.gemini` holds the live native login. */
  home: () => string;
  validateCredential?: (credential: NativeCredential) => Promise<VerifiedIdentity>;
  /** The live native login; only read when its file exists. */
  readNativeCredential?: () => Promise<NativeCredential>;
  now?: () => number;
}

export interface AntigravitySignInResult {
  profileId: string;
  email: string;
  plan: string | null;
}

/** The terminal command a user runs on Ubuntu to add or sign in a profile again. */
export function antigravitySignInCommand(profileId: string): string {
  if (!ANTIGRAVITY_PROFILE_NAME.test(profileId))
    throw new AntigravityLifecycleError('write_failed');
  return `ai-account-center antigravity signin ${profileId}`;
}

function storageCode(error: unknown, fallback: AntigravityLifecycleCode): AntigravityLifecycleCode {
  if (error instanceof AntigravityLifecycleError) return error.code;
  if (error instanceof PrivateStorageError) {
    if (error.code === 'busy' || error.code === 'recovery-required') return 'activation_running';
    if (error.code === 'missing') return 'unknown_account';
  }
  return fallback;
}

export class AntigravityAccountLifecycle {
  private worker: ReturnType<typeof createAntigravityQuotaWorker> | null = null;

  constructor(private readonly deps: AntigravityLifecycleDeps) {}

  private now(): number {
    return (this.deps.now ?? Date.now)();
  }

  private ccsDir(): string {
    return path.resolve(this.deps.ccsDir());
  }

  /** The registry, only when it exists, opened read-only: reading never creates a folder. */
  private existingRegistry(): AntigravityProfileRegistry | null {
    return AntigravityProfileRegistry.openExisting(this.ccsDir());
  }

  private validate(credential: NativeCredential): Promise<VerifiedIdentity> {
    if (this.deps.validateCredential) return this.deps.validateCredential(credential);
    this.worker ??= createAntigravityQuotaWorker();
    return this.worker.validateCredential(credential);
  }

  private readNative(): Promise<NativeCredential> {
    if (this.deps.readNativeCredential) return this.deps.readNativeCredential();
    return createUbuntuNativeCredentialStore({ home: path.resolve(this.deps.home()) }).read();
  }

  nameError(name: unknown): boolean {
    return typeof name !== 'string' || !ANTIGRAVITY_PROFILE_NAME.test(name);
  }

  listProfiles(): ProfileDto[] {
    return this.existingRegistry()?.listProfiles() ?? [];
  }

  hasProfile(profileId: string): boolean {
    return this.listProfiles().some((profile) => profile.id === profileId);
  }

  profileCount(): number {
    return this.listProfiles().length;
  }

  /**
   * A live switch, import or remove holds the lock, or an unresolved switch
   * needs recovery. An abandoned lock does not count: the next Add, Sign in
   * again or Remove takes it over.
   */
  activationRunning(): boolean {
    const registry = this.existingRegistry();
    return registry !== null && (registry.lockHeldLive() || registry.hasRecovery());
  }

  /** A terminal sign-in for this profile is running now. */
  signInRunning(profileId: string): boolean {
    return !this.nameError(profileId) && antigravitySignInRunning(this.ccsDir(), profileId);
  }

  /** The registry's last runtime-verified active profile, if any. */
  runtimeActiveProfileId(): string | null {
    return (
      this.listProfiles().find((profile) => profile.hosts.some((host) => host.active))?.id ?? null
    );
  }

  /**
   * Whether this saved profile is the live native login, checked now: the same
   * credential bytes the registry recorded for it, else the same verified
   * Google identity. The saved snapshot file is not read, so a missing or
   * corrupt one never blocks the check. No native login file means no profile
   * is live. A check that cannot run throws.
   */
  async isLiveNativeProfile(profileId: string): Promise<boolean> {
    const registry = this.existingRegistry();
    if (!registry) return false;
    const profile = registry.listProfiles().find((entry) => entry.id === profileId);
    if (!profile) return false;
    try {
      fs.lstatSync(path.join(path.resolve(this.deps.home()), ANTIGRAVITY_NATIVE_TOKEN));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
      throw error;
    }
    const native = await this.readNative();
    if (credentialFingerprint(native) === registry.savedRevision(profileId)) return true;
    const identity = checkedIdentity(await this.validate(native));
    return identityKey(identity) === profile.identityKey;
  }

  /**
   * The first refusal a Remove would hit now. `fresh` also asks the live
   * native login (a provider identity check); without it only the registry's
   * own records and the caller's `liveHint` count, for the page listing.
   */
  async removeRefusal(
    profileId: string,
    options: { signinRunning: boolean; fresh: boolean; liveHint?: boolean }
  ): Promise<AntigravityRemoveRefusal | null> {
    if (this.activationRunning()) return 'activation_running';
    if (this.runtimeActiveProfileId() === profileId || options.liveHint === true)
      return 'account_active';
    if (options.fresh && (await this.isLiveNativeProfile(profileId))) return 'account_active';
    if (options.signinRunning || this.signInRunning(profileId)) return 'signin_running';
    return null;
  }

  /** The reviewed state a Remove confirmation is bound to; no secret or identity value. */
  removeFingerprint(profileId: string): string {
    const profiles = this.listProfiles();
    const profile = profiles.find((entry) => entry.id === profileId) ?? null;
    let revision: string | null = null;
    try {
      revision = profile
        ? (this.existingRegistry()?.readCredential(profileId, 'ubuntu').credentialRevision ?? null)
        : null;
    } catch {
      revision = null;
    }
    return createHash('sha256')
      .update(
        JSON.stringify({
          profile: profile ? { id: profile.id, email: profile.email } : null,
          revision,
          active: this.runtimeActiveProfileId(),
          profiles: profiles.map((entry) => entry.id).sort(),
        })
      )
      .digest('hex');
  }

  /** Delete the saved snapshot under the registry lock; refusals are checked again there. */
  async remove(profileId: string): Promise<{ leftInPlace: number }> {
    if (!this.existingRegistry()) throw new AntigravityLifecycleError('unknown_account');
    let registry: AntigravityProfileRegistry;
    try {
      registry = new AntigravityProfileRegistry(this.ccsDir());
    } catch {
      throw new AntigravityLifecycleError('remove_failed');
    }
    try {
      return await registry.withLock(async () => {
        if (registry.hasRecovery()) throw new AntigravityLifecycleError('activation_running');
        if (!registry.listProfiles().some((profile) => profile.id === profileId))
          throw new AntigravityLifecycleError('unknown_account');
        if (this.runtimeActiveProfileId() === profileId)
          throw new AntigravityLifecycleError('account_active');
        if (this.signInRunning(profileId)) throw new AntigravityLifecycleError('signin_running');
        let live: boolean;
        try {
          live = await this.isLiveNativeProfile(profileId);
        } catch {
          throw new AntigravityLifecycleError('remove_failed');
        }
        if (live) throw new AntigravityLifecycleError('account_active');
        return registry.removeProfile(profileId);
      });
    } catch (error) {
      throw new AntigravityLifecycleError(storageCode(error, 'remove_failed'));
    }
  }

  /**
   * Maintenance: delete the credential files of profile folders no saved
   * profile names (a crash between a Remove's registry revision and its file
   * deletion, or a failed Add). Skipped while the lock is held.
   */
  async sweepOrphans(): Promise<{ removed: number; leftInPlace: number }> {
    if (!this.existingRegistry()) return { removed: 0, leftInPlace: 0 };
    const registry = new AntigravityProfileRegistry(this.ccsDir());
    try {
      return await registry.withLock(async () => registry.sweepOrphanedInstances());
    } catch (error) {
      if (error instanceof PrivateStorageError && error.code === 'busy')
        return { removed: 0, leftInPlace: 0 };
      throw error;
    }
  }

  /**
   * Import a credential the isolated sign-in created. Add saves a new profile;
   * Sign in again replaces a profile's credential with the same Google account.
   * The identity is verified with the provider under the registry lock.
   */
  async importSignIn(options: {
    profileId: string;
    mode: 'add' | 'signin-again';
    credential: NativeCredential;
  }): Promise<AntigravitySignInResult> {
    const { profileId, mode, credential } = options;
    if (this.nameError(profileId)) throw new AntigravityLifecycleError('write_failed');
    let registry: AntigravityProfileRegistry;
    try {
      registry = new AntigravityProfileRegistry(this.ccsDir());
    } catch {
      throw new AntigravityLifecycleError('write_failed');
    }
    try {
      return await registry.withLock(async () => {
        if (registry.hasRecovery()) throw new AntigravityLifecycleError('activation_running');
        const profiles = registry.listProfiles();
        const existing = profiles.find((profile) => profile.id === profileId);
        if (mode === 'add') {
          if (existing) throw new AntigravityLifecycleError('id_in_use');
          if (profiles.length >= ANTIGRAVITY_MAX_PROFILES)
            throw new AntigravityLifecycleError('too_many_accounts');
        } else if (!existing) {
          throw new AntigravityLifecycleError('unknown_account');
        }
        let identity: VerifiedIdentity;
        try {
          identity = checkedIdentity(await this.validate(credential));
        } catch {
          throw new AntigravityLifecycleError('write_failed');
        }
        const age = this.now() - Date.parse(identity.verifiedAt);
        if (!(age <= IDENTITY_MAX_AGE_MS && age >= -60_000))
          throw new AntigravityLifecycleError('write_failed');
        const key = identityKey(identity);
        if (mode === 'add') {
          if (
            profiles.some(
              (profile) => profile.identityKey === key || profile.email === identity.email
            )
          )
            throw new AntigravityLifecycleError('duplicate_identity');
        } else {
          if (existing?.identityKey !== key)
            throw new AntigravityLifecycleError('identity_mismatch');
          if (this.runtimeActiveProfileId() === profileId)
            throw new AntigravityLifecycleError('account_active');
          let live: boolean;
          try {
            live = await this.isLiveNativeProfile(profileId);
          } catch {
            throw new AntigravityLifecycleError('write_failed');
          }
          if (live) throw new AntigravityLifecycleError('account_active');
        }
        try {
          registry.saveCredential(profileId, 'ubuntu', credential, identity, this.now());
        } catch {
          throw new AntigravityLifecycleError('write_failed');
        }
        return { profileId, email: identity.email, plan: identity.plan };
      });
    } catch (error) {
      throw new AntigravityLifecycleError(storageCode(error, 'write_failed'));
    }
  }
}
