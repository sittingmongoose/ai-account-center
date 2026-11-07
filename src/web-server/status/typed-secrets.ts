/**
 * The typed-secret check for the owner's notes (answers.ts): secrets as a person types them, by name and value.
 * The file-shaped secret patterns are in schema-helpers.ts.
 */

/**
 * A secret written the way a person types one in prose, a shell or code (the patterns above are shaped for files),
 * for the owner's notes, which go to the share. Found by its name, then judged by its value.
 * - Names: an identifier (`DB_PASSWORD`, `PGPASSWORD`, `API_KEY_2`, `clientSecret`, `x-api-key`), a short phrase
 *   (`the api key`, `db pass`), a flag (`--password`), `setx NAME`, before `=`, `:`, `=>`, `->`, `:=`, `"]=`, a
 *   quoted name's `,` (`setenv("PASSWORD", ...)`) or, after a phrase, "is"; then `Bearer`/`Basic`, a URL's password
 *   (`scheme://user:pw@`, the user may be empty), `-p<glued>` and `-u user:pw`. A name ending in a word like file,
 *   path, count, hash or manager names something else; `ssh key` and `GPG key` in prose are public ids.
 * - A bare `pass`, `pw` or `pwd` (or one in a phrase, `db pass`) is weaker evidence: never `PASS:` (a test result),
 *   and its value must hold a run of 6 or more mixing letters with digits or symbols, with no slash.
 * - Values: a reference or placeholder (`$VAR`, `%VAR%`, `<your-key>`), a URL, a date, a version (`v2.3.1`) or a
 *   `SHA256:` fingerprint is no secret; after "is", neither is `p95=320ms`. A path (from `~`, `.`, a drive or a
 *   system root, or `a/b.ext`) is split at slashes too and judged by its runs. After a password's name, any other
 *   value of 8 or more with a letter and a digit, or 8 or more digits, is one (`Summer_2026`, `Winter-2025!`);
 *   otherwise a run of 6 mixing letters with digits or symbols (`hunter2`). After a key's name, a run of 8.
 *   Runs are split at `-` `.` `_` `:`, so model names, versions and test names have none (`claude-opus-5-5`).
 * Each pattern is bounded and matches only the name, reading the value by lookahead, so a value is searched for
 * names in its turn, and the search stays linear.
 */
type SecretKind = 'password' | 'bare' | 'key';
const NAMED_VALUE =
  /(?<![A-Za-z0-9_-])((?:[A-Za-z][A-Za-z0-9]{0,23} )?\$?[A-Za-z_][A-Za-z0-9_-]{0,63})(['"]?\]?\s{0,4}(?:=>|->|:=|[:=])\s{0,4}|['"]\]?\s{0,4},\s{0,4}|\s{1,4}(?:is|was)\s{1,4})(?=['"`]?([^\s'"`]{1,256}))/g;
const SETX_VALUE =
  /(?<![A-Za-z0-9])setx\s{1,4}([A-Za-z_][A-Za-z0-9_]{0,63})\s{1,4}(?=['"]?([^\s'"]{1,256}))/gi;
const FLAG_VALUE =
  /(?<![A-Za-z0-9-])--([A-Za-z][A-Za-z0-9-]{0,47})(?:=|\s{1,4})(?=['"`]?([^\s'"`]{1,256}))/g;
const AUTH_VALUE = /(?<![A-Za-z0-9])(?:bearer|basic)\s{1,4}(?=([^\s'"`]{1,256}))/gi;
const URL_PASSWORD =
  /(?<![A-Za-z0-9+.-])[A-Za-z][A-Za-z0-9+.-]{0,31}:\/\/[^\s/:@]{0,64}:(?=([^\s/@]{1,256})@)/g;
const GLUED_PASSWORD = /(?<![A-Za-z0-9-])-p(?=([^\s'"=-][^\s'"]{0,255}))/g;
const USER_PASSWORD =
  /(?<![A-Za-z0-9-])(?:-u|--user)(?:\s{1,4}|=)['"]?[^\s:'"]{1,64}:(?=([^\s'"@]{1,256}))/g;
const NOT_A_SECRET_NAME = new Set(
  (
    'file files path dir url uri name names count limit length len size ttl type kind mode policy budget usage id ' +
    'ids at expires expiry env hash hashes manager scanning scanner fingerprint min max'
  ).split(' ')
);
const KEY_PAIR = new Set('api access private secret signing encryption master deploy'.split(' '));
const PATH_LIKE = new RegExp(
  '^(?:~[\\\\/]|\\.{1,2}[\\\\/]|[A-Za-z]:[\\\\/]|' +
    '[\\\\/](?:home|Users|etc|var|tmp|usr|opt|mnt|run|srv|root|Volumes|private|proc|dev|sys|bin|sbin|lib|lib64|' +
    'snap|media|nix|workspaces?|app|data|cygdrive)(?:[\\\\/]|$)|' +
    '[\\w.-]+(?:[\\\\/][\\w.-]+)+\\.[A-Za-z][A-Za-z0-9]{0,5}$)'
);

function secretKind(name: string, separator: string, flag: boolean): SecretKind | null {
  const phrase = name.includes(' ');
  const words = name
    .replace(/^\$/, '')
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .toLowerCase()
    .split(/[\s_-]+/)
    .filter(Boolean);
  if (words.length === 0 || NOT_A_SECRET_NAME.has(words[words.length - 1])) return null;
  if (words.some((word) => /(?:password|passwd|passphrase)$/.test(word))) return 'password';
  if (words.some((word) => word === 'pass' || word === 'pw' || word === 'pwd')) {
    if (flag || (!phrase && words.length > 1)) return 'password';
    // `PASS: TestLogin2FA` is a test result
    if (!(name === 'PASS' && separator === ':')) return 'bare';
  }
  const glued = phrase
    ? /^(?:secret|token|credentials?)$/
    : /(?:secret|token|credentials?|apikey|accesskey|privatekey|signingkey|secretkey)$/;
  if (words.some((word) => glued.test(word))) return 'key';
  for (let at = 1; at < words.length; at += 1)
    if (words[at] === 'key' && (!phrase || KEY_PAIR.has(words[at - 1]))) return 'key';
  return null;
}

/** The longest run, between `separators`, that mixes letters with digits or symbols. */
function longestMixedRun(value: string, separators: RegExp): number {
  let longest = 0;
  for (const run of value.split(separators))
    if (/[A-Za-z]/.test(run) && /[0-9!@#$%^&*+=?~|]/.test(run))
      longest = Math.max(longest, run.length);
  return longest;
}

/** Whether a value after a secret's name looks like one (see above). */
function secretValue(kind: SecretKind, raw: string, afterIs = false): boolean {
  if (/^(?:\$\{?[A-Za-z_][A-Za-z0-9_]*\}?|%[A-Za-z_][A-Za-z0-9_]*%|<[^>]*>?|\{\{.*)$/.test(raw))
    return false;
  const value = raw.replace(/^[([]+/, '').replace(/[)\]}>.,;:!?]+$/, '');
  if (
    /^[A-Za-z][A-Za-z0-9+.-]*:\/\//.test(value) ||
    /^\d{4}-\d{2}-\d{2}/.test(value) ||
    /^v?\d+(?:\.\d+)+(?:[-+][\w.]+)?$/.test(value) ||
    /^(?:SHA256|SHA1|MD5):/i.test(value) ||
    (afterIs && value.includes('='))
  )
    return false;
  if (PATH_LIKE.test(value))
    return kind !== 'bare' && longestMixedRun(value, /[-._:\\/]+/) >= (kind === 'key' ? 8 : 6);
  if (kind === 'key') return longestMixedRun(value, /[-._:]+/) >= 8;
  if (kind === 'bare') return !/[\\/]/.test(value) && longestMixedRun(value, /[-._:]+/) >= 6;
  // a password: human ones are a word and a number (`Summer_2026`), so the whole value counts
  if (/^\d{8,}$/.test(value)) return true;
  if (value.length >= 8 && /[A-Za-z]/.test(value) && /\d/.test(value)) return true;
  return longestMixedRun(value, /[-._:]+/) >= 6;
}

export function findTypedSecret(text: string): string | null {
  const found = (kind: SecretKind | null, value: string | undefined, afterIs = false) =>
    kind !== null && value !== undefined && secretValue(kind, value, afterIs);
  for (const match of text.matchAll(NAMED_VALUE)) {
    const separator = match[2].replace(/['"\]\s]/g, '');
    const afterIs = separator === 'is' || separator === 'was';
    if (found(secretKind(match[1], separator, false), match[3], afterIs))
      return 'a password or key';
  }
  for (const match of text.matchAll(SETX_VALUE))
    if (found(secretKind(match[1], '=', false), match[2])) return 'a password or key';
  for (const match of text.matchAll(FLAG_VALUE))
    if (found(secretKind(match[1], '=', true), match[2])) return 'a password or key';
  for (const pattern of [AUTH_VALUE, URL_PASSWORD, GLUED_PASSWORD, USER_PASSWORD])
    for (const match of text.matchAll(pattern))
      if (found('password', match[1])) return 'a password or key';
  return null;
}
