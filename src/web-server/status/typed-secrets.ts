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
 *   (`scheme://user:pw@`, the user may be empty), `-p<glued>`, `-p <value>` after `sshpass` or a registry's `login`, and
 *   `-u user:pw`. A name ending in a word like file, path, count, hash, manager, ref or endpoint names something
 *   else; `ssh key` and `GPG key` in prose are public ids; `cache_key`, `sort_key` and their kind are lookup keys.
 * - A bare `pass`, `pw` or `pwd` (or one in a phrase, `db pass`) is weaker evidence: never `PASS:` (a test result),
 *   and no slash; its value is a secret with a run of 6 or more mixing letters with digits or symbols, or, as a
 *   human password (8 or more with a letter and a digit), after `=` or with a capital or a symbol (`Summer_2026`,
 *   not `test_auth_2`), unless it reads as a test or step name (`TestAuth_2`, `Step_2_Done`).
 * - Values: a reference or placeholder (`$VAR`, `%VAR%`, `<your-key>`), a URL, a date, a version (`v2.3.1`), a
 *   `SHA256:` fingerprint is no secret; a list of settings (`id=x,src=y`) is judged setting by setting; after "is",
 *   never a value with `=` (`p95=320ms`), nor one under 8 that more words follow (`was wrong3 times`). A path
 *   (from `~`, `.`, a drive or a
 *   system root, or `a/b.ext`) is split at slashes too and judged by its runs. After a password's name, any other
 *   value of 8 or more with a letter and a digit, or 8 or more digits, is one (`Summer_2026`, `Winter-2025!`);
 *   otherwise a run of 6 mixing letters with digits or symbols (`hunter2`). After a key's name, a run of 8.
 *   Runs are split at `-` `.` `_` `:`, so model names, versions and test names have none (`claude-opus-5-5`).
 * Each pattern is bounded and matches only the name, reading the value by lookahead, so a value is searched for
 * names in its turn, and the search stays linear.
 * Known limits: a letters-only password (`correcthorsebatterystaple`), a quoted value with a space, a name with no
 * separator (`password Summer_2026`), `PASS: <password>` (read as a test result) and a short password after "is"
 * that more words follow are not found; a bare `pass` before a word-and-number name (`pass=build_2026`,
 * `pass: 2FA_Login`) is refused.
 */
type SecretKind = 'password' | 'bare' | 'key';
const NAMED_VALUE =
  /(?<![A-Za-z0-9_-])((?:[A-Za-z][A-Za-z0-9]{0,23} )?\$?[A-Za-z_][A-Za-z0-9_-]{0,63})(['"]?\]?\s{0,4}(?:=>|->|:=|[:=])\s{0,4}|['"]\]?\s{0,4},\s{0,4}|\s{1,4}(?:is|was)\s{1,4})(?=['"`]?([^\s'"`]{1,256})['"`]?([^\S\n]{1,4}[A-Za-z])?)/g;
const SETX_VALUE =
  /(?<![A-Za-z0-9])setx\s{1,4}([A-Za-z_][A-Za-z0-9_]{0,63})\s{1,4}(?=['"]?([^\s'"]{1,256}))/gi;
const FLAG_VALUE =
  /(?<![A-Za-z0-9-])--([A-Za-z][A-Za-z0-9-]{0,47})(?:=|\s{1,4})(?=['"`]?([^\s'"`]{1,256}))/g;
const AUTH_VALUE = /(?<![A-Za-z0-9])(?:bearer|basic)\s{1,4}(?=([^\s'"`]{1,256}))/gi;
const URL_PASSWORD =
  /(?<![A-Za-z0-9+.-])[A-Za-z][A-Za-z0-9+.-]{0,31}:\/\/[^\s/:@]{0,64}:(?=([^\s/@]{1,256})@)/g;
const GLUED_PASSWORD = /(?<![A-Za-z0-9-])-p(?=([^\s'"=-][^\s'"]{0,255}))/g;
// within one command: spaces and tabs only, never a newline or a shell separator
const LOGIN_PASSWORD =
  /(?<![A-Za-z0-9-])(?:sshpass|(?:docker|podman|nerdctl|helm|oras|buildah|skopeo|oc|crane|regctl|az|cf)(?:[^\S\n]{1,4}[a-z]{1,16}){0,2}[^\S\n]{1,4}login)(?=(?:[^\S\n]{1,4}[^\s;|&]{1,128}){0,8}?[^\S\n]{1,4}-p[^\S\n]{1,4}['"]?([^\s;|&'"]{1,256}))/g;
const USER_PASSWORD =
  /(?<![A-Za-z0-9-])(?:-u|--user)(?:\s{1,4}|=)['"]?[^\s:'"]{1,64}:(?=([^\s'"@]{1,256}))/g;
const NOT_A_SECRET_NAME = new Set(
  (
    'file files path dir url uri name names count limit length len size ttl type kind mode policy budget usage id ' +
    'ids at expires expiry env hash hashes manager scanning scanner fingerprint min max ref refs endpoint helper'
  ).split(' ')
);
const KEY_PAIR = new Set('api access private secret signing encryption master deploy'.split(' '));
/** Words before `key` that make it a lookup key, not a secret (`cache_key`, `sort_key`). */
const LOOKUP_KEY = new Set(
  'cache sort partition primary foreign idempotency dedupe dedup hot row shard routing lookup map group index object'.split(
    ' '
  )
);
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
    if (
      words[at] === 'key' &&
      !LOOKUP_KEY.has(words[at - 1]) &&
      (!phrase || KEY_PAIR.has(words[at - 1]))
    )
      return 'key';
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
function secretValue(kind: SecretKind, raw: string, separator = '=', more = false): boolean {
  const afterIs = separator === 'is' || separator === 'was';
  if (/^(?:\$\{?[A-Za-z_][A-Za-z0-9_]*\}?|%[A-Za-z_][A-Za-z0-9_]*%|<[^>]*>?|\{\{.*)$/.test(raw))
    return false;
  const value = raw.replace(/^[([]+/, '').replace(/[)\]}>.,;:!?]+$/, '');
  if (
    /^[A-Za-z][A-Za-z0-9+.-]*:\/\//.test(value) ||
    /^\d{4}-\d{2}-\d{2}/.test(value) ||
    /^v?\d+(?:\.\d+)+(?:[-+][\w.]+)?$/.test(value) ||
    /^(?:SHA256|SHA1|MD5):/i.test(value) ||
    (afterIs && (value.includes('=') || (value.length < 8 && more)))
  )
    return false;
  // a list of settings (`id=x,src=y`): each setting's value is judged on its own
  if (/^[A-Za-z_][\w-]*=[^,]*(?:,[A-Za-z_][\w-]*=[^,]*)+$/.test(value))
    return value
      .split(',')
      .some((setting) => secretValue(kind, setting.slice(setting.indexOf('=') + 1), separator));
  if (PATH_LIKE.test(value))
    return kind !== 'bare' && longestMixedRun(value, /[-._:\\/]+/) >= (kind === 'key' ? 8 : 6);
  if (kind === 'key') return longestMixedRun(value, /[-._:]+/) >= 8;
  if (kind === 'bare') {
    if (/[\\/]/.test(value)) return false;
    if (longestMixedRun(value, /[-._:]+/) >= 6) return true;
    const human = value.length >= 8 && /[A-Za-z]/.test(value) && /\d/.test(value);
    // a test or step name (`TestAuth_2`, `Step_2_Done`, `round_16_ok`) is no password; a password ends in a digit
    // or a symbol (`Summer_Winter_2026`)
    const words = value.split(/[_-]+/).filter((part) => /^[A-Za-z]+$/.test(part)).length;
    const testName =
      /^(?:test|step|case|stage|phase)/i.test(value) || (words >= 2 && /[A-Za-z]$/.test(value));
    if (!human || testName) return false;
    return separator === '=' || /[A-Z]/.test(value) || /[^A-Za-z0-9_.-]/.test(value);
  }
  // a password: human ones are a word and a number (`Summer_2026`), so the whole value counts
  if (/^\d{8,}$/.test(value)) return true;
  if (value.length >= 8 && /[A-Za-z]/.test(value) && /\d/.test(value)) return true;
  return longestMixedRun(value, /[-._:]+/) >= 6;
}

export function findTypedSecret(text: string): string | null {
  const found = (
    kind: SecretKind | null,
    value: string | undefined,
    separator = '=',
    more = false
  ) => kind !== null && value !== undefined && secretValue(kind, value, separator, more);
  for (const match of text.matchAll(NAMED_VALUE)) {
    const separator = match[2].replace(/['"\]\s]/g, '');
    if (found(secretKind(match[1], separator, false), match[3], separator, match[4] !== undefined))
      return 'a password or key';
  }
  for (const match of text.matchAll(SETX_VALUE))
    if (found(secretKind(match[1], '=', false), match[2])) return 'a password or key';
  for (const match of text.matchAll(FLAG_VALUE))
    if (found(secretKind(match[1], '=', true), match[2])) return 'a password or key';
  for (const pattern of [AUTH_VALUE, URL_PASSWORD, GLUED_PASSWORD, LOGIN_PASSWORD, USER_PASSWORD])
    for (const match of text.matchAll(pattern))
      if (found('password', match[1])) return 'a password or key';
  return null;
}
