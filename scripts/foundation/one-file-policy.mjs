#!/usr/bin/env node
// L2 gate (DESIGN-M3-COMPLETE section 2.2): one file policy. Outside crates/aac-files, no Rust
// source opens, links, renames or writes a file by itself: no O_NOFOLLOW, custom_flags,
// OpenOptions, File::open, File::create, fs::read, fs::read_to_string, fs::write, fs::rename or
// tempfile persist (`fs::read` follows links and blocks on a FIFO swapped in after a check).
// Readers use aac_files::read, writers aac_files::OwnedStore. Test modules, tests/ and build
// scripts are not scanned. Each allowlisted file carries its reason, and an entry that no longer
// matches anything fails the gate, so the list cannot rot. `--self-test` proves the scan catches
// a planted copy.
import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = fileURLToPath(new URL('../..', import.meta.url));

export const PATTERNS = [
  ['O_NOFOLLOW', /\bO_NOFOLLOW\b/],
  ['custom_flags', /\bcustom_flags\s*\(/],
  ['OpenOptions', /\bOpenOptions\b/],
  ['File::open', /\bFile::open\s*\(/],
  ['File::create', /\bFile::create\s*\(/],
  ['fs::read', /\bfs::read\s*\(/],
  ['fs::read_to_string', /\bfs::read_to_string\s*\(/],
  ['fs::write', /\bfs::write\s*\(/],
  ['fs::rename', /\bfs::rename\s*\(/],
  ['persist', /\.persist(?:_noclobber)?\s*\(/],
];

/** Files that keep their own mechanics, each with the reason written next to it. */
export const ALLOWLIST = [
  {
    path: 'crates/aac-store/src/',
    reason:
      "The store engine's own files (redb, log segments, staged sets, backups). The engine is " +
      'their single writer, held by its kernel lock store/store.lock; every family is declared ' +
      'in crates/aac-store/registry.json, which the store checks at open. The store host (R3) ' +
      'moves them as one unit with its own switch.',
  },
  {
    path: 'native/aac-usage/src/fsx.rs',
    reason:
      "The scanner's walker: one nonblocking open per tool log while it walks very large trees " +
      'under its own depth, count and byte bounds (big-review search bound). Tool logs are not ' +
      'AAC private files, and the cursor needs the descriptor.',
  },
  {
    path: 'native/aac-usage/src/read/omp.rs',
    reason:
      "Scanner reader of OMP's own manifest and session files (another tool's data), reached " +
      "through the walker's admission. aac-usage is a native crate with its own lock; moving " +
      'its readers onto aac-files is follow-up l2-usage-readers.',
  },
  {
    path: 'native/aac-usage/src/read/claude.rs',
    reason:
      "Scanner reader comparing two of Claude Code's own transcripts (another tool's data), " +
      "reached through the walker's admission. Follow-up l2-usage-readers.",
  },
  {
    path: 'native/aac-usage/src/collector.rs',
    reason:
      "Scanner collector reading a tool database file named by the walker (another tool's " +
      'data). Follow-up l2-usage-readers.',
  },
  {
    path: 'native/aac-cursor-account/src/keys.rs',
    reason:
      'The Cursor key writer of the NAPI module (account-usage/keys/cursor-*, registry row ' +
      'account-keys, owner ts in process). A native crate with its own lock and build; it ' +
      'stages with create_new 0600, fsyncs and renames. Its load opens with File::open after ' +
      'an lstat (a race the shared read closes). Follow-up l2-cursor-keys: depend on aac-files.',
  },
  {
    path: 'native/aac-cursor-account/src/billing/mod.rs',
    reason:
      "The Cursor billing reader's checkpoint database (cache/cursor-billing-v1.sqlite): it creates " +
      'the file create_new 0600 and rejects a non-regular path before SQLite opens it. A native crate ' +
      'with its own build and no aac-files dependency yet. Follow-up l2-cursor-billing-store: move the ' +
      'checkpoint into aac-files / the redb store (the Rust cut keeps every cache in the store).',
  },
  {
    path: 'native/aac-cursor-account/src/billing/signin.rs',
    reason:
      "Reads Cursor's own CLI sign-in file (another tool's data, never written), capped at MAX_AUTH " +
      'bytes, like the scanner readers. Follow-up l2-cursor-billing-store: route through aac-files ' +
      'once the crate depends on it.',
  },
  {
    path: 'crates/aac-http/src/identity.rs',
    reason:
      "The service hashes its own executable image at start (L1 build check). That is not a " +
      'private file, and the open must follow the path the kernel ran.',
  },
];

const OWNER = 'crates/aac-files/src/';

/** Rust source with every `#[cfg(test)]` module body blanked (line count kept). */
export function withoutTests(source) {
  const out = source.split('');
  const lines = source.split('\n');
  let offset = 0;
  const starts = lines.map((line) => {
    const at = offset;
    offset += line.length + 1;
    return at;
  });
  for (let i = 0; i < lines.length; i++) {
    if (!/^\s*#\[cfg\(test\)\]\s*$/.test(lines[i])) continue;
    let j = i + 1;
    while (j < lines.length && /^\s*(#\[|$)/.test(lines[j])) j++;
    if (j >= lines.length || !/^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*\{/.test(lines[j])) continue;
    const open = source.indexOf('{', starts[j]);
    const close = matching(source, open);
    for (let k = starts[i]; k <= close && k < out.length; k++) if (out[k] !== '\n') out[k] = ' ';
  }
  return out.join('');
}

/** The index of the brace that closes `open`, skipping strings, chars and comments. */
function matching(source, open) {
  let depth = 0;
  for (let i = open; i < source.length; i++) {
    const c = source[i];
    const next = source[i + 1];
    if (c === '/' && next === '/') {
      i = source.indexOf('\n', i);
      if (i < 0) return source.length - 1;
      continue;
    }
    if (c === '/' && next === '*') {
      i = source.indexOf('*/', i + 2) + 1;
      if (i <= 0) return source.length - 1;
      continue;
    }
    // A raw string, `r"..."`, `r#"..."#` or the byte form `br"..."` (L2 review L5): no escapes inside.
    const raw = /^b?r(#*)"/.exec(source.slice(i, i + 9));
    if ((c === 'r' || c === 'b') && raw && !/\w/.test(source[i - 1] ?? '')) {
      const quote = i + raw[0].length - 1;
      const end = source.indexOf(`"${raw[1]}`, quote + 1);
      i = end < 0 ? source.length : end + raw[1].length;
      continue;
    }
    if (c === '"') {
      for (i++; i < source.length && source[i] !== '"'; i++) if (source[i] === '\\') i++;
      continue;
    }
    if (c === "'" && /^'(\\.|[^\\'])'/.test(source.slice(i, i + 4))) {
      i = source.indexOf("'", i + (source[i + 1] === '\\' ? 3 : 2));
      continue;
    }
    if (c === '{') depth++;
    if (c === '}' && --depth === 0) return i;
  }
  return source.length - 1;
}

/** Every gated use in `files` (`[[relative, source], ...]`), and the allowlist entries used. */
export function scan(files, allowlist = ALLOWLIST) {
  const violations = [];
  const used = new Set();
  for (const [relative, source] of files) {
    if (relative.startsWith(OWNER)) continue;
    const lines = withoutTests(source).split('\n');
    lines.forEach((line, index) => {
      const code = line.replace(/\/\/.*$/, '');
      for (const [name, pattern] of PATTERNS) {
        if (!pattern.test(code)) continue;
        const entry = allowlist.find((e) =>
          e.path.endsWith('/') ? relative.startsWith(e.path) : relative === e.path
        );
        if (entry) used.add(entry.path);
        else violations.push(`${relative}:${index + 1}: ${name}`);
      }
    });
  }
  const stale = allowlist.filter((e) => !used.has(e.path)).map((e) => e.path);
  const unexplained = allowlist.filter((e) => (e.reason ?? '').trim().length < 40).map((e) => e.path);
  return { violations, stale, unexplained };
}

function rustSources() {
  const files = [];
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const full = path.join(dir, name);
      if (statSync(full).isDirectory()) walk(full);
      else if (name.endsWith('.rs')) files.push(full);
    }
  };
  for (const group of ['crates', 'native'])
    for (const crate of readdirSync(path.join(ROOT, group))) {
      const src = path.join(ROOT, group, crate, 'src');
      try {
        if (statSync(src).isDirectory()) walk(src);
      } catch {
        /* A folder without src/ (native/shared). */
      }
    }
  return files.map((file) => [
    path.relative(ROOT, file).split(path.sep).join('/'),
    readFileSync(file, 'utf8'),
  ]);
}

function selfTest() {
  const planted = [
    ['crates/x/src/a.rs', 'fn f() { let _ = std::fs::File::open("p"); }\n'],
    ['crates/x/src/b.rs', 'fn g() { o.custom_flags(libc::O_NOFOLLOW); }\n'],
    ['crates/x/src/c.rs', 'fn h() { fs::rename(a, b).ok(); t.persist(&p).ok(); }\n'],
    [
      'crates/x/src/d.rs',
      'fn ok() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let s = "}"; let c = \'{\'; std::fs::write(p, b"x"); }\n}\nfn after() { fs::write(p, b"y"); }\n',
    ],
    ['crates/aac-files/src/e.rs', 'fn i() { std::fs::File::open("p"); }\n'],
    ['crates/x/src/f.rs', '// File::open( in a comment\nfn j() {}\n'],
    [
      'crates/x/src/g.rs',
      'fn k() { let _ = std::fs::read(p); let _ = fs::read_to_string(p); }\n' +
        'fn listing() { fs::read_dir(p); fs::read_link(p); }\n',
    ],
    // A byte raw string ending in a backslash must not swallow the module's closing brace.
    [
      'crates/x/src/h.rs',
      'fn ok() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let b = br"\\"; let s = "}"; }\n}\nfn after() { fs::read(p); }\n',
    ],
  ];
  const { violations } = scan(planted, []);
  const expected = [
    'crates/x/src/a.rs:1: File::open',
    'crates/x/src/b.rs:1: O_NOFOLLOW',
    'crates/x/src/b.rs:1: custom_flags',
    'crates/x/src/c.rs:1: fs::rename',
    'crates/x/src/c.rs:1: persist',
    'crates/x/src/d.rs:6: fs::write',
    'crates/x/src/g.rs:1: fs::read',
    'crates/x/src/g.rs:1: fs::read_to_string',
    'crates/x/src/h.rs:6: fs::read',
  ];
  if (JSON.stringify(violations.sort()) !== JSON.stringify(expected))
    throw new Error(`self-test: ${JSON.stringify(violations)}`);
  const stale = scan(planted, [{ path: 'crates/x/src/zz.rs', reason: 'x'.repeat(40) }]).stale;
  if (stale.length !== 1) throw new Error('self-test: a stale allowlist entry was not reported');
  console.log('one-file-policy self-test: the scan catches every planted copy');
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  selfTest();
  const { violations, stale, unexplained } = scan(rustSources());
  for (const v of violations) console.error(`one-file-policy: ${v} (use aac_files)`);
  for (const s of stale) console.error(`one-file-policy: stale allowlist entry ${s}`);
  for (const u of unexplained)
    console.error(`one-file-policy: allowlist entry without a reason ${u}`);
  if (violations.length || stale.length || unexplained.length) process.exit(1);
  console.log(
    `one-file-policy: no file mechanics outside aac-files (${ALLOWLIST.length} allowlisted)`
  );
}
