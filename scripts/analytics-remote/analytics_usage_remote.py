#!/usr/bin/env python3
"""Aggregate Claude Code, Codex, OMP, Muse, zcode and Antigravity usage into per-model, per-hour rows.

Reads one JSON request on stdin, scans the fixed default roots resolved on
this host, T3 Code's account homes (Claude config dirs and Codex homes found
under ~/.claude-t3, ~/.codex-t3 and T3's settings; each real file and each
record read once, see _t3_roots) plus the validated extra roots in the
request, and prints
per-model, per-hour aggregates plus per-file fingerprints. Only model names,
providers, hour buckets, numeric token and cost sums and one hashed session key
per session aggregate leave the host; paths, session ids, prompts, tool output
and every other conversation content stay here.

Request (all fields validated, unknown fields rejected):
  {"kinds": ["claude", "codex", "omp", "muse", "zcode", "antigravity"], "minDateMs": 123,
   "immutableSqlite": true,
   "extraRoots": {"claude": ["/abs/projects"], "codex": ["/abs/.codex"],
                  "omp": ["/abs/sessions"], "muse": [...], "zcode": [...]},
   "fingerprints": {"claude": {"<filekey>": {"size": 1, "mtimeMs": 2}},
                   "codex": {...}, "omp": {...}, "muse": {...}, "zcode": {...}}}

Response:
  {"version": 1, "truncated": false, "discoveryTruncated": false,
   "kinds": {"omp": {"state": "ok"|"not_installed"|"error"|"pending",
                     "partial": true?, "fingerprints": {...},
                     "walUnread": true?}} ,
   "rows": [{"k": "omp", "f": "<filekey>", "m": "<model>", "p": "<provider>",
             "h": "2026-10-01 15:00", "i": 1, "o": 2, "cr": 3, "cw": 4,
             "c": 0.01, "n": 5}],
   "srows": [{"k": "omp", "f": "<filekey>", "s": "<session key>", "m": "<model>",
              "p": "<provider>", "a": 123, "z": 456, "i": 1, "o": 2, "cr": 3,
              "cw": 4, "c": 0.01, "n": 5}]}
Only files whose fingerprint is new or changed contribute rows; the caller
merges rows by filekey and drops rows whose filekey disappeared.

"zcodeRecords": true (local experiment databases only, never sent to a remote
host) replaces zcode's hourly and session rows with one entry per in-window
usage row of each new or changed database, so the server can count a row once
however many copies of its database exist:
  "zrecs": {"<filekey>": [["<row key>", "<session key>", "<model>",
             "<provider>", <started_at ms>, i, o, cr, cw], ...]}
The row key is sha256 of the row's id and content, truncated to 16 hex
characters; ids, like session ids, never leave the host.

"s" is sha256("aac-session-v1:<kind>:<session id>") truncated to 16 hex
characters: the same key the server derives from its own local readers, so one
session groups across hosts and the id itself never leaves this one. A kind
whose records carry no session id contributes no session aggregate.

"antigravity" reads every conversation database T3 Code's usage reader reads
(_agy_dirs: ~/.gemini stores, ~/.config/antigravity, T3's Antigravity instance
folders) with mode=ro opens, never immutable, whatever immutableSqlite says. A
record counts once across databases and copies, so the kind has one store
fingerprint over all of them: any change re-reads them together, and its rows
and session rows carry the store key. "unreadable": true says a database or
folder could not be read; the store is read again on the next call.

"truncated" means a file, row or deadline cap stopped the scan, so some files
were not read; "discoveryTruncated" means only that the search for custom OMP
session roots hit its bounds, so roots it did not reach were not read. A row
is either wholly logged (c > 0) or wholly unlogged (c == 0): events with and
without a logged cost never share a row. "error" means the kind's data exists
but could not be read; nothing of that kind is confirmed. "pending" means the
scan ran out of time before it reached the kind: none of its files were
visited, and the next scan continues from the saved fingerprints. "partial"
marks a kind whose own scan a cap or the deadline cut short after some of its
files were read; its numbers are incomplete until a later scan finishes it.
"""

import collections
import datetime
import hashlib
import json
import os
import re
import sqlite3
import sys
import time
import urllib.parse

VERSION = 1
# Same ceilings the server collector uses where they apply.
MAX_FILES = 20000
MAX_LINE_BYTES = 8 * 1024 * 1024
MAX_ROWS = 50000
# Per-session-model aggregates share the response with the hourly rows; the
# session cap is separate so a host with many sessions keeps its hours too.
MAX_SROWS = 50000
SESSION_MAX_LEN = 160
# The incremental request carries one fingerprint per known file; the Mac alone
# holds thousands of session files, so the cap must fit tens of thousands.
MAX_REQUEST_BYTES = 8 * 1024 * 1024
# Saved extra roots per kind; zcode also takes the experiment databases the server found
# (one per content, experiment-usage-roots.ts), each read through its own fingerprint.
EXTRA_ROOTS_MAX = 16
EXTRA_ZCODE_DBS_MAX = 2048
# zcodeRecords: row entries one response carries (about 110 bytes each); past it the scan
# says truncated and the unconfirmed databases are read on the next call.
MAX_ZRECS = 60000
# Walk of one session root (the server collector's per-root ceilings).
WALK_MAX_DIRS = 10000
WALK_MAX_ENTRIES = 100000
# Breadth-first marker scan under ~/PM-Experiments for custom --session-dir
# roots, with the server scanner's bounds plus a share of the request budget.
SCAN_MAX_DEPTH = 6
SCAN_MAX_DIRS = 100000
SCAN_MAX_ENTRIES = 2000000
SCAN_BUDGET_SHARE = 0.4
SCAN_MAX_ROOTS = 512
SCAN_SKIP_DIRS = frozenset(
    [
        "node_modules",
        ".git",
        "tests",
        "test",
        "fixtures",
        "__fixtures__",
        "target",
        "dist",
        "coverage",
        "test-results",
    ]
)
# Sandbox marker: synthetic-log generators write this file at their
# data-tree root, and the session-root scan skips any subtree whose
# directory holds it, so measurement fixtures never count as real usage.
# Explicit roots (env vars, extra sources) are exempt: configuring a path
# explicitly means it should count.
AAC_SANDBOX_MARKER = ".aac-synthetic"
# T3 Code account homes (see _t3_roots): account folders read per base, and
# the largest T3 settings file read.
T3_MAX_HOMES = 64
T3_SETTINGS_MAX_BYTES = 1024 * 1024
CODEX_SESSION_UUID = re.compile(
    r"([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\.jsonl$", re.I
)
SESSION_TS = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}[:-]\d{2}")
MODEL_MAX_LEN = 160
PROVIDER_MAX_LEN = 64


def _fail(message):
    sys.stderr.write("analytics-usage-remote: %s\n" % message)
    raise SystemExit(1)


def _home():
    home = os.path.expanduser("~")
    if not home or not os.path.isdir(home):
        _fail("home directory is unavailable")
    return home


def _filekey(kind, path):
    digest = hashlib.sha256()
    digest.update(kind.encode("utf-8"))
    digest.update(b"\x00")
    digest.update(os.path.abspath(path).encode("utf-8"))
    return digest.hexdigest()


def _fingerprint(path):
    try:
        stat = os.stat(path)
    except OSError:
        return None
    head = tail = ""
    try:
        with open(path, "rb") as handle:
            head = hashlib.sha256(handle.read(256)).hexdigest()
            if stat.st_size > 256:
                handle.seek(max(0, stat.st_size - 256))
                tail = hashlib.sha256(handle.read(256)).hexdigest()
            else:
                tail = head
    except OSError:
        return None
    return {
        "size": stat.st_size,
        "mtimeMs": int(stat.st_mtime * 1000),
        "head": head,
        "tail": tail,
    }


def _same_fingerprint(left, right):
    return (
        isinstance(left, dict)
        and isinstance(right, dict)
        and left.get("size") == right.get("size")
        and left.get("mtimeMs") == right.get("mtimeMs")
        and left.get("head") == right.get("head")
        and left.get("tail") == right.get("tail")
    )


def _clean_model(value):
    if not isinstance(value, str):
        return None
    text = value.strip()
    if not text or len(text) > MODEL_MAX_LEN:
        return None
    if re.search(r"[\x00-\x1f\x7f]", text):
        return None
    return text


def _clean_provider(value):
    if not isinstance(value, str):
        return None
    text = value.strip()
    if not text or len(text) > PROVIDER_MAX_LEN:
        return None
    if re.search(r"[\x00-\x1f\x7f]", text):
        return None
    return text


# The server publishes sha256("aac-session-v1:<tool>:<session id>")[:16] as a
# session's key (src/web-server/usage/analytics-session-key.ts). Deriving the
# same value here groups one session across hosts without the id ever leaving
# this one, so Session stats is not an Ubuntu-only surface.
SESSION_KEY_PREFIX = "aac-session-v1:"
SESSION_KEY_LENGTH = 16


def _session_key(kind, session_id):
    """The published session key of one raw session id, or None without one."""
    if not isinstance(session_id, str) or not session_id:
        return None
    digest = hashlib.sha256()
    digest.update(("%s%s:" % (SESSION_KEY_PREFIX, kind)).encode("utf-8"))
    digest.update(session_id.encode("utf-8"))
    return digest.hexdigest()[:SESSION_KEY_LENGTH]


def _omp_session_id(path):
    """The session id the server's ompSessionIdForFile derives from one path.

    The file's own <ts>_<uuid> stem when it has one, else the enclosing
    directory name (a custom --session-dir layout, or `__advisor.jsonl`),
    truncated to SESSION_MAX_LEN characters exactly as the server truncates it.
    """
    base = os.path.basename(path)
    if base != "__advisor.jsonl" and _is_session_filename(base):
        stem = base[: -len(".jsonl")] if base.endswith(".jsonl") else base
        return stem[:SESSION_MAX_LEN]
    parent = os.path.basename(os.path.dirname(path))
    if parent and parent not in (".", os.sep):
        return parent[:SESSION_MAX_LEN]
    return None


def _muse_session_id(path):
    """The server's museSessionIdForFile: the enclosing directory name."""
    return os.path.basename(os.path.dirname(path))[:SESSION_MAX_LEN]


def _non_negative_number(value):
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        number = float(value)
    else:
        return None
    if number != number or number == float("inf") or number == float("-inf"):
        return None
    if number < 0:
        return None
    return number


def _hour_label(epoch_ms, now_ms):
    if epoch_ms < 0 or epoch_ms > now_ms + 3600000:
        return None
    hour = time.strftime("%Y-%m-%d %H:00", time.gmtime(epoch_ms / 1000))
    if not re.match(r"^\d{4}-\d{2}-\d{2} \d{2}:00$", hour):
        return None
    return hour


class Collector(object):
    def __init__(self, min_date_ms, deadline, fingerprints, budget=20.0):
        self.min_date_ms = min_date_ms
        self.deadline = deadline
        self.budget = budget
        self.now_ms = int(time.time() * 1000)
        self.prior = fingerprints if isinstance(fingerprints, dict) else {}
        self.fresh = {}
        self.pending = {}
        self.rows = {}
        self.srows = {}
        # A file, row or deadline cap stopped the scan: some files were not read.
        self.truncated = False
        # The row cap was hit: nothing more can be added.
        self.row_cap = False
        # The custom-root search hit its bounds: some roots were not found.
        self.discovery_truncated = False
        self.wal_unread = False
        self.files_seen = 0
        # zcodeRecords mode: per-row entries by database filekey instead of zcode rows.
        self.zcode_records = False
        self.zrecs = {}
        self.zrec_count = 0
        # T3 copies (_scan_kind_files): while `capture` is a set, Claude and
        # Codex parsers only collect record keys into it; while `skip` is a
        # set, records whose key it holds are not counted.
        self.capture = None
        self.skip = None
        # Antigravity databases (or folders) that could not be read this scan.
        self.agy_unreadable = 0

    def keep_record(self, key):
        """False when the record is captured as a key or is a known copy."""
        if self.capture is not None:
            if key is not None:
                self.capture.add(key)
            return False
        return self.skip is None or key is None or key not in self.skip

    def expired(self):
        return time.monotonic() >= self.deadline

    def note_file(self, kind, path, deps=None):
        """Return the filekey when the file needs parsing (new or changed).

        `deps` (a T3 file's copy references, _deps_print) is folded into the
        tail print, so the file is read again when one of them changes.
        """
        self.files_seen += 1
        if self.files_seen > MAX_FILES:
            self.truncated = True
            return None
        current = _fingerprint(path)
        if current is None:
            return None
        if deps:
            current["tail"] = hashlib.sha256(
                (current["tail"] + "\n" + deps).encode("utf-8")
            ).hexdigest()
        key = _filekey(kind, path)
        self.pending.setdefault(kind, {})[key] = current
        prior = self.prior.get(kind, {}).get(key)
        return None if _same_fingerprint(prior, current) else key

    def confirm_file(self, kind, key):
        """Mark a fully parsed file fresh; truncated files stay unconfirmed."""
        pending = self.pending.get(kind, {}).get(key)
        if pending is not None:
            self.fresh.setdefault(kind, {})[key] = pending

    def add(self, kind, filekey, model, provider, hour, tokens, cost):
        # Logged and unlogged events never share a row, so a row's logged
        # cost never seems to cover tokens that logged none.
        key = (kind, filekey, model, provider or "", hour, cost > 0)
        row = self.rows.get(key)
        if row is None:
            if len(self.rows) >= MAX_ROWS:
                self.truncated = True
                self.row_cap = True
                return
            row = {
                "k": kind,
                "f": filekey,
                "m": model,
                "h": hour,
                "i": 0,
                "o": 0,
                "cr": 0,
                "cw": 0,
                "c": 0.0,
                "n": 0,
            }
            if provider:
                row["p"] = provider
            self.rows[key] = row
        row["i"] += tokens[0]
        row["o"] += tokens[1]
        row["cr"] += tokens[2]
        row["cw"] += tokens[3]
        row["c"] += cost
        row["n"] += 1

    def sadd(self, kind, filekey, session, model, provider, epoch_ms, tokens, cost):
        # One aggregate per session, model, routing provider and logged-cost
        # split, with the session's first and last event. `session` is already
        # the published key (_session_key): the log's own id never leaves this
        # host, and no path can ride along inside a digest.
        if not session:
            return
        key = (kind, filekey, session, model, provider or "", cost > 0)
        srow = self.srows.get(key)
        if srow is None:
            if len(self.srows) >= MAX_SROWS:
                # The session cap is separate from the row cap: a host with more
                # sessions than fit still reports its hours; the scan is partial.
                self.truncated = True
                return
            srow = {
                "k": kind,
                "f": filekey,
                "s": session,
                "m": model,
                "a": epoch_ms,
                "z": epoch_ms,
                "i": 0,
                "o": 0,
                "cr": 0,
                "cw": 0,
                "c": 0.0,
                "n": 0,
            }
            if provider:
                srow["p"] = provider
            self.srows[key] = srow
        if epoch_ms < srow["a"]:
            srow["a"] = epoch_ms
        if epoch_ms > srow["z"]:
            srow["z"] = epoch_ms
        srow["i"] += tokens[0]
        srow["o"] += tokens[1]
        srow["cr"] += tokens[2]
        srow["cw"] += tokens[3]
        srow["c"] += cost
        srow["n"] += 1


def _iter_jsonl_files(root, accept, collector):
    """Yield files under root; caps flag truncation, unreadable is skipped."""
    pending = [(root, 0)]
    visited_dirs = 0
    visited_entries = 0
    while pending:
        directory, depth = pending.pop()
        if visited_dirs >= WALK_MAX_DIRS or collector.expired():
            collector.truncated = True
            return
        visited_dirs += 1
        try:
            entries = sorted(os.listdir(directory))
        except OSError:
            continue
        for name in entries:
            if visited_entries >= WALK_MAX_ENTRIES:
                collector.truncated = True
                return
            visited_entries += 1
            if name in SCAN_SKIP_DIRS:
                continue
            path = os.path.join(directory, name)
            try:
                if os.path.isdir(path) and not os.path.islink(path):
                    if depth < 64:
                        pending.append((path, depth + 1))
                elif os.path.isfile(path) and accept(name):
                    yield path
            except OSError:
                continue


def _omp_roots(home, env, collector, extra_roots=()):
    roots = []
    seen = set()

    def _add(candidate):
        absolute = os.path.abspath(candidate)
        if absolute not in seen:
            seen.add(absolute)
            roots.append(absolute)

    default_agent = os.path.join(home, ".omp", "agent")
    _add(os.path.join(default_agent, "sessions"))
    # Per-profile OMP instances (Windows keeps `~/.omp/profiles/<name>`) store
    # their own sessions under `<profile>/agent/sessions`; enumerate the bounded
    # profile dirs so their usage is discovered too, not just the default agent.
    profiles_root = os.path.join(home, ".omp", "profiles")
    try:
        for profile in sorted(os.listdir(profiles_root))[:SCAN_MAX_ROOTS]:
            candidate = os.path.join(profiles_root, profile, "agent", "sessions")
            if os.path.isdir(candidate):
                _add(candidate)
    except OSError:
        pass
    custom_agent = env.get("PI_CODING_AGENT_DIR")
    if custom_agent:
        expanded = os.path.expanduser(custom_agent)
        if os.path.isabs(expanded):
            _add(os.path.join(expanded, "sessions"))
    extra = env.get("OMP_SESSION_DIRS")
    if extra:
        for piece in extra.split(os.pathsep):
            piece = piece.strip()
            if piece:
                expanded = os.path.expanduser(piece)
                if os.path.isabs(expanded):
                    _add(expanded)
    for found in _scan_session_roots(os.path.join(home, "PM-Experiments"), collector):
        _add(found)
    if len(roots) > SCAN_MAX_ROOTS:
        collector.discovery_truncated = True
    trimmed = roots[:SCAN_MAX_ROOTS]
    # Saved extras are explicit configuration, not discovery: they join after
    # the discovery cap, never cut by it, and never scanned twice.
    for candidate in extra_roots:
        absolute = os.path.abspath(candidate)
        if absolute not in seen:
            seen.add(absolute)
            trimmed.append(absolute)
    return trimmed


def _is_session_filename(name):
    if name == "__advisor.jsonl":
        return True
    if name.endswith(".jsonl"):
        stem = name[: -len(".jsonl")]
        return bool(SESSION_TS.match(stem) or SESSION_TS.match(name))
    # Custom --session-dir layouts also write extensionless <ts>_<uuid> files.
    return bool(SESSION_TS.match(name)) and "." not in name and "_" in name


def _scan_session_roots(base, collector):
    """Breadth-first marker scan for custom OMP session roots.

    Bounded by depth, directories, entries and a share of the request budget.
    Hitting a bound sets only discovery_truncated: the roots found so far are
    read in full, and nothing else is cut.
    """
    found = []
    if not os.path.isdir(base):
        return found
    deadline = min(
        collector.deadline, time.monotonic() + collector.budget * SCAN_BUDGET_SHARE
    )
    pending = collections.deque([(base, 0)])
    visited = 0
    examined = 0
    while pending and len(found) < SCAN_MAX_ROOTS:
        if (
            visited >= SCAN_MAX_DIRS
            or examined > SCAN_MAX_ENTRIES
            or time.monotonic() >= deadline
        ):
            collector.discovery_truncated = True
            break
        directory, depth = pending.popleft()
        if depth > SCAN_MAX_DEPTH:
            continue
        visited += 1
        try:
            with os.scandir(directory) as iterator:
                entries = list(iterator)
        except OSError:
            continue
        examined += len(entries)
        # Synthetic sandbox trees are never roots and never descended: their
        # fixture records would otherwise count as real usage.
        if _is_sandbox_tree(directory, entries):
            continue
        if directory != base:
            if os.path.basename(directory) == "sessions":
                if _sessions_dir_has_marker(directory):
                    found.append(directory)
                continue
            # Custom `--session-dir` roots are named freely (`<branch>-sessions`,
            # `rev-<id>-sessions`, ...); a literal `sessions` name check missed
            # them. Accept any non-base dir that directly holds an OMP session
            # file, by the same name rule the reader uses. Accepted roots are not
            # descended; the reader walks their nested subagent files. Mirrors the
            # TypeScript scanSessionRoots fix.
            if any(
                entry.is_file(follow_symlinks=False) and _is_session_filename(entry.name)
                for entry in entries
            ):
                found.append(directory)
                continue
        if depth >= SCAN_MAX_DEPTH:
            # One level past the depth cap, still examine session-container
            # children: experiment runners nest per-job session dirs one level
            # deeper than the cap (`runs/<run>/jobs/<job>/sessions`), and the
            # usage inside is real. Only `*sessions*`-named children are
            # examined, inline and never descended, so the extra work stays
            # bounded by that small set. Mirrors the TypeScript fix.
            if depth == SCAN_MAX_DEPTH:
                for entry in entries:
                    if (
                        len(found) >= SCAN_MAX_ROOTS
                        or examined > SCAN_MAX_ENTRIES
                        or time.monotonic() >= deadline
                    ):
                        break
                    try:
                        is_dir = entry.is_dir(follow_symlinks=False)
                    except OSError:
                        continue
                    if not is_dir or "sessions" not in entry.name:
                        continue
                    try:
                        with os.scandir(entry.path) as iterator:
                            child_entries = list(iterator)
                    except OSError:
                        continue
                    examined += len(child_entries) + 1
                    if _is_sandbox_tree(entry.path, child_entries):
                        continue
                    if entry.name == "sessions":
                        if _sessions_dir_has_marker(entry.path):
                            found.append(entry.path)
                    elif any(
                        child.is_file(follow_symlinks=False)
                        and _is_session_filename(child.name)
                        for child in child_entries
                    ):
                        found.append(entry.path)
            continue
        for entry in entries:
            if entry.name in SCAN_SKIP_DIRS:
                continue
            try:
                if entry.is_dir(follow_symlinks=False):
                    pending.append((entry.path, depth + 1))
            except OSError:
                continue
    if pending and len(found) >= SCAN_MAX_ROOTS:
        collector.discovery_truncated = True
    return found


def _is_sandbox_tree(directory, entries):
    """True when the scanned directory is a synthetic sandbox tree: it holds
    the sandbox marker, a SANDBOX.md, or a generator MANIFEST.json with a
    `trees` key (the T5-recipe sandboxes predate the marker). The MANIFEST
    read is one bounded small file, only for dirs that hold one; anything
    unparseable or oversized fails open (not a sandbox) so real logs are
    never hidden."""
    names = set()
    for entry in entries:
        try:
            is_file = entry.is_file(follow_symlinks=False)
        except OSError:
            continue
        if is_file:
            names.add(entry.name)
    if AAC_SANDBOX_MARKER in names or "SANDBOX.md" in names:
        return True
    if "MANIFEST.json" not in names:
        return False
    try:
        with open(os.path.join(directory, "MANIFEST.json"), "rb") as handle:
            text = handle.read(64 * 1024 + 1)
        if len(text) > 64 * 1024:
            return False
        parsed = json.loads(text.decode("utf-8"))
        return (
            isinstance(parsed, dict)
            and isinstance(parsed.get("trees"), dict)
        )
    except (OSError, ValueError):
        return False


def _sessions_dir_has_marker(directory):
    """True when a sessions/ dir holds an OMP-named file within depth 2.

    Presence alone (any .jsonl) accepted synthetic Muse trees
    (<uuid>/session.jsonl); the OMP line parser still rejects non-OMP
    records inside an accepted root.
    """
    pending = [(directory, 0)]
    checked = 0
    while pending:
        current, depth = pending.pop()
        try:
            entries = os.listdir(current)
        except OSError:
            return False
        for name in entries[:200]:
            checked += 1
            if checked > 400:
                return False
            path = os.path.join(current, name)
            try:
                if os.path.isfile(path):
                    if _is_session_filename(name):
                        return True
                elif depth < 1 and os.path.isdir(path) and not os.path.islink(path):
                    pending.append((path, depth + 1))
            except OSError:
                continue
    return False


def _parse_omp_line(line, collector, kind, filekey, session=None):
    # Pre-filter before parsing so conversation content is never decoded.
    if '"type"' not in line or '"message"' not in line or '"usage"' not in line:
        return
    try:
        record = json.loads(line)
    except ValueError:
        return
    if not isinstance(record, dict) or record.get("type") != "message":
        return
    message = record.get("message")
    if not isinstance(message, dict) or message.get("role") != "assistant":
        return
    model = _clean_model(message.get("model"))
    if model is None:
        return
    usage = message.get("usage")
    if not isinstance(usage, dict):
        return
    values = [
        _non_negative_number(usage.get("input")),
        _non_negative_number(usage.get("output")),
        _non_negative_number(usage.get("cacheRead")),
        _non_negative_number(usage.get("cacheWrite")),
    ]
    if any(value is None for value in values):
        return
    tokens = [int(values[0]), int(values[1]), int(values[2]), int(values[3])]
    cost = 0.0
    cost_block = usage.get("cost")
    if isinstance(cost_block, dict):
        total = _non_negative_number(cost_block.get("total"))
        if total is not None:
            cost = total
    # Zero-usage pings (e.g. union-alpha) carry no usage; never sum
    # cumulative goal counters or context snapshots (we only read usage.*).
    if tokens[0] + tokens[1] + tokens[2] + tokens[3] == 0 and cost == 0:
        return
    timestamp = record.get("timestamp")
    epoch_ms = None
    if isinstance(timestamp, str):
        try:
            text = timestamp.strip()
            if text.endswith("Z"):
                text = text[:-1] + "+00:00"
            moment = datetime.datetime.fromisoformat(text)
            if moment.tzinfo is None:
                moment = moment.replace(tzinfo=datetime.timezone.utc)
            epoch_ms = int(moment.timestamp() * 1000)
        except ValueError:
            return
    elif isinstance(timestamp, (int, float)) and not isinstance(timestamp, bool):
        epoch_ms = int(timestamp)
        while epoch_ms > 4102444800:
            epoch_ms //= 1000
        epoch_ms *= 1000
    if epoch_ms is None or epoch_ms < collector.min_date_ms:
        return
    hour = _hour_label(epoch_ms, collector.now_ms)
    if hour is None:
        return
    provider = _clean_provider(message.get("provider"))
    collector.add(kind, filekey, model, provider, hour, tokens, cost)
    collector.sadd(kind, filekey, session, model, provider, epoch_ms, tokens, cost)


def _session_copy_key(path):
    """The session-named part of an OMP file's path, or None.

    The nearest <ts>_<uuid> component (the file or its session directory) and
    everything below it. A resumed run copies a session into a new root under
    the same name, so files with the same key may hold the same records.
    """
    parts = os.path.abspath(path).split(os.sep)
    for index in range(len(parts) - 1, -1, -1):
        part = parts[index]
        if part != "__advisor.jsonl" and _is_session_filename(part):
            return "/".join(parts[index:])
    return None


def _is_prefix_copy(small, large):
    """True when small is a byte prefix of large: same head, same bytes where small ends."""
    small_path, small_size = small
    large_path, large_size = large
    if small_size > large_size:
        return False
    head = min(256, small_size)
    tail = max(0, small_size - 256)
    try:
        with open(small_path, "rb") as left, open(large_path, "rb") as right:
            if left.read(head) != right.read(head):
                return False
            left.seek(tail)
            right.seek(tail)
            return left.read(small_size - tail) == right.read(small_size - tail)
    except OSError:
        return False


def _drop_resume_copies(paths):
    """Keep the longest file per session; drop files that are a byte prefix of a kept one.

    A resumed OMP run copies the session file into its new root and appends to
    the copy, so the older copy's records are all in the newer one. Files that
    diverge are all kept.
    """
    groups = {}
    for path in paths:
        key = _session_copy_key(path)
        if key is None:
            continue
        try:
            size = os.stat(path).st_size
        except OSError:
            continue
        groups.setdefault(key, []).append((path, size))
    dropped = set()
    for group in groups.values():
        if len(group) < 2:
            continue
        group.sort(key=lambda item: (-item[1], item[0]))
        kept = []
        for item in group:
            if any(_is_prefix_copy(item, larger) for larger in kept):
                dropped.add(item[0])
            else:
                kept.append(item)
    return [path for path in paths if path not in dropped]


def _collect_omp(collector, home, env, extra=()):
    roots = [
        root
        for root in _omp_roots(home, env, collector, extra)
        if os.path.isdir(root) and not os.path.islink(root)
    ]
    if not roots:
        return "not_installed"

    def _accept(name):
        return name.endswith(".jsonl") or _is_session_filename(name)

    paths = []
    for root in roots:
        for path in _iter_jsonl_files(root, _accept, collector):
            paths.append(path)
            if len(paths) > MAX_FILES:
                collector.truncated = True
                break
        if collector.expired() or len(paths) > MAX_FILES:
            break
    for path in _drop_resume_copies(_newest_first(paths)):
        if collector.expired():
            collector.truncated = True
            return "ok"
        filekey = collector.note_file("omp", path)
        if filekey is None:
            staged = _filekey("omp", path)
            if staged in collector.pending.get("omp", {}):
                collector.confirm_file("omp", staged)
            continue
        session = _session_key("omp", _omp_session_id(path))
        try:
            handle = open(path, "rb")
        except OSError:
            continue
        with handle:
            while True:
                # Only the deadline and the row cap stop a file part-way.
                if collector.expired():
                    collector.truncated = True
                    return "ok"
                chunk = handle.readline(MAX_LINE_BYTES + 2)
                if not chunk:
                    break
                if len(chunk) > MAX_LINE_BYTES + 1:
                    # Skip the oversized line without decoding content.
                    while chunk and not chunk.endswith(b"\n"):
                        chunk = handle.readline(MAX_LINE_BYTES + 2)
                    continue
                try:
                    line = chunk.decode("utf-8")
                except UnicodeDecodeError:
                    continue
                _parse_omp_line(line, collector, "omp", filekey, session)
                if collector.row_cap:
                    return "ok"
        collector.confirm_file("omp", filekey)
    return "ok"


def _normalize_recorded_at(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    epoch = int(value)
    if epoch <= 0:
        return None
    seconds = epoch
    while seconds > 4102444800:
        seconds //= 1000
    if seconds <= 0:
        return None
    return seconds * 1000


def _muse_event_from_record(record):
    if not isinstance(record, dict):
        return None
    payload = record.get("payload")
    event = payload.get("event") if isinstance(payload, dict) else None
    if isinstance(event, dict) and event.get("kind") == "model_completed":
        return record.get("recorded_at"), event
    return None


def _parse_muse_line(line, collector, kind, filekey, session=None):
    # model_completed only; goal_usage_attribution carries the same quantities
    # and would double every token. Pre-filter before parsing.
    if "model_completed" not in line:
        return
    try:
        record = json.loads(line)
    except ValueError:
        return
    if not isinstance(record, dict):
        return
    found = _muse_event_from_record(record)
    if found is None:
        children = record.get("children")
        if isinstance(children, list):
            for child in children:
                if not isinstance(child, dict):
                    continue
                embedded = child.get("record_json")
                if isinstance(embedded, str):
                    try:
                        embedded = json.loads(embedded)
                    except ValueError:
                        continue
                # Unpack one level only; deeper nesting is not a usage record.
                inner = _muse_event_from_record(embedded)
                if inner is not None:
                    found = (record.get("recorded_at"), inner[1])
                    break
    if found is None:
        return
    recorded_at, event = found
    epoch_ms = _normalize_recorded_at(recorded_at)
    if epoch_ms is None or epoch_ms < collector.min_date_ms:
        return
    model = _clean_model(event.get("model"))
    if model is None:
        return
    usage = event.get("usage")
    if not isinstance(usage, dict):
        return
    values = [
        _non_negative_number(usage.get("input_tokens")),
        _non_negative_number(usage.get("output_tokens")),
        _non_negative_number(usage.get("cache_read_tokens")),
        _non_negative_number(usage.get("cache_write_tokens")),
    ]
    if any(value is None for value in values):
        return
    # cached_tokens duplicates cache_read_tokens; output already includes
    # reasoning per the Codex convention, so reasoning_tokens is not added.
    # input_tokens includes the cache reads (as zcode's does): net them out.
    tokens = [
        max(0, int(values[0]) - int(values[2])),
        int(values[1]),
        int(values[2]),
        int(values[3]),
    ]
    hour = _hour_label(epoch_ms, collector.now_ms)
    if hour is None:
        return
    collector.add(kind, filekey, model, None, hour, tokens, 0.0)
    collector.sadd(kind, filekey, session, model, None, epoch_ms, tokens, 0.0)


def _scan_muse_dir(collector, directory):
    """Scan one Muse sessions directory; True when the scan stopped early."""
    for path in _iter_jsonl_files(
        directory, lambda name: name == "session.jsonl", collector
    ):
        if collector.expired():
            collector.truncated = True
            return True
        filekey = collector.note_file("muse", path)
        if filekey is None:
            staged = _filekey("muse", path)
            if staged in collector.pending.get("muse", {}):
                collector.confirm_file("muse", staged)
            continue
        session = _session_key("muse", _muse_session_id(path))
        try:
            handle = open(path, "rb")
        except OSError:
            continue
        with handle:
            while True:
                if collector.expired():
                    collector.truncated = True
                    return True
                chunk = handle.readline(MAX_LINE_BYTES + 2)
                if not chunk:
                    break
                if len(chunk) > MAX_LINE_BYTES + 1:
                    while chunk and not chunk.endswith(b"\n"):
                        chunk = handle.readline(MAX_LINE_BYTES + 2)
                    continue
                try:
                    line = chunk.decode("utf-8")
                except UnicodeDecodeError:
                    continue
                _parse_muse_line(line, collector, "muse", filekey, session)
                if collector.row_cap:
                    return True
        collector.confirm_file("muse", filekey)
    return False


def _collect_muse(collector, home, env, extra=()):
    override = env.get("MUSE_SESSIONS_DIR")
    sessions = (
        os.path.expanduser(override)
        if override and os.path.isabs(os.path.expanduser(override))
        else os.path.join(home, ".local", "share", "muse", "sessions")
    )
    seen = set()
    dirs = []
    for candidate in (sessions, *extra):
        absolute = os.path.abspath(candidate)
        if absolute not in seen:
            seen.add(absolute)
            dirs.append(absolute)
    scanned = False
    for directory in dirs:
        if not os.path.isdir(directory) or os.path.islink(directory):
            continue
        scanned = True
        if _scan_muse_dir(collector, directory):
            return "ok"
    return "ok" if scanned else "not_installed"


def _epoch_ms_from_timestamp(value):
    """An ISO-8601 log timestamp (Claude Code, Codex) to epoch ms; None when unusable."""
    if not isinstance(value, str):
        return None
    text = value.replace("\ufeff", "").strip()
    if not text:
        return None
    if text.endswith(("Z", "z")):
        text = text[:-1] + "+00:00"
    try:
        moment = datetime.datetime.fromisoformat(text)
    except ValueError:
        return None
    if moment.tzinfo is None:
        moment = moment.replace(tzinfo=datetime.timezone.utc)
    return int(moment.timestamp() * 1000)


def _newest_first(paths):
    """Order paths newest first (ties by path).

    A cut scan always starves the same tail under walk order; newest-first
    banks the most valuable progress first, and any changed file jumps to the
    front by its fresh mtime, so the next scan continues where this one stopped.
    """
    stamped = []
    for path in paths:
        try:
            stamped.append((os.stat(path).st_mtime_ns, path))
        except OSError:
            continue
    stamped.sort(key=lambda item: (-item[0], item[1]))
    return [path for _, path in stamped]


def _read_file(collector, kind, path, filekey, parse, flush):
    """Parse one file whole: "ok", "cut" (deadline or row cap) or "unreadable"."""
    try:
        handle = open(path, "rb")
    except OSError:
        return "unreadable"
    box = {}
    with handle:
        while True:
            if collector.expired():
                collector.truncated = True
                return "cut"
            chunk = handle.readline(MAX_LINE_BYTES + 2)
            if not chunk:
                break
            if len(chunk) > MAX_LINE_BYTES + 1:
                # Skip the oversized line without decoding content.
                while chunk and not chunk.endswith(b"\n"):
                    chunk = handle.readline(MAX_LINE_BYTES + 2)
                continue
            try:
                line = chunk.decode("utf-8")
            except UnicodeDecodeError:
                continue
            parse(line, collector, kind, filekey, box)
            if collector.row_cap:
                return "cut"
        if flush is not None:
            flush(collector, kind, filekey, box)
    return "ok"


def _scan_kind_files(collector, kind, dirs, accept, parse, flush=None, t3_dirs=()):
    """Scan dirs for kind's files, newest first; parse(line, collector, kind, filekey, box) reads one line with a per-file box.

    flush(collector, kind, filekey, box), when given, runs once per fully
    read file (never on a cut pass: cut files are re-read whole next pass).
    t3_dirs (T3 Code homes, _t3_roots) are read too, each real file once and
    never a record a default file (or an earlier T3 file) already holds.
    """
    paths = []
    for directory in dirs:
        if not os.path.isdir(directory) or os.path.islink(directory):
            continue
        for path in _iter_jsonl_files(directory, accept, collector):
            paths.append(path)
            if len(paths) > MAX_FILES:
                collector.truncated = True
                break
        if collector.expired() or len(paths) > MAX_FILES:
            break
    refs = _t3_files(collector, kind, dirs, paths, t3_dirs, accept) if t3_dirs else {}
    paths.extend(refs)
    for path in _newest_first(paths):
        if collector.expired():
            collector.truncated = True
            return True
        copies = refs.get(path)
        filekey = collector.note_file(kind, path, _deps_print(kind, copies) if copies else None)
        if filekey is None:
            staged = _filekey(kind, path)
            if staged in collector.pending.get(kind, {}):
                collector.confirm_file(kind, staged)
            continue
        if copies:
            known = _record_keys(collector, kind, copies, parse, flush)
            if known is None:
                return True
            collector.skip = known
        try:
            outcome = _read_file(collector, kind, path, filekey, parse, flush)
        finally:
            collector.skip = None
        if outcome == "cut":
            return True
        if outcome == "ok":
            collector.confirm_file(kind, filekey)
    return False


def _t3_child_dirs(base):
    """Sub-folders of base (a link to a folder counts), by name, at most T3_MAX_HOMES."""
    try:
        names = sorted(os.listdir(base))
    except OSError:
        return []
    found = []
    for name in names:
        if len(found) >= T3_MAX_HOMES:
            break
        if os.path.isdir(os.path.join(base, name)):
            found.append(os.path.join(base, name))
    return found


def _t3_home_path(value, home):
    if not isinstance(value, str):
        return None
    text = value.strip()
    if not text or len(text) > 1024 or "\x00" in text:
        return None
    if text == "~":
        text = home
    elif text[:2] in ("~/", "~\\"):
        text = os.path.join(home, text[2:])
    return os.path.normpath(text) if os.path.isabs(text) else None


def _t3_roots(home, kind):
    """T3 Code's account homes on this host: `projects` of each Claude config
    dir (~/.claude-t3/<account>), `sessions` of each Codex home and shadow home
    (~/.codex-t3/<account>, T3's managed ones under its state folder), plus the
    homes T3's settings name (providerInstances.<id>.config.homePath, and
    shadowHomePath for Codex; no other field is read). Any folder name counts."""
    state = os.path.join(home, ".t3", "userdata")
    homes = []
    if kind == "claude":
        homes.extend(_t3_child_dirs(os.path.join(home, ".claude-t3")))
    else:
        homes.extend(_t3_child_dirs(os.path.join(home, ".codex-t3")))
        for managed in _t3_child_dirs(os.path.join(state, "providers", "codex")):
            homes.append(os.path.join(managed, "shadow"))
    try:
        settings = os.path.join(state, "settings.json")
        if os.path.getsize(settings) <= T3_SETTINGS_MAX_BYTES:
            with open(settings, "rb") as handle:
                instances = json.loads(handle.read().decode("utf-8")).get("providerInstances")
        else:
            instances = None
    except Exception:
        # Any unreadable settings file (deep nesting raises RecursionError)
        # only means no settings homes; it must never fail the scan.
        instances = None
    driver_keys = {"claude": ("claudeAgent", ("homePath",)),
                   "codex": ("codex", ("homePath", "shadowHomePath"))}[kind]
    if isinstance(instances, dict):
        for instance in list(instances.values())[:T3_MAX_HOMES]:
            if not isinstance(instance, dict) or instance.get("driver") != driver_keys[0]:
                continue
            config = instance.get("config")
            for key in driver_keys[1] if isinstance(config, dict) else ():
                path = _t3_home_path(config.get(key), home)
                if path:
                    homes.append(path)
    sub = "projects" if kind == "claude" else "sessions"
    return _dedup_dirs([os.path.join(path, sub) for path in homes])


def _same_or_inside(child, roots):
    child = os.path.normcase(child)
    for root in roots:
        root = os.path.normcase(root)
        if child == root or child.startswith(root.rstrip("\\/") + os.sep):
            return True
    return False


def _t3_link_inside(path, start):
    """The real path of the link (or junction) at path, or None when its target
    leaves start. The target is checked as written first, without a stat, so a
    link to a network mount is never touched (a stat on a dead mount blocks)."""
    try:
        written = os.readlink(path)
    except (OSError, ValueError, AttributeError, NotImplementedError):
        return None
    if written.startswith("\\\\?\\UNC\\"):
        written = "\\\\" + written[8:]
    elif written.startswith("\\\\?\\"):
        written = written[4:]
    target = os.path.normpath(os.path.join(os.path.dirname(path), written))
    if not _same_or_inside(target, [start]):
        return None
    real = os.path.realpath(path)
    return real if _same_or_inside(real, [start]) else None


def _is_link(path):
    isjunction = getattr(os.path, "isjunction", None)
    return os.path.islink(path) or bool(isjunction and isjunction(path))


def _iter_t3_files(root, accept, collector, exclude):
    """Yield (path, identity) of wanted files under one T3 root. Unlike every
    other walk here it follows links (a T3 home may be made of them), but only
    to a target inside the root's own real path: a link out of the root (into
    ~/.codex, ~/PM-Experiments, a network mount) is skipped untouched. A root
    or folder inside `exclude` (the real default roots) is not read, and each
    folder is entered once by identity, so a link loop ends."""
    try:
        start = os.path.realpath(root)
    except (OSError, ValueError):
        return
    if not os.path.isdir(start) or _same_or_inside(start, exclude):
        return
    pending = [(start, 0)]
    seen = set()
    entries = 0
    while pending:
        if len(seen) >= WALK_MAX_DIRS or collector.expired():
            collector.truncated = True
            return
        directory, depth = pending.pop()
        try:
            stat = os.stat(directory)
            if (stat.st_dev, stat.st_ino) in seen:
                continue
            seen.add((stat.st_dev, stat.st_ino))
            names = sorted(os.listdir(directory))
        except OSError:
            continue
        for name in names:
            entries += 1
            if entries > WALK_MAX_ENTRIES:
                collector.truncated = True
                return
            if name in SCAN_SKIP_DIRS:
                continue
            path = os.path.join(directory, name)
            try:
                real = _t3_link_inside(path, start) if _is_link(path) else path
                if real is None or _same_or_inside(real, exclude):
                    continue
                if os.path.isdir(real):
                    if depth < 64:
                        pending.append((real, depth + 1))
                elif os.path.isfile(real) and accept(name):
                    stat = os.stat(real)
                    yield real, (stat.st_dev, stat.st_ino)
            except (OSError, ValueError):
                continue


def _copy_key(kind, path):
    """Where copies of a log's records can be: a log of the same name (Claude
    keeps <session>.jsonl and agent-<id>.jsonl), or of the same conversation
    id (a Codex rollout's name ends with it)."""
    name = os.path.basename(path)
    match = CODEX_SESSION_UUID.search(name) if kind == "codex" else None
    return match.group(1).lower() if match else name


def _t3_files(collector, kind, dirs, default_paths, t3_dirs, accept):
    """T3 files to read, each mapped to the default and earlier T3 files that
    may hold copies of its records. A file that is a default file (a link or a
    hard link to it) or an earlier T3 file is not listed at all."""
    exclude = []
    for directory in dirs:
        try:
            if os.path.isdir(directory):
                exclude.append(os.path.realpath(directory))
        except (OSError, ValueError):
            continue
    identities = set()
    for path in default_paths:
        try:
            stat = os.stat(path)
            identities.add((stat.st_dev, stat.st_ino))
        except OSError:
            continue
    found = []
    for root in t3_dirs:
        for path, identity in _iter_t3_files(root, accept, collector, exclude):
            if identity in identities:
                continue
            identities.add(identity)
            found.append(path)
            if len(default_paths) + len(found) > MAX_FILES:
                collector.truncated = True
                break
    by_key = {}
    for path in default_paths:
        by_key.setdefault(_copy_key(kind, path), []).append(path)
    copies = {}
    for path in sorted(found):
        key = _copy_key(kind, path)
        copies[path] = list(by_key.get(key, ()))
        by_key.setdefault(key, []).append(path)
    return copies


def _deps_print(kind, copies):
    prints = []
    for path in sorted(copies):
        print_ = _fingerprint(path) or {}
        prints.append("%s:%s:%s:%s:%s" % (_filekey(kind, path), print_.get("size"),
                                         print_.get("mtimeMs"), print_.get("head"),
                                         print_.get("tail")))
    return hashlib.sha256("\n".join(prints).encode("utf-8")).hexdigest()


def _record_keys(collector, kind, copies, parse, flush):
    """The record keys the copy references hold, or None when the deadline cut the read."""
    keys = set()
    collector.capture = keys
    try:
        for path in copies:
            if _read_file(collector, kind, path, "", parse, flush) == "cut":
                return None
    finally:
        collector.capture = None
    return keys


def _dedup_dirs(candidates):
    seen = set()
    dirs = []
    for candidate in candidates:
        absolute = os.path.abspath(candidate)
        if absolute not in seen:
            seen.add(absolute)
            dirs.append(absolute)
    return dirs


def _parse_claude_line(line, collector, kind, filekey, box):
    # The local jsonl-parser's parseUsageEntry: assistant messages with usage.
    # Pre-filter before parsing so conversation content is never decoded.
    if '"type"' not in line or '"assistant"' not in line or '"usage"' not in line:
        return
    try:
        record = json.loads(line)
    except ValueError:
        return
    if not isinstance(record, dict) or record.get("type") != "assistant":
        return
    message = record.get("message")
    if not isinstance(message, dict):
        return
    model = _clean_model(message.get("model"))
    if model is None:
        return
    usage = message.get("usage")
    if not isinstance(usage, dict):
        return
    values = [
        _non_negative_number(usage.get("input_tokens")),
        _non_negative_number(usage.get("output_tokens")),
    ]
    if any(value is None for value in values):
        return
    cache_read = _non_negative_number(usage.get("cache_read_input_tokens"))
    cache_write = _non_negative_number(usage.get("cache_creation_input_tokens"))
    tokens = [int(values[0]), int(values[1]), int(cache_read or 0), int(cache_write or 0)]
    epoch_ms = _epoch_ms_from_timestamp(record.get("timestamp"))
    if epoch_ms is None or epoch_ms < collector.min_date_ms:
        return
    hour = _hour_label(epoch_ms, collector.now_ms)
    if hour is None:
        return
    # One API response, not one content block: Claude Code writes several
    # assistant lines per response (same message id and request id, usage
    # repeated, last line complete). Consecutive same-response lines collapse
    # to the last; files are read whole or not at all per pass, so the
    # group-in-progress lives in the per-file box and flushes at EOF.
    message_id = message.get("id")
    request_id = record.get("requestId", message.get("requestId"))
    session = _session_key(kind, record.get("sessionId"))
    if (
        isinstance(message_id, str)
        and message_id
        and len(message_id) <= 200
        and (
            request_id is None
            or (isinstance(request_id, str) and len(request_id) <= 200)
        )
    ):
        key = message_id + "\n" + (request_id or "")
        pending = box.get("claude_pending")
        if pending is not None and pending[0] == key:
            box["claude_pending"] = (key, model, hour, tokens, epoch_ms, session)
        else:
            if pending is not None:
                _emit_claude_response(collector, kind, filekey, pending)
            box["claude_pending"] = (key, model, hour, tokens, epoch_ms, session)
        return
    pending = box.pop("claude_pending", None)
    if pending is not None:
        _emit_claude_response(collector, kind, filekey, pending)
    if not collector.keep_record(None):
        return
    # Claude Code logs no cost; the server prices the tokens at its rates.
    collector.add(kind, filekey, model, None, hour, tokens, 0.0)
    collector.sadd(kind, filekey, session, model, None, epoch_ms, tokens, 0.0)


def _emit_claude_response(collector, kind, filekey, pending):
    key, model, hour, tokens, epoch_ms, session = pending
    # A copy is the same API response: its message id, as the server's
    # experiment dedup keys it (a stream capture spells the request id apart).
    if not collector.keep_record("m:" + key.split("\n")[0]):
        return
    collector.add(kind, filekey, model, None, hour, tokens, 0.0)
    collector.sadd(kind, filekey, session, model, None, epoch_ms, tokens, 0.0)


def _flush_claude_box(collector, kind, filekey, box):
    pending = box.pop("claude_pending", None)
    if pending is not None:
        _emit_claude_response(collector, kind, filekey, pending)


def _collect_claude(collector, home, env, extra=()):
    override = env.get("CLAUDE_CONFIG_DIR")
    base = (
        os.path.expanduser(override)
        if override and os.path.isabs(os.path.expanduser(override))
        else os.path.join(home, ".claude")
    )
    dirs = _dedup_dirs([os.path.join(base, "projects"), *extra])
    t3_dirs = [d for d in _t3_roots(home, "claude") if os.path.isdir(d)]
    if not t3_dirs and not any(os.path.isdir(d) and not os.path.islink(d) for d in dirs):
        return "not_installed"
    _scan_kind_files(
        collector,
        "claude",
        dirs,
        lambda name: name.endswith(".jsonl"),
        _parse_claude_line,
        _flush_claude_box,
        t3_dirs,
    )
    return "ok"


def _codex_number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return 0
    if value != value or value in (float("inf"), float("-inf")) or value <= 0:
        return 0
    return value


def _codex_snapshot(value):
    # Codex's TokenUsage copies the Responses usage fields; cached reads and
    # cache writes are details of input_tokens (see codex-native-usage-collector).
    if not isinstance(value, dict):
        return None
    cache_read = _codex_number(value.get("cached_input_tokens"))
    cache_write = _codex_number(value.get("cache_write_input_tokens"))
    return (
        max(0, _codex_number(value.get("input_tokens")) - cache_read - cache_write),
        cache_write,
        cache_read,
        _codex_number(value.get("output_tokens")),
    )


def _parse_codex_line(line, collector, kind, filekey, box):
    # The local codex-native-usage-collector's stateful rollout parser: session
    # metadata and turn context set the box up, token_count events carry
    # cumulative counters that are differenced here. Cliproxy-backed sessions
    # are skipped, as locally. Pre-filter before parsing so prompts and tool
    # output are never decoded.
    if '"type"' not in line:
        return
    if '"session_meta"' not in line and '"turn_context"' not in line and '"event_msg"' not in line:
        return
    if '"token_count"' not in line and '"session_meta"' not in line and '"turn_context"' not in line:
        return
    try:
        record = json.loads(line)
    except ValueError:
        return
    if not isinstance(record, dict):
        return
    payload = record.get("payload")
    if record.get("type") == "session_meta" and isinstance(payload, dict):
        session = payload.get("id")
        if isinstance(session, str) and session.strip():
            box["session"] = session
        provider = payload.get("model_provider")
        if isinstance(provider, str) and provider.strip():
            box["provider"] = provider
        return
    if record.get("type") == "turn_context" and isinstance(payload, dict):
        model = payload.get("model")
        if isinstance(model, str) and model.strip():
            box["model"] = model
        return
    if (
        record.get("type") != "event_msg"
        or not isinstance(payload, dict)
        or payload.get("type") != "token_count"
        or not box.get("session")
        or box.get("provider") in ("cliproxy", "ccs_runtime")
    ):
        return
    info = payload.get("info")
    if not isinstance(info, dict):
        return
    total = _codex_snapshot(info.get("total_token_usage"))
    last = _codex_snapshot(info.get("last_token_usage"))
    if total is None:
        return
    previous = box.get("previous")
    if previous is not None and total == previous:
        return
    if previous is None:
        delta = last if last is not None and any(v > 0 for v in last) else total
    else:
        delta = tuple(max(0, now - was) for now, was in zip(total, previous))
    box["previous"] = total
    if not any(v > 0 for v in delta):
        return
    timestamp = record.get("timestamp")
    if not isinstance(timestamp, str) or not timestamp.strip():
        return
    epoch_ms = _epoch_ms_from_timestamp(timestamp)
    if epoch_ms is None or epoch_ms < collector.min_date_ms:
        return
    hour = _hour_label(epoch_ms, collector.now_ms)
    if hour is None:
        return
    model = _clean_model(box.get("model", "unknown-codex-model"))
    if model is None:
        return
    tokens = [int(delta[0]), int(delta[3]), int(delta[2]), int(delta[1])]
    if not collector.keep_record(
        "c:%s|%s|%s|%s" % (box.get("session"), timestamp, model, tokens)
    ):
        return
    collector.add(kind, filekey, model, None, hour, tokens, 0.0)
    session = _session_key(kind, box.get("session"))
    collector.sadd(kind, filekey, session, model, None, epoch_ms, tokens, 0.0)


def _collect_codex(collector, home, env, extra=()):
    override = env.get("CODEX_HOME")
    default_home = (
        os.path.expanduser(override)
        if override and os.path.isabs(os.path.expanduser(override))
        else os.path.join(home, ".codex")
    )
    # Extra codex roots are codex homes, as locally (sessions hangs under each).
    dirs = _dedup_dirs([os.path.join(h, "sessions") for h in [default_home, *extra]])
    t3_dirs = [d for d in _t3_roots(home, "codex") if os.path.isdir(d)]
    if not t3_dirs and not any(os.path.isdir(d) and not os.path.islink(d) for d in dirs):
        return "not_installed"

    def _accept(name):
        return name.startswith("rollout-") and name.endswith(".jsonl")

    _scan_kind_files(collector, "codex", dirs, _accept, _parse_codex_line, None, t3_dirs)
    return "ok"


def _zcode_fingerprint(db_path):
    """The database plus its write-ahead log, where zcode keeps new rows until a checkpoint."""
    stat = os.stat(db_path)
    print_ = {
        "size": stat.st_size,
        "mtimeMs": int(stat.st_mtime * 1000),
        "walSize": 0,
        "walMtimeMs": 0,
    }
    try:
        wal = os.stat(db_path + "-wal")
        print_["walSize"] = wal.st_size
        print_["walMtimeMs"] = int(wal.st_mtime * 1000)
    except OSError:
        pass
    return print_


def _same_zcode_fingerprint(prior, current):
    return isinstance(prior, dict) and all(
        prior.get(key, 0) == current.get(key, 0)
        for key in ("size", "mtimeMs", "walSize", "walMtimeMs")
    )


def _scan_zcode_db(collector, db_path, immutable):
    filekey = _filekey("zcode", db_path)
    try:
        current = _zcode_fingerprint(db_path)
    except OSError:
        return "not_installed"
    if immutable and current["walSize"] > 0:
        # An immutable open never reads the write-ahead log: rows still in it
        # appear once zcode checkpoints them.
        collector.wal_unread = True
    collector.pending.setdefault("zcode", {})[filekey] = current
    prior = collector.prior.get("zcode", {}).get(filekey)
    if _same_zcode_fingerprint(prior, current):
        collector.confirm_file("zcode", filekey)
        return "ok"
    # Model names and integers only; never raw_usage_json, provider metadata
    # or message tables. Read-only, immutable where requested.
    uri = "file:%s?mode=ro%s" % (
        db_path.replace("?", "%3F").replace("#", "%23"),
        "&immutable=1" if immutable else "",
    )
    if collector.zcode_records:
        return _scan_zcode_records(collector, filekey, uri)
    try:
        connection = sqlite3.connect(uri, uri=True, timeout=5.0)
    except sqlite3.Error:
        # Present but unreadable (locked mid-write, for example): never "ok".
        return "error"
    try:
        cursor = connection.cursor()
        cursor.execute(
            "SELECT model_id, provider_id,"
            " CAST(started_at / 3600000 AS INTEGER) * 3600,"
            " SUM(input_tokens), SUM(output_tokens),"
            " SUM(cache_read_input_tokens),"
            " SUM(cache_creation_input_tokens), COUNT(*)"
            " FROM model_usage WHERE started_at >= ?"
            " GROUP BY model_id, provider_id,"
            " CAST(started_at / 3600000 AS INTEGER)",
            (int(collector.min_date_ms),),
        )
        groups = cursor.fetchall()
    except sqlite3.Error:
        return "error"
    finally:
        try:
            connection.close()
        except sqlite3.Error:
            pass
    for group in groups:
        if collector.expired():
            collector.truncated = True
            return "ok"
        model = _clean_model(group[0])
        if model is None:
            continue
        provider = _clean_provider(group[1])
        hour_seconds = group[2]
        if not isinstance(hour_seconds, int):
            continue
        epoch_ms = hour_seconds * 1000
        if epoch_ms < collector.min_date_ms:
            continue
        sums = [_non_negative_number(value or 0) for value in group[3:7]]
        if any(value is None for value in sums):
            continue
        count = group[7]
        if not isinstance(count, int) or count <= 0:
            continue
        # input_tokens includes cache reads (same convention as Codex).
        net_input = max(0, int(sums[0]) - int(sums[2]))
        tokens = [net_input, int(sums[1]), int(sums[2]), int(sums[3])]
        hour = _hour_label(epoch_ms, collector.now_ms)
        if hour is None:
            continue
        key = ("zcode", filekey, model, provider or "", hour, False)
        row = collector.rows.get(key)
        if row is None:
            if len(collector.rows) >= MAX_ROWS:
                collector.truncated = True
                collector.row_cap = True
                return "ok"
            row = {
                "k": "zcode",
                "f": filekey,
                "m": model,
                "h": hour,
                "i": 0,
                "o": 0,
                "cr": 0,
                "cw": 0,
                "c": 0.0,
                "n": 0,
            }
            if provider:
                row["p"] = provider
            collector.rows[key] = row
        row["i"] += tokens[0]
        row["o"] += tokens[1]
        row["cr"] += tokens[2]
        row["cw"] += tokens[3]
        row["n"] += count
    # Sessions: the same integers grouped by the log's own session id, with
    # first and last event. Ids and integers only, like the hourly groups.
    # A fresh read-only open: the hourly query's connection is already closed.
    # A database without session ids (an older schema) keeps its hourly
    # rows; only the hourly query failing is an error.
    sgroups = []
    try:
        scon = sqlite3.connect(uri, uri=True, timeout=5.0)
    except sqlite3.Error:
        scon = None
    if scon is not None:
        try:
            scursor = scon.cursor()
            scursor.execute(
                "SELECT session_id, model_id, provider_id,"
                " MIN(started_at), MAX(started_at),"
                " SUM(input_tokens), SUM(output_tokens),"
                " SUM(cache_read_input_tokens),"
                " SUM(cache_creation_input_tokens), COUNT(*)"
                " FROM model_usage WHERE started_at >= ?"
                " GROUP BY session_id, model_id, provider_id",
                (int(collector.min_date_ms),),
            )
            sgroups = scursor.fetchall()
        except sqlite3.Error:
            sgroups = []
        finally:
            try:
                scon.close()
            except sqlite3.Error:
                pass
    for group in sgroups:
        if collector.expired():
            collector.truncated = True
            return "ok"
        session = _session_key("zcode", group[0])
        model = _clean_model(group[1])
        if session is None or model is None:
            continue
        provider = _clean_provider(group[2])
        first = group[3]
        last = group[4]
        if (
            not isinstance(first, int)
            or not isinstance(last, int)
            or first < collector.min_date_ms
            or last < first
        ):
            continue
        sums = [_non_negative_number(value or 0) for value in group[5:9]]
        if any(value is None for value in sums):
            continue
        count = group[9]
        if not isinstance(count, int) or count <= 0:
            continue
        tokens = [max(0, int(sums[0]) - int(sums[2])), int(sums[1]), int(sums[2]), int(sums[3])]
        key = ("zcode", filekey, session, model, provider or "", False)
        srow = collector.srows.get(key)
        if srow is None:
            if len(collector.srows) >= MAX_SROWS:
                # Session aggregates stop here; the hourly groups above stay and
                # the database is still confirmed below.
                collector.truncated = True
                break
            srow = {
                "k": "zcode",
                "f": filekey,
                "s": session,
                "m": model,
                "a": first,
                "z": last,
                "i": 0,
                "o": 0,
                "cr": 0,
                "cw": 0,
                "c": 0.0,
                "n": 0,
            }
            if provider:
                srow["p"] = provider
            collector.srows[key] = srow
        if first < srow["a"]:
            srow["a"] = first
        if last > srow["z"]:
            srow["z"] = last
        srow["i"] += tokens[0]
        srow["o"] += tokens[1]
        srow["cr"] += tokens[2]
        srow["cw"] += tokens[3]
        srow["n"] += count
    # The session groups that fit were added: the database is confirmed whatever other kinds hit.
    collector.confirm_file("zcode", filekey)
    return "ok"


def _scan_zcode_records(collector, filekey, uri):
    """zcodeRecords mode: one entry per in-window usage row, keyed by a digest of the row's id
    and content, so a row copied into several databases (copied or continued zcode homes) is
    counted once by the server. Hashed keys, model names and integers only."""
    if collector.expired():
        collector.truncated = True
        return "ok"
    try:
        connection = sqlite3.connect(uri, uri=True, timeout=5.0)
    except sqlite3.Error:
        return "error"
    try:
        cursor = connection.cursor()
        columns = [row[1] for row in cursor.execute('PRAGMA table_info("model_usage")')]
        cursor.execute(
            "SELECT %s, %s, model_id, provider_id, started_at, input_tokens, output_tokens,"
            " cache_read_input_tokens, cache_creation_input_tokens"
            " FROM model_usage WHERE started_at >= ?"
            % (
                # A copied database keeps its row ids (and rowids): the key a copy shares.
                "id" if "id" in columns else "rowid",
                "session_id" if "session_id" in columns else "NULL",
            ),
            (int(collector.min_date_ms),),
        )
        found = cursor.fetchall()
    except sqlite3.Error:
        return "error"
    finally:
        try:
            connection.close()
        except sqlite3.Error:
            pass
    records = []
    for row in found:
        model = _clean_model(row[2])
        started = row[4]
        if model is None or not isinstance(started, int) or isinstance(started, bool):
            continue
        if started < collector.min_date_ms or _hour_label(started, collector.now_ms) is None:
            continue
        sums = [_non_negative_number(value or 0) for value in row[5:9]]
        if any(value is None for value in sums):
            continue
        sums = [int(value) for value in sums]
        row_id = row[0] if isinstance(row[0], (str, int)) and not isinstance(row[0], bool) else None
        session_id = row[1] if isinstance(row[1], str) else None
        identity = json.dumps([row_id, session_id, model, started] + sums)
        key = hashlib.sha256(("zcode-row:" + identity).encode("utf-8")).hexdigest()[:16]
        provider = _clean_provider(row[3]) or ""
        session = _session_key("zcode", session_id) or ""
        # input_tokens includes cache reads (same convention as the hourly rows).
        records.append(
            [key, session, model, provider, started, max(0, sums[0] - sums[2])] + sums[1:4]
        )
    if collector.zrec_count + len(records) > MAX_ZRECS:
        # Left unconfirmed: the next call reads it once the confirmed ones send nothing.
        collector.truncated = True
        return "ok"
    collector.zrecs[filekey] = records
    collector.zrec_count += len(records)
    collector.confirm_file("zcode", filekey)
    return "ok"


def _collect_zcode(collector, home, env, immutable, extra=()):
    override = env.get("ZCODE_DB_PATH")
    if override and os.path.isabs(os.path.expanduser(override)):
        db_path = os.path.expanduser(override)
    else:
        db_path = os.path.join(home, ".zcode", "cli", "db", "db.sqlite")
    seen = set()
    dbs = []
    for candidate in (db_path, *extra):
        absolute = os.path.abspath(candidate)
        if absolute not in seen:
            seen.add(absolute)
            dbs.append(absolute)
    states = []
    for candidate in dbs:
        if not os.path.isfile(candidate) or os.path.islink(candidate):
            continue
        states.append(_scan_zcode_db(collector, candidate, immutable))
    if not states or all(state == "not_installed" for state in states):
        return "not_installed"
    if "error" in states:
        return "error"
    return "ok"


# Antigravity: a port of T3 Code's reader (apps/server/src/usage/
# antigravityUsageReader.ts, tag v0.0.46-nightly.20261006.2735). Each
# conversation is one SQLite database; its usage is protobuf in
# gen_metadata.data and steps.metadata, apart from the conversation text.
# Only model ids and names, token counts, record ids and timestamps are taken
# from those blobs; nothing else is kept, and nothing but the totals leaves.
AGY_DIRS = ("antigravity", "antigravity-cli", "antigravity-ide", "antigravity-backup")
AGY_MAX_DBS = 2000
AGY_MAX_CANDIDATES = 200000
AGY_MAX_DEPTH = 32
AGY_SAFE_INTEGER = 2 ** 53 - 1
AGY_MODEL_IDS = {
    246: "gemini-2.5-pro", 312: "gemini-2.5-flash", 313: "gemini-2.5-flash-thinking",
    329: "gemini-2.5-flash-thinking", 330: "gemini-2.5-flash-lite", 281: "claude-sonnet-4",
    282: "claude-sonnet-4", 290: "claude-opus-4", 291: "claude-opus-4",
    333: "claude-sonnet-4-5", 334: "claude-sonnet-4-5", 340: "claude-haiku-4-5",
    341: "claude-haiku-4-5", 1026: "claude-opus-4-6", 1035: "claude-sonnet-4-6",
    1016: "gemini-3.1-pro", 1036: "gemini-3.1-pro", 1037: "gemini-3.1-pro",
    1018: "gemini-3-flash-preview", 1084: "gemini-3-flash-preview",
    1047: "gemini-3-flash-preview",
}
# String.prototype.trim()'s whitespace, so a name or id reads as T3 reads it.
AGY_TRIM = (
    "\t\n\x0b\x0c\r \xa0        "
    "        　﻿"
)
AGY_SUFFIX = re.compile("[%s]*\\([^)]*\\)[%s]*\\Z" % (AGY_TRIM, AGY_TRIM))
AGY_CLAUDE = re.compile(r"^claude-(4(?:\.[0-9]+)?)-(sonnet|opus|haiku)")
AGY_UNKNOWN = "antigravity-unknown"


class _AgyError(Exception):
    """A database T3's reader refuses as a whole: it contributes nothing."""


class _AgyBig(int):
    """A varint past 2**53 - 1: T3 holds it as a bigint, which no read takes as a number."""


def _agy_varint(data, offset):
    value = 0
    shift = 0
    while shift < 70:
        if offset >= len(data):
            raise _AgyError("truncated varint")
        byte = data[offset]
        offset += 1
        if shift == 63 and byte > 1:
            raise _AgyError("invalid varint")
        value |= (byte & 127) << shift
        if byte < 128:
            return (_AgyBig(value) if value > AGY_SAFE_INTEGER else value), offset
        shift += 7
    raise _AgyError("invalid varint")


def _agy_fields(data):
    """Field number -> values of one message; fixed32/64 fields are skipped, as in T3."""
    result = {}
    offset = 0
    while offset < len(data):
        tag, offset = _agy_varint(data, offset)
        if type(tag) is not int or tag // 8 == 0:
            raise _AgyError("invalid field")
        number, wire = tag // 8, tag % 8
        if wire == 0:
            value, offset = _agy_varint(data, offset)
        elif wire in (1, 2, 5):
            if wire == 2:
                length, offset = _agy_varint(data, offset)
                if type(length) is not int:
                    raise _AgyError("invalid length")
            else:
                length = 8 if wire == 1 else 4
            if length > len(data) - offset:
                raise _AgyError("truncated field")
            value = data[offset:offset + length]
            offset += length
            if wire != 2:
                continue
        else:
            raise _AgyError("unsupported wire type")
        result.setdefault(number, []).append(value)
    return result


def _agy_first(fields, key):
    values = fields.get(key)
    return values[0] if values else None


def _agy_number(fields, key):
    value = _agy_first(fields, key)
    return value if type(value) is int else 0


def _agy_bytes(fields, key):
    value = _agy_first(fields, key)
    return value if isinstance(value, memoryview) else None


def _agy_nested(fields, key):
    data = _agy_bytes(fields, key)
    return {} if data is None else _agy_fields(data)


def _agy_text(fields, key):
    data = _agy_bytes(fields, key)
    if data is None:
        return ""
    try:
        return data.tobytes().decode("utf-8").strip(AGY_TRIM)
    except UnicodeDecodeError:
        raise _AgyError("invalid text")


def _agy_timestamp(fields):
    seconds = _agy_number(fields, 1)
    return seconds * 1000 + _agy_number(fields, 2) // 1000000 if seconds > 0 else None


def _agy_model_name(name, model_id):
    if name:
        normalized = AGY_SUFFIX.sub("", name.lower(), count=1).replace(" ", "-")
        if normalized.startswith("claude-"):
            return AGY_CLAUDE.sub(r"claude-\2-\1", normalized, count=1).replace(".", "-")
        return normalized
    if model_id in AGY_MODEL_IDS:
        return AGY_MODEL_IDS[model_id]
    return "antigravity-model-%d" % model_id if model_id > 0 else ""


def _agy_metadata(blob, step):
    """(model, model id, named, timestamp ms, usages) of one step or generation."""
    root = _agy_fields(blob)
    if not step and _agy_bytes(root, 1) is None:
        raise _AgyError("missing generation metadata")
    data = root if step else _agy_nested(root, 1)
    model = _agy_nested(data, 24) if step else data
    usage = _agy_bytes(data, 9 if step else 4)
    usages = [] if usage is None else [_agy_fields(usage)]
    for retry in data.get(28 if step else 17, ()):
        if not isinstance(retry, memoryview):
            raise _AgyError("invalid retry metadata")
        retry_usage = _agy_bytes(_agy_fields(retry), 2)
        if retry_usage is not None:
            usages.append(_agy_fields(retry_usage))
    name = _agy_text(model, 12 if step else 19) or _agy_text(model, 8 if step else 21)
    model_id = _agy_number(model, 1 if step else 3)
    if step:
        stamp = _agy_timestamp(_agy_nested(data, 8))
        if stamp is None:
            stamp = _agy_timestamp(_agy_nested(data, 1))
    else:
        stamp = _agy_timestamp(_agy_nested(_agy_nested(data, 9), 4))
    return _agy_model_name(name, model_id), model_id, bool(name), stamp, usages


def _agy_blob(value):
    if not isinstance(value, bytes):
        raise _AgyError("invalid metadata blob")
    return memoryview(value)


def _agy_uri(path):
    text = os.path.abspath(path).replace("\\", "/")
    if not text.startswith("/"):
        text = "/" + text
    return "file://%s?mode=ro" % urllib.parse.quote(text, safe="/:")


def _agy_read_db(path, fallback_ms):
    """T3's readDatabase: the usage candidates of one conversation database.

    mode=ro (never immutable=1, which can misread a live write-ahead log), one
    short read transaction, a 100 ms busy wait, no writes and no checkpoint.
    """
    connection = sqlite3.connect(_agy_uri(path), uri=True, timeout=0.1, isolation_level=None)
    try:
        connection.execute("PRAGMA busy_timeout = 100")
        connection.execute("BEGIN")
        tables = set(
            row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type = 'table'")
        )
        if "gen_metadata" not in tables and "steps" not in tables:
            raise _AgyError("missing usage tables")
        # The metadata columns only, fetched in one snapshot; parsing waits until
        # the connection is closed, so no lock is held longer than the reads.
        raw_generations = raw_trajectory = raw_steps = ()
        if "gen_metadata" in tables:
            raw_generations = connection.execute(
                "SELECT idx, data FROM gen_metadata ORDER BY idx"
            ).fetchall()
        if "trajectory_metadata_blob" in tables:
            raw_trajectory = connection.execute(
                "SELECT data FROM trajectory_metadata_blob"
            ).fetchall()
        if "steps" in tables:
            raw_steps = connection.execute(
                "SELECT idx, metadata FROM steps WHERE metadata IS NOT NULL ORDER BY idx"
            ).fetchall()
    finally:
        connection.close()

    def entries(rows, step):
        found = []
        for idx, blob in rows:
            if isinstance(idx, bool) or not isinstance(idx, (int, float)):
                raise _AgyError("invalid metadata index")
            found.append((idx, _agy_metadata(_agy_blob(blob), step)))
        return found

    generations = entries(raw_generations, False)
    trajectory = None
    for (data,) in raw_trajectory:
        if trajectory is None:
            trajectory = _agy_timestamp(_agy_nested(_agy_fields(_agy_blob(data)), 2))
    steps = entries(raw_steps, True)
    names = []
    for _, (model, model_id, named, _, _) in steps + generations:
        if named and model_id > 0:
            names.append((model_id, model))
    positional = dict((idx, entry[0]) for idx, entry in generations)
    candidates = []
    for source, listed in (("step", steps), ("generation", generations)):
        for idx, (model, model_id, named, stamp, usages) in listed:
            for usage in usages:
                output = max(_agy_number(usage, 3), _agy_number(usage, 9) + _agy_number(usage, 10))
                tokens = [
                    _agy_number(usage, 2),
                    _agy_number(usage, 5),
                    _agy_number(usage, 4),
                    output,
                ]
                if sum(tokens) == 0:
                    continue
                keys = []
                for key in (11, 12, 7):
                    text = _agy_text(usage, key)
                    if text:
                        keys.append("antigravity:%d:%s" % (key, text))
                usage_id = _agy_number(usage, 1)
                t3_model = (
                    AGY_MODEL_IDS.get(usage_id)
                    or model
                    or (positional.get(idx) if source == "step" else "")
                    or _agy_model_name("", usage_id)
                    or AGY_UNKNOWN
                )
                candidates.append(
                    {
                        "keys": keys,
                        "stamp": stamp if stamp is not None else (
                            trajectory if trajectory is not None else fallback_ms
                        ),
                        "quality": 2 if stamp is not None else (1 if trajectory is not None else 0),
                        "tokens": tokens,
                        "t3_model": t3_model,
                        "static": AGY_MODEL_IDS.get(usage_id) or (model if named else ""),
                        "ids": (usage_id, model_id),
                    }
                )
    return candidates, names


def _agy_model(candidate, learned):
    """T3's model, except that a numeric id another record names (gen_metadata
    names id 1318 "gemini-3.8-flash" where steps carry only the id) takes that
    name instead of T3's guess by position, so one model is one row. Tokens and
    the set of unknown-model records are unchanged."""
    if candidate["static"]:
        return candidate["static"]
    usage_id, entry_id = candidate["ids"]
    if usage_id > 0 and usage_id in learned:
        return learned[usage_id]
    if entry_id in AGY_MODEL_IDS:
        return AGY_MODEL_IDS[entry_id]
    if entry_id > 0 and entry_id in learned:
        return learned[entry_id]
    return candidate["t3_model"]


def _agy_excluded(path, home):
    """A network mount, a UNC path or ~/PM-Experiments: never entered."""
    text = os.path.abspath(path)
    if text.startswith("\\\\") or text.startswith("//"):
        return True
    return _same_or_inside(
        text, ["/mnt", "/media", "/Volumes", "/net", os.path.join(home, "PM-Experiments")]
    )


def _agy_real_dir(path, home):
    """The real path of a root, resolved one component at a time: every link's
    target is checked as written before it is followed, so a link into a
    network mount (a stat there can block) or ~/PM-Experiments is never touched."""
    drive, rest = os.path.splitdrive(os.path.abspath(path))
    pieces = [piece for piece in re.split(r"[\\/]+", rest) if piece]
    current = drive + os.sep
    hops = 0
    while pieces:
        piece = pieces.pop(0)
        if piece == ".":
            continue
        if piece == "..":
            current = os.path.dirname(current)
            continue
        candidate = os.path.join(current, piece)
        if _agy_excluded(candidate, home):
            return None
        if not _is_link(candidate):
            current = candidate
            continue
        hops += 1
        try:
            written = os.readlink(candidate)
        except (OSError, ValueError, AttributeError, NotImplementedError):
            return None
        if hops > 40 or written.startswith("\\\\?\\UNC\\"):
            return None
        if written.startswith("\\\\?\\"):
            written = written[4:]
        target = os.path.normpath(os.path.join(current, written))
        if _agy_excluded(target, home):
            return None
        drive, rest = os.path.splitdrive(target)
        pieces = [piece for piece in re.split(r"[\\/]+", rest) if piece] + pieces
        current = drive + os.sep
    return None if _agy_excluded(current, home) else current


def _agy_dirs(home, env):
    """UsageService.ts:626-661: $ANTIGRAVITY_DATA_DIR (comma-separated), else the
    ~/.gemini stores and ~/.config/antigravity; then every T3 Code instance's
    <stateDir>/providers/antigravity/<sha256(id)>/antigravity-acp. A root with a
    `conversations` folder is read there only. Each real folder once."""
    listed = [part.strip() for part in (env.get("ANTIGRAVITY_DATA_DIR") or "").split(",")]
    listed = [os.path.expanduser(part) for part in listed if part]
    if not listed:
        listed = [os.path.join(home, ".gemini", name) for name in AGY_DIRS]
        listed.append(os.path.join(home, ".config", "antigravity"))
    instances = os.path.join(home, ".t3", "userdata", "providers", "antigravity")
    listed.extend(os.path.join(profile, "antigravity-acp") for profile in _t3_child_dirs(instances))
    dirs = []
    present = False
    for root in listed:
        real = _agy_real_dir(root, home)
        if real is None or not os.path.isdir(real):
            continue
        present = True
        nested = _agy_real_dir(os.path.join(real, "conversations"), home)
        chosen = nested if nested is not None and os.path.isdir(nested) else real
        if chosen not in dirs:
            dirs.append(chosen)
    return dirs, present


def _agy_walk(collector, directory, start, depth, state):
    """T3's walk (names in order, depth first) for `.db` files, each real file
    once. Links are followed only to a target inside the root; `secrets` folders
    and every other file (acp_token.json included) are never opened."""
    try:
        stat = os.stat(directory)
        if (stat.st_dev, stat.st_ino) in state["dirs"]:
            return True
        state["dirs"].add((stat.st_dev, stat.st_ino))
        names = sorted(os.listdir(directory))
    except FileNotFoundError:
        return True
    except OSError:
        state["unreadable"] += 1
        return True
    for name in names:
        if collector.expired() or len(state["dirs"]) > WALK_MAX_DIRS:
            collector.truncated = True
            return False
        path = os.path.join(directory, name)
        try:
            real = _t3_link_inside(path, start) if _is_link(path) else path
            if real is None:
                continue
            if os.path.isdir(real):
                if name == "secrets":
                    continue
                if depth >= AGY_MAX_DEPTH:
                    collector.truncated = True
                    continue
                if not _agy_walk(collector, real, start, depth + 1, state):
                    return False
            elif name.endswith(".db") and os.path.isfile(real):
                canonical = os.path.realpath(real)
                if canonical in state["seen"]:
                    continue
                state["seen"].add(canonical)
                if len(state["files"]) >= AGY_MAX_DBS:
                    collector.truncated = True
                    return False
                state["files"].append((real, name[:-3] if len(name) > 3 else name))
        except (OSError, ValueError):
            continue
    return True


def _agy_store_print(files):
    """One fingerprint for every database and write-ahead log: a record counts
    once across databases, so any change re-reads them all together."""
    digest = hashlib.sha256()
    size = 0
    newest = 0
    for path, _ in files:
        try:
            stat = os.stat(path)
        except OSError:
            continue
        wal = (0, 0)
        try:
            wal_stat = os.stat(path + "-wal")
            wal = (wal_stat.st_size, wal_stat.st_mtime_ns)
        except OSError:
            pass
        size += stat.st_size
        newest = max(newest, int(stat.st_mtime * 1000), wal[1] // 1000000)
        digest.update(
            ("%s\0%d\0%d\0%d\0%d\n" % (
                _filekey("antigravity", path), stat.st_size, stat.st_mtime_ns, wal[0], wal[1]
            )).encode("utf-8")
        )
    head = digest.hexdigest()
    return {"size": size, "mtimeMs": newest, "head": head, "tail": head}


def _collect_antigravity(collector, home, env):
    dirs, present = _agy_dirs(home, env)
    if not present:
        return "not_installed"
    state = {"dirs": set(), "seen": set(), "files": [], "unreadable": 0}
    for directory in dirs:
        if not _agy_walk(collector, directory, directory, 0, state):
            return "ok"
    files = state["files"]
    if not files:
        return "error" if state["unreadable"] else "ok"
    storekey = _filekey("antigravity", home)
    current = _agy_store_print(files)
    collector.pending.setdefault("antigravity", {})[storekey] = current
    if _same_fingerprint(collector.prior.get("antigravity", {}).get(storekey), current):
        collector.confirm_file("antigravity", storekey)
        return "ok"
    read = []
    named = collections.Counter()
    total = 0
    for path, session in files:
        if collector.expired():
            collector.truncated = True
            return "ok"
        try:
            candidates, names = _agy_read_db(path, os.stat(path).st_mtime * 1000.0)
        except Exception:
            # Busy past the wait, damaged, or not a conversation database: T3
            # drops the whole database too. One deleted since the walk is gone.
            if os.path.exists(path):
                state["unreadable"] += 1
            continue
        total += len(candidates)
        if total > AGY_MAX_CANDIDATES:
            collector.truncated = True
            return "ok"
        named.update(names)
        read.append((session[:SESSION_MAX_LEN], candidates))
    learned = {}
    for (model_id, model), _ in sorted(named.items(), key=lambda item: (-item[1], item[0])):
        learned.setdefault(model_id, model)
    # T3's alias merge (union-find over every record id, across databases): a
    # merged record keeps the earliest owner, the best timestamp and the
    # largest count of each token kind.
    groups = []
    identities = {}

    def find(index):
        root = index
        while groups[root]["parent"] != root:
            root = groups[root]["parent"]
        while index != root:
            parent = groups[index]["parent"]
            groups[index]["parent"] = root
            index = parent
        return root

    def merge(left, right):
        a, b = find(left), find(right)
        if a == b:
            return
        if groups[a]["size"] < groups[b]["size"]:
            a, b = b, a
        target, source = groups[a], groups[b]
        first = target if target["owner"] < source["owner"] else source
        best = source if (
            source["quality"] > target["quality"]
            or (source["quality"] == target["quality"] and source["stamp"] < target["stamp"])
        ) else target
        model = first["model"]
        if model == AGY_UNKNOWN:
            model = source["model"] if first is target else target["model"]
        target["tokens"] = [max(x, y) for x, y in zip(target["tokens"], source["tokens"])]
        target["model"] = model
        target["session"] = first["session"]
        target["stamp"] = best["stamp"]
        target["quality"] = best["quality"]
        target["owner"] = first["owner"]
        target["size"] += source["size"]
        source["parent"] = a

    for session, candidates in read:
        for candidate in candidates:
            index = len(groups)
            groups.append(
                {
                    "parent": index,
                    "size": 1,
                    "owner": index,
                    "session": session,
                    "model": _agy_model(candidate, learned),
                    "stamp": candidate["stamp"],
                    "quality": candidate["quality"],
                    "tokens": candidate["tokens"],
                }
            )
            for key in candidate["keys"]:
                existing = identities.get(key)
                if existing is not None:
                    merge(index, existing)
                identities[key] = index
    for index, group in enumerate(groups):
        if group["parent"] != index or group["stamp"] < collector.min_date_ms:
            continue
        epoch_ms = int(group["stamp"])
        hour = _hour_label(epoch_ms, collector.now_ms)
        if hour is None:
            continue
        model = _clean_model(group["model"]) or AGY_UNKNOWN
        uncached, cached, written, output = group["tokens"]
        tokens = (uncached, output, cached, written)
        collector.add("antigravity", storekey, model, None, hour, tokens, 0.0)
        collector.sadd(
            "antigravity",
            storekey,
            _session_key("antigravity", group["session"]),
            model,
            None,
            epoch_ms,
            tokens,
            0.0,
        )
    collector.agy_unreadable = state["unreadable"]
    if not read:
        return "error"
    if not state["unreadable"]:
        collector.confirm_file("antigravity", storekey)
    return "ok"


def _valid_extra_root(candidate):
    """A saved extra root: an absolute path with no parent escape."""
    if not isinstance(candidate, str) or not candidate or len(candidate) > 1024:
        return False
    if "\x00" in candidate:
        return False
    if ".." in candidate.replace("\\", "/").split("/"):
        return False
    if os.path.isabs(candidate):
        return True
    # A Windows path keeps its shape even when this host is not Windows.
    return bool(re.match(r"^[A-Za-z]:[\\/]", candidate))


def _read_request():
    raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
    if not raw or len(raw) > MAX_REQUEST_BYTES:
        _fail("request is missing or too large")
    try:
        request = json.loads(raw.decode("utf-8"))
    except ValueError:
        _fail("request is not valid JSON")
    if not isinstance(request, dict):
        _fail("request must be an object")
    kinds = request.get("kinds")
    if (
        not isinstance(kinds, list)
        or not kinds
        or any(
            kind not in ("claude", "codex", "omp", "muse", "zcode", "antigravity") for kind in kinds
        )
    ):
        _fail("request.kinds must list claude, codex, omp, muse, zcode and/or antigravity")
    min_date_ms = request.get("minDateMs")
    if (
        isinstance(min_date_ms, bool)
        or not isinstance(min_date_ms, (int, float))
        or min_date_ms < 0
    ):
        _fail("request.minDateMs must be a non-negative number")
    immutable = request.get("immutableSqlite", True)
    if not isinstance(immutable, bool):
        _fail("request.immutableSqlite must be a boolean")
    fingerprints = request.get("fingerprints", {})
    if not isinstance(fingerprints, dict):
        _fail("request.fingerprints must be an object")
    extra_roots = request.get("extraRoots", {})
    if (
        not isinstance(extra_roots, dict)
        or any(kind not in ("claude", "codex", "omp", "muse", "zcode") for kind in extra_roots)
        or any(
            not isinstance(paths, list)
            or len(paths) > (EXTRA_ZCODE_DBS_MAX if kind == "zcode" else EXTRA_ROOTS_MAX)
            or any(not _valid_extra_root(path) for path in paths)
            for kind, paths in extra_roots.items()
        )
    ):
        _fail("request.extraRoots must map kinds to absolute paths")
    zcode_records = request.get("zcodeRecords", False)
    if not isinstance(zcode_records, bool):
        _fail("request.zcodeRecords must be a boolean")
    deadline_ms = request.get("deadlineMs", 20000)
    if (
        isinstance(deadline_ms, bool)
        or not isinstance(deadline_ms, (int, float))
        or deadline_ms < 1000
        or deadline_ms > 120000
    ):
        _fail("request.deadlineMs must be within 1s and 120s")
    allowed = {
        "kinds",
        "minDateMs",
        "immutableSqlite",
        "fingerprints",
        "extraRoots",
        "deadlineMs",
        "zcodeRecords",
    }
    if any(key not in allowed for key in request):
        _fail("request has unknown fields")
    # Preserve order, drop duplicates.
    ordered = list(dict.fromkeys(kinds))
    return (
        ordered,
        int(min_date_ms),
        immutable,
        fingerprints,
        extra_roots,
        float(deadline_ms) / 1000.0,
        zcode_records,
    )


def _lower_priority():
    """Scan at low CPU and IO priority, so a scan (the server may run one per
    kind at once) never competes with the host's own foreground work: nice 10,
    plus utility-tier disk IO on macOS; on Windows, below-normal CPU priority
    and background mode, which also lowers IO and memory priority. Best-effort:
    a host that refuses keeps normal priority and the scan still runs."""
    try:
        if os.name == "nt":
            import ctypes

            kernel32 = ctypes.windll.kernel32
            process = kernel32.GetCurrentProcess()
            kernel32.SetPriorityClass(process, 0x00004000)  # BELOW_NORMAL_PRIORITY_CLASS
            kernel32.SetPriorityClass(process, 0x00100000)  # PROCESS_MODE_BACKGROUND_BEGIN
            return
        os.nice(10)
        if sys.platform == "darwin":
            import ctypes
            import ctypes.util

            libc = ctypes.CDLL(ctypes.util.find_library("c"))
            # setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, IOPOL_UTILITY)
            libc.setiopolicy_np(0, 0, 4)
    except Exception:
        pass


def main():
    kinds, min_date_ms, immutable, fingerprints, extra_roots, budget, zcode_records = (
        _read_request()
    )
    _lower_priority()
    home = _home()
    env = dict(os.environ)
    collector = Collector(min_date_ms, time.monotonic() + budget, fingerprints, budget)
    collector.zcode_records = zcode_records
    states = {}
    # Kinds whose own scan a cap or the deadline cut short: their numbers are
    # partial, unlike kinds that finished before a later kind hit a cap.
    partials = set()
    for kind in kinds:
        collector.fresh.setdefault(kind, {})
        cut_before = collector.truncated
        if kind == "claude":
            states[kind] = _collect_claude(collector, home, env, extra_roots.get("claude", ()))
        elif kind == "codex":
            states[kind] = _collect_codex(collector, home, env, extra_roots.get("codex", ()))
        elif kind == "omp":
            states[kind] = _collect_omp(collector, home, env, extra_roots.get("omp", ()))
        elif kind == "muse":
            states[kind] = _collect_muse(collector, home, env, extra_roots.get("muse", ()))
        elif kind == "zcode":
            states[kind] = _collect_zcode(
                collector, home, env, immutable, extra_roots.get("zcode", ())
            )
        elif kind == "antigravity":
            # Always mode=ro, whatever immutableSqlite says (see _agy_read_db).
            states[kind] = _collect_antigravity(collector, home, env)
        # A cap that outlives the kind that hit it (row_cap) cuts every later kind
        # short too, and a kind that ends past the deadline never flipped the flag
        # itself: mark partial where the data stops, not only where the flag flips.
        if (
            (collector.truncated and not cut_before)
            or collector.row_cap
            or collector.expired()
        ):
            partials.add(kind)
        if collector.expired():
            collector.truncated = True
            break
    if collector.truncated:
        confirmed = {key for prints in collector.fresh.values() for key in prints}
        collector.rows = {
            key: row for key, row in collector.rows.items() if row["f"] in confirmed
        }
        collector.srows = {
            key: srow for key, srow in collector.srows.items() if srow["f"] in confirmed
        }
        collector.zrecs = {
            key: records for key, records in collector.zrecs.items() if key in confirmed
        }
    kind_entries = {}
    for kind in kinds:
        # A kind the deadline skipped was never visited: "pending", never a
        # silent "ok" (the server reads pending as still scanning, not read).
        entry = {
            "state": states.get(kind, "pending"),
            "fingerprints": collector.fresh.get(kind, {}),
        }
        if kind in partials:
            entry["partial"] = True
        if kind == "zcode" and collector.wal_unread:
            entry["walUnread"] = True
        if kind == "antigravity" and collector.agy_unreadable:
            entry["unreadable"] = True
        kind_entries[kind] = entry
    response = {
        "version": VERSION,
        "truncated": collector.truncated,
        "discoveryTruncated": collector.discovery_truncated,
        "kinds": kind_entries,
        "rows": sorted(
            collector.rows.values(), key=lambda row: (row["h"], row["k"], row.get("p", ""), row["m"])
        ),
        "srows": sorted(
            collector.srows.values(),
            key=lambda srow: (srow["z"], srow["k"], srow["s"], srow["m"]),
        ),
    }
    if collector.zcode_records:
        response["zrecs"] = collector.zrecs
    sys.stdout.write(json.dumps(response))


if __name__ == "__main__":
    main()
