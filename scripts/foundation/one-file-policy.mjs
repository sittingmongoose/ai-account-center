#!/usr/bin/env node
// L2 gate (DESIGN-M3-COMPLETE section 2.2): one file policy. Outside crates/aac-files, no Rust
// source opens, links, renames or writes a file by itself: no O_NOFOLLOW, custom_flags,
// OpenOptions, File::open, File::create, fs::read, fs::read_to_string, fs::write, fs::rename or
// tempfile persist (`fs::read` follows links and blocks on a FIFO swapped in after a check).
// Readers use aac_files::read, writers aac_files::OwnedStore. Test modules, tests/ and build
// scripts are not scanned. The pre-cut allowlist and the exact counts of operations carried over
// by the cut have reasons and must stay current. The self-test proves the scan catches a planted
// copy, including an extra instance of an explicitly admitted operation.
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = fileURLToPath(new URL('../..', import.meta.url));
const CUT_RECEIPT = JSON.parse(
  readFileSync(new URL('./one-file-cut-receipt.json', import.meta.url), 'utf8')
);

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
    path: 'crates/aac-agent/src/signin_session/verb.rs',
    reason:
      'The sign-in session cancel reads /proc/<relay>/task/<relay>/children to name the CLI ' +
      'pid before killing the relay group (Linux only, best effort). A kernel process listing, ' +
      'not an AAC private file; the journal itself writes through the aac-files registry row.',
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
    path: 'native/aac-usage/src/scan_index.rs',
    reason:
      "The scanner's persistent discovery index (CCS_HOME/analytics-scan-index-v1): directory " +
      'names and traversal progress only, never log contents or credentials. aac-usage is a ' +
      'native crate with its own lock and no aac-files dependency; it writes 0600 with ' +
      'O_NOFOLLOW under its lock, fsyncs and renames, and a busy or damaged index falls back to ' +
      'the bounded walk. Follow-up l2-usage-readers.',
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
    path: 'crates/aac-agent/src/apps/updater/',
    reason:
      'Update all (W1-W6, ported from app_updates.py and its workers): it installs and moves ' +
      "other apps' files (CLI packages, desktop bundles, ZCode and T3 adapters, the T3 runtime) " +
      'and reads their manifests, /proc and session files, plus its own worker locks, pending ' +
      'markers and Windows task request files. None is an AAC private store; moving its own ' +
      'locks and markers onto aac-files registry rows is follow-up l2-updater-files.',
  },
  {
    path: 'crates/aac-claude-renew/src/',
    reason:
      'The Claude renewal service (signin-works) keeps its own settings, state, run lock and log ' +
      'under ~/.ccs/claude-renew and publishes one status file to the share folder: create_new ' +
      'lock, 0600 state written then renamed, append-only scrubbed log. Like aac-renewal it is a ' +
      'separate service crate; moving it onto aac-files registry rows is follow-up l2-renew-files.',
  },
  {
    path: 'crates/aac-snapshot/src/reader/',
    reason:
      'The offline usage reader (P3) reads the published usage snapshot files from the share ' +
      "folder and updates the aac-usage block inside other tools' instruction files in place, so " +
      'their hard links and line endings survive. Neither is an AAC private file.',
  },
  {
    path: 'crates/aac-http/src/identity.rs',
    reason:
      "The service hashes its own executable image at start (L1 build check). That is not a " +
      'private file, and the open must follow the path the kernel ran.',
  },
  {
    path: 'crates/aac-http/src/status_tools.rs',
    reason:
      'Phase 4 step 5 reads browser-check and Simlock state files (another tool data and kernel /proc) directly instead of spawning Node helpers; tool state is not an AAC private registry row, and the readers are size-bounded with honest missing/damaged errors.',
  },
  {
    path: 'crates/aac-log/src/lib.rs',
    reason:
      "The dashboard's structured log stream (logs/current.jsonl and logs/archive), not a " +
      'registry row: one O_APPEND write per JSON line under the lease shared with log-storage.ts. ' +
      'Rotation publishes archives without overwrite and pruning uses age and total size. Its ' +
      'config read goes through aac-files.',
  },
  {
    path: 'crates/aac-status/src/share/seed.rs',
    reason:
      'The R4 catalog seeder keeps the TypeScript share mechanics byte for byte: a new file ' +
      'is admitted by hard link (fails on a file another machine wrote meanwhile), an existing ' +
      'models/aliases file is appended in its own layout and re-read before the rename. The ' +
      'share catalog is not a private file; the readers-check writer uses the leased aac-files ' +
      'writer, not these mechanics.',
  },
  {
    path: 'crates/aac-status/src/answer_signing.rs',
    reason:
      'The R4 answer-key lifecycle keeps the TypeScript mechanics byte for byte: exclusive ' +
      '0600 creation, hard-link admission (a racing process uses the first key), fsync of file ' +
      'and folder, and the broken-key repair rename. The key is a credential the signer owns; ' +
      'the answers store writer lock is held by the caller. Follow-up: a leased aac-files ' +
      'writer for the key row once the registry lists it.',
  },
  {
    path: 'crates/aac-renewal/src/',
    reason:
      "G10 saved-login renewal ports the TS Codex renewal mechanics: bounded reads of the " +
      "Codex CLI's own auth files and Linux /proc (another tool's data and kernel data, not " +
      'AAC private registry files), and the atomic 0600 temp+rename write of auth.json and ' +
      'the advisory codex-renewal-status.json under the Codex activation lock (the TS ' +
      'codex-atomic-file.ts writer; neither path is a private-files registry row). The ' +
      'parity bin and fixtures write fake-credential sandboxes in temp roots. Follow-up ' +
      'l2-renewal-readers: move the bounded reads onto aac_files::read.',
  },
  {
    path: 'crates/aac-renewal/src/',
    reason:
      "G10 saved-login renewal ports the TS Codex renewal mechanics: bounded reads of the " +
      "Codex CLI's own auth files and Linux /proc (another tool's data and kernel data, not " +
      'AAC private registry files), and the atomic 0600 temp+rename write of auth.json and ' +
      'the advisory codex-renewal-status.json under the Codex activation lock (the TS ' +
      'codex-atomic-file.ts writer; neither path is a private-files registry row). The ' +
      'parity bin and fixtures write fake-credential sandboxes in temp roots. Follow-up ' +
      'l2-renewal-readers: move the bounded reads onto aac_files::read.',
  },
  {
    path: 'crates/aac-xtask/src/',
    reason:
      'Dev, build, and performance test harness tasks (never linked into the dashboard service or ' +
      'agent closure): reads /proc, writes synthetic fixtures in temp directories, and manages build outputs.',
  },
];

const OWNER = 'crates/aac-files/src/';
const TEST_ONLY = /^\s*#\[cfg\((?:test|all\((?:test\s*,.*|.*?,\s*test)\))\)\]\s*$/;

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
    if (!TEST_ONLY.test(lines[i])) continue;
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
export function scan(files, allowlist = ALLOWLIST, cutReceipt = {}) {
  const violations = [];
  const used = new Set();
  const outstanding = new Map(
    Object.entries(cutReceipt).map(([file, { uses }]) => [file, [...uses]])
  );
  for (const [relative, source] of files) {
    if (relative.startsWith(OWNER) || /^#!\[cfg\(test\)\]\r?\n/.test(source)) continue;
    const lines = withoutTests(source).split('\n');
    lines.forEach((line, index) => {
      const code = line.replace(/\/\/.*$/, '');
      for (const [name, pattern] of PATTERNS) {
        if (!pattern.test(code)) continue;
        const entry = allowlist.find((e) =>
          e.path.endsWith('/') ? relative.startsWith(e.path) : relative === e.path
        );
        if (entry) {
          used.add(entry.path);
          continue;
        }
        const remaining = outstanding.get(relative);
        const use = name + ':' + createHash('sha256').update(code.trim()).digest('hex');
        const admitted = remaining?.indexOf(use) ?? -1;
        if (admitted >= 0) remaining.splice(admitted, 1);
        else violations.push(`${relative}:${index + 1}: ${name}`);
      }
    });
  }
  const stale = allowlist.filter((e) => !used.has(e.path)).map((e) => e.path);
  for (const [file, remaining] of outstanding) {
    if (remaining.length) stale.push(`${file} (${remaining.length} cut receipts)`);
  }
  const unexplained = [...allowlist, ...Object.entries(cutReceipt).map(([path, e]) => ({ path, ...e }))]
    .filter((e) => (e.reason ?? '').trim().length < 40).map((e) => e.path);
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
      'fn ok() {}\n#[cfg(all(test, unix))]\nmod tests {\n    fn t() { fs::write(p, br"\\"); let s = "}"; }\n}\nfn after() { fs::read(p); }\n',
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
  const [file, operation] = planted[0];
  const receipt = {
    [file]: {
      reason: 'Only the original operation is admitted by this receipt.',
      uses: ['File::open:' + createHash('sha256').update(operation.trim()).digest('hex')],
    },
  };
  if (scan([[file, operation.repeat(2)]], [], receipt).violations.length !== 1)
    throw new Error('self-test: a new use matched a spent cut receipt');
  if (scan([[file, '']], [], receipt).stale.length !== 1)
    throw new Error('self-test: a stale cut receipt was not reported');
  console.log('one-file-policy self-test: the scan catches every planted copy');
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  selfTest();
  const { violations, stale, unexplained } = scan(rustSources(), ALLOWLIST, CUT_RECEIPT);
  for (const v of violations) console.error(`one-file-policy: ${v} (use aac_files)`);
  for (const s of stale) console.error(`one-file-policy: stale allowlist entry ${s}`);
  for (const u of unexplained)
    console.error(`one-file-policy: allowlist entry without a reason ${u}`);
  if (violations.length || stale.length || unexplained.length) process.exit(1);
  console.log(
    `one-file-policy: no new file mechanics outside aac-files (${ALLOWLIST.length} allowlisted, ${Object.keys(CUT_RECEIPT).length} cut files pinned)`
  );
}
