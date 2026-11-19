import { createHash, timingSafeEqual } from 'crypto';
import fs from 'fs';
import path from 'path';
import { runLifecycleEffects } from '../web-server/services/lifecycle-effect-pipe';
import {
  checkedIdentity,
  credentialFingerprint,
  identityKey,
  type AntigravityProfileRegistry,
  type SavedProfileSnapshot,
} from './registry';
import type { NativeCredential, ProfileDto, VerifiedIdentity } from './types';

/**
 * Sign in again on the live Antigravity login (switching was removed on
 * 2026-11-13, so the live login signs in again in place). Called by
 * AntigravityAccountLifecycle.importSignIn under the registry lock, after the
 * new credential was verified as the profile's own Google identity.
 */

const MAX_CREDENTIAL_BYTES = 16_384;
/** The native fallback credential file under the user's home. */
export const ANTIGRAVITY_NATIVE_TOKEN = path.join(
  '.gemini',
  'antigravity-cli',
  'antigravity-oauth-token'
);

/** Whether the live native login file exists under `home`; a check that cannot run throws. */
export function liveLoginFileExists(home: string): boolean {
  try {
    fs.lstatSync(path.join(home, ANTIGRAVITY_NATIVE_TOKEN));
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
    throw error;
  }
}

/** The live native login, as the lifecycle reaches it. */
export interface LiveLoginAccess {
  /** Whether the live login file exists; a check that cannot run throws. */
  exists(): boolean;
  read(): Promise<NativeCredential>;
  validate(credential: NativeCredential): Promise<VerifiedIdentity>;
  /** Installs only while the live login still has `expected` (an atomic exchange). */
  install(credential: NativeCredential, expected: string): Promise<void>;
}

/**
 * The live native login's fingerprint when it holds this profile's account,
 * read fresh now, or null to leave it alone: no live login, one that cannot
 * be read, or another account. It is this account when its bytes are the
 * profile's saved credential, when Google verifies it as the profile's
 * identity (the removal guard's two rules), or, when Google cannot verify it
 * (an expired or revoked login, the usual reason to sign in again), when it
 * holds the same Google grant (refresh token) as the saved credential. The new
 * credential was already verified as the profile's identity, so each rule
 * keeps the live login on the same Google account.
 */
export async function liveLoginToRenew(
  live: LiveLoginAccess,
  registry: AntigravityProfileRegistry,
  profile: ProfileDto
): Promise<string | null> {
  let native: NativeCredential;
  try {
    if (!live.exists()) return null;
    native = await live.read();
  } catch {
    return null;
  }
  const fingerprint = credentialFingerprint(native);
  if (fingerprint === registry.savedRevision(profile.id)) return fingerprint;
  try {
    const identity = checkedIdentity(await live.validate(native));
    return identityKey(identity) === profile.identityKey ? fingerprint : null;
  } catch {
    // Google could not verify it: only the same grant still names this account.
  }
  let saved: NativeCredential;
  try {
    saved = registry.readCredential(profile.id, 'ubuntu').credential;
  } catch {
    return null;
  }
  return sameGrant(native, saved) ? fingerprint : null;
}

/**
 * Commit a Sign in again that renews the live login too: the saved profile
 * first (`save`), then the live login through the guarded store's atomic
 * exchange, which refuses a live login that changed since it was read. When
 * the live install fails, the live login is read again: if it already holds
 * the new credential, both are updated; otherwise the previous profile record
 * goes back (write_failed, nothing changed). Only when that also fails does
 * the sign-in report live_login_not_updated: the profile has the new
 * credential, the live login the old one, both the same Google account.
 * `save` throws when the profile itself cannot be saved (nothing changed).
 */
export async function renewLiveLogin(
  live: LiveLoginAccess,
  registry: AntigravityProfileRegistry,
  previous: SavedProfileSnapshot,
  credential: NativeCredential,
  expected: string,
  save: () => void,
  root: string
): Promise<'renewed' | 'write_failed' | 'live_login_not_updated'> {
  let restored = false;
  const renewed = await runLifecycleEffects(
    root,
    ['--antigravity-renew-login'],
    ['save', 'install', 'verify', 'restore'] as const,
    async (effect) => {
      switch (effect) {
        case 'save':
          save();
          return true;
        case 'install':
          try {
            await live.install(credential, expected);
            return true;
          } catch {
            return false;
          }
        case 'verify':
          try {
            return credentialFingerprint(await live.read()) === credentialFingerprint(credential);
          } catch {
            return false;
          }
        case 'restore':
          try {
            registry.restoreProfile(previous);
            restored = true;
          } catch {
            restored = false;
          }
          return restored;
      }
    },
    async (perform) => {
      await perform('save');
      if ((await perform('install')) || (await perform('verify'))) return true;
      await perform('restore');
      return false;
    }
  );
  return renewed ? 'renewed' : restored ? 'write_failed' : 'live_login_not_updated';
}

/**
 * Both credentials hold the same Google grant: the same non-empty refresh
 * token, compared in constant time. Anything unparsable is not the same.
 */
export function sameGrant(left: NativeCredential, right: NativeCredential): boolean {
  const a = grantDigest(left);
  const b = grantDigest(right);
  return a !== null && b !== null && timingSafeEqual(a, b);
}

function grantDigest(credential: NativeCredential): Buffer | null {
  if (credential.bytes.length === 0 || credential.bytes.length > MAX_CREDENTIAL_BYTES) return null;
  try {
    const value = JSON.parse(credential.bytes.toString('utf8')) as unknown;
    const token =
      value !== null && typeof value === 'object'
        ? (value as { token?: unknown }).token
        : undefined;
    const refresh =
      token !== null && typeof token === 'object'
        ? (token as { refresh_token?: unknown }).refresh_token
        : undefined;
    return typeof refresh === 'string' && refresh.length > 0
      ? createHash('sha256').update(refresh, 'utf8').digest()
      : null;
  } catch {
    return null;
  }
}
