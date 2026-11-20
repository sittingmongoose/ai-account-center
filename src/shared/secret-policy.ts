/** Shared AAC refusal policy. Status keeps compatibility exports; new callers import here. */
import packagedPolicy from '../../config/secret-policy.json';
import { ValidationError } from '../errors/error-types';
import * as path from 'path';
import { loadNativeStatus, nativeStatusModuleName } from '../status-native/native-loader';

export const SECRET_POLICY_PATH = 'config/secret-policy.json';
export type SecretPolicy = typeof packagedPolicy;

/** Return an independent copy so consumers cannot mutate the detector's cached rules. */
export function loadSecretPolicy(): SecretPolicy {
  return structuredClone(packagedPolicy);
}

/*
 * The one secret filter is Rust (owner, 2026-11-19): `native/shared/secret_policy.rs` and
 * `crates/aac-status/src/typed_secrets.rs`, compiled into the aac-status native module and
 * called here synchronously until the TypeScript server is cut. Its verdicts are byte-equal to
 * the last TypeScript implementation's (tests/spec/secret-filter.json). Without the module
 * nothing is accepted unchecked: `findSecret` and `findTypedSecret` name the text as unchecked
 * and `findSecretIn` throws, which every file reader turns into "could not be checked for
 * secrets".
 */

interface SecretFilter {
  findSecret(text: string): string | null | undefined;
  findSecretIn(source: string, valueJson: string): string | null | undefined;
  findTypedSecret(text: string): string | null | undefined;
}

/** The kind named when the filter cannot run: the text is refused, never passed unchecked. */
export const UNCHECKED_SECRET_KIND = 'text the secret filter could not check';

let filter: SecretFilter | null | undefined;

/** The native filter, loaded once (null when the aac-status module is missing or too old). */
export function secretFilter(): SecretFilter | null {
  if (filter === undefined) {
    // The package's dist/native, from src/shared (tests run from source) and dist/shared alike. The readers'
    // switch (AAC_STATUS_NATIVE=off) does not apply: the filter is not optional.
    const loaded = loadNativeStatus({
      env: { ...process.env, AAC_STATUS_NATIVE: undefined },
      file:
        process.env.AAC_STATUS_MODULE ??
        path.resolve(__dirname, '..', '..', 'dist', 'native', nativeStatusModuleName()),
    });
    const module = loaded.module as unknown as Partial<SecretFilter> | null;
    filter =
      module &&
      typeof module.findSecret === 'function' &&
      typeof module.findSecretIn === 'function' &&
      typeof module.findTypedSecret === 'function'
        ? (module as SecretFilter)
        : null;
  }
  return filter;
}

/** Test seam: forget the loaded filter (the next call loads it again). */
export function resetSecretFilterForTests(): void {
  filter = undefined;
}

/** The kind of secret a text contains, or null. */
export function findSecret(text: string): string | null {
  const native = secretFilter();
  if (!native) return UNCHECKED_SECRET_KIND;
  try {
    return native.findSecret(text) ?? null;
  } catch {
    return UNCHECKED_SECRET_KIND;
  }
}

/**
 * The secret check for a parsed JSON file: over the text as written, then over every decoded key and string of the
 * parsed value (escapes, embedded JSON text, credential keys), as the reader sees it. Throws when the filter cannot
 * run; every caller turns that into a refusal.
 */
export function findSecretIn(source: string, value: unknown): string | null {
  const native = secretFilter();
  if (!native) throw new ValidationError('The secret filter is not available.');
  return native.findSecretIn(source, JSON.stringify(value) ?? 'null') ?? null;
}

/** serde_json's nesting limit: Rust reads 127 nested arrays or objects and refuses the 128th. */
const NATIVE_DEPTH = 127;
/** A UTF-16 surrogate without its partner: a high one not followed by a low one, or a lone low one. */
const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/;

/**
 * Whether a value `JSON.parse` returned comes from text Rust's parser (serde_json) refuses: a
 * number JavaScript turned into Infinity (an overflow such as `1e400`), a string or key holding a
 * lone surrogate (an unpaired `\ud800` escape), or arrays and objects nested deeper than serde's
 * limit. Such text counts as unparseable here too, so both languages refuse the same documents:
 * every TypeScript reader that parses before this policy's check applies it (Scoreboard v2,
 * recheck 2 M1). Iterative, like the secret walk.
 */
export function nativeUnreadable(value: unknown): boolean {
  const stack: Array<[unknown, number]> = [[value, 0]];
  for (let entry = stack.pop(); entry !== undefined; entry = stack.pop()) {
    const [node, depth] = entry;
    if (typeof node === 'number') {
      if (!Number.isFinite(node)) return true;
    } else if (typeof node === 'string') {
      if (LONE_SURROGATE.test(node)) return true;
    } else if (node && typeof node === 'object') {
      if (depth + 1 > NATIVE_DEPTH) return true;
      if (Array.isArray(node)) for (const item of node) stack.push([item, depth + 1]);
      else
        for (const [key, item] of Object.entries(node as Record<string, unknown>)) {
          if (LONE_SURROGATE.test(key)) return true;
          stack.push([item, depth + 1]);
        }
    }
  }
  return false;
}
