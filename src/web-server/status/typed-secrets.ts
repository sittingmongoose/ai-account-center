/**
 * The typed-secret check for the owner's notes (answers.ts): secrets as a person types them, by name and value.
 * The rules live in Rust (`crates/aac-status/src/typed_secrets.rs`, whose doc comment is their specification); this
 * calls the one filter through the native module. Without it a note is refused as unchecked, never sent unchecked.
 */
import { secretFilter, UNCHECKED_SECRET_KIND } from '../../shared/secret-policy';

export function findTypedSecret(text: string): string | null {
  const native = secretFilter();
  if (!native) return UNCHECKED_SECRET_KIND;
  try {
    return native.findTypedSecret(text) ?? null;
  } catch {
    return UNCHECKED_SECRET_KIND;
  }
}
