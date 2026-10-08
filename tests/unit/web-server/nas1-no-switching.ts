/**
 * The no-switching rule for Nas1, shared by the update guard
 * (app-update-nas1.test.ts) and the analytics guard (analytics-remote-nas1.test.ts)
 * so both check the same words. Nas1 only updates apps and is scanned for
 * analytics: nothing the dashboard sends it may activate or switch an account,
 * run the key store or the Claude desktop collector, or name a login file.
 */
export const NAS1_FORBIDDEN_WORDS = [
  'activate',
  'switch',
  'key_store',
  'claude_usage',
  'auth.json',
  '.credentials.json',
] as const;

/** The forbidden words found anywhere in one ssh argv (empty when it is clean). */
export function forbiddenWordsIn(argv: readonly string[]): string[] {
  const text = argv.join(' ').toLowerCase();
  return NAS1_FORBIDDEN_WORDS.filter((word) => text.includes(word));
}
