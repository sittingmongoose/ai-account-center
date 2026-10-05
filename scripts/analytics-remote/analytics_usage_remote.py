#!/usr/bin/env python3
"""Aggregate Claude Code, Codex, OMP, Muse and zcode usage into per-model, per-hour rows.

Reads one JSON request on stdin, scans the fixed default roots resolved on
this host plus the validated extra roots in the request, and prints
per-model, per-hour aggregates plus per-file fingerprints. Only model names,
providers, hour buckets, numeric token and cost sums and one hashed session key
per session aggregate leave the host; paths, session ids, prompts, tool output
and every other conversation content stay here.

Request (all fields validated, unknown fields rejected):
  {"kinds": ["claude", "codex", "omp", "muse", "zcode"], "minDateMs": 123,
   "immutableSqlite": true,
   "extraRoots": {"claude": ["/abs/projects"], "codex": ["/abs/.codex"],
                  "omp": ["/abs/sessions"], "muse": [...], "zcode": [...]},
   "fingerprints": {"claude": {"<filekey>": {"size": 1, "mtimeMs": 2}},
                   "codex": {...}, "omp": {...}, "muse": {...}, "zcode": {...}}}

Response:
  {"version": 1, "truncated": false, "discoveryTruncated": false,
   "kinds": {"omp": {"state": "ok"|"not_installed"|"error",
                     "fingerprints": {...}, "walUnread": true?}} ,
   "rows": [{"k": "omp", "f": "<filekey>", "m": "<model>", "p": "<provider>",
             "h": "2026-10-01 15:00", "i": 1, "o": 2, "cr": 3, "cw": 4,
             "c": 0.01, "n": 5}],
   "srows": [{"k": "omp", "f": "<filekey>", "s": "<session key>", "m": "<model>",
              "p": "<provider>", "a": 123, "z": 456, "i": 1, "o": 2, "cr": 3,
              "cw": 4, "c": 0.01, "n": 5}]}
Only files whose fingerprint is new or changed contribute rows; the caller
merges rows by filekey and drops rows whose filekey disappeared.

"s" is sha256("aac-session-v1:<kind>:<session id>") truncated to 16 hex
characters: the same key the server derives from its own local readers, so one
session groups across hosts and the id itself never leaves this one. A kind
whose records carry no session id contributes no session aggregate.

"truncated" means a file, row or deadline cap stopped the scan, so some files
were not read; "discoveryTruncated" means only that the search for custom OMP
session roots hit its bounds, so roots it did not reach were not read. A row
is either wholly logged (c > 0) or wholly unlogged (c == 0): events with and
without a logged cost never share a row. "error" means the kind's data exists
but could not be read; nothing of that kind is confirmed.
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
# Walk of one session root (the server collector's per-root ceilings).
WALK_MAX_DIRS = 10000
WALK_MAX_ENTRIES = 100000
# Breadth-first marker scan under ~/PM-Experiments for custom --session-dir
# roots, with the server scanner's bounds plus a share of the request budget.
SCAN_MAX_DEPTH = 6
SCAN_MAX_DIRS = 100000
SCAN_MAX_ENTRIES = 2000000
SCAN_BUDGET_SHARE = 0.4
SCAN_MAX_ROOTS = 128
SCAN_SKIP_DIRS = frozenset(["node_modules", ".git"])
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

    def expired(self):
        return time.monotonic() >= self.deadline

    def note_file(self, kind, path):
        """Return the filekey when the file needs parsing (new or changed)."""
        self.files_seen += 1
        if self.files_seen > MAX_FILES:
            self.truncated = True
            return None
        current = _fingerprint(path)
        if current is None:
            return None
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
        if os.path.basename(directory) == "sessions" and directory != base:
            if _sessions_dir_has_marker(directory):
                found.append(directory)
            continue
        if depth >= SCAN_MAX_DEPTH:
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


def _sessions_dir_has_marker(directory):
    """True when a sessions/ dir holds a *.jsonl file within depth 2.

    Custom --session-dir layouts name files freely, so the marker is
    presence, not naming; the OMP line parser rejects non-OMP records.
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
                    if name.endswith(".jsonl"):
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


def _scan_kind_files(collector, kind, dirs, accept, parse):
    """Scan dirs for kind's files, newest first; parse(line, collector, kind, filekey, box) reads one line with a per-file box."""
    stopped = False
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
    for path in _newest_first(paths):
        if collector.expired():
            collector.truncated = True
            return True
        filekey = collector.note_file(kind, path)
        if filekey is None:
            staged = _filekey(kind, path)
            if staged in collector.pending.get(kind, {}):
                collector.confirm_file(kind, staged)
            continue
        try:
            handle = open(path, "rb")
        except OSError:
            continue
        box = {}
        with handle:
            while True:
                if collector.expired():
                    collector.truncated = True
                    return True
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
                    return True
            collector.confirm_file(kind, filekey)
    return stopped


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
    # Claude Code logs no cost; the server prices the tokens at its rates.
    collector.add(kind, filekey, model, None, hour, tokens, 0.0)
    session = _session_key(kind, record.get("sessionId"))
    collector.sadd(kind, filekey, session, model, None, epoch_ms, tokens, 0.0)


def _collect_claude(collector, home, env, extra=()):
    override = env.get("CLAUDE_CONFIG_DIR")
    base = (
        os.path.expanduser(override)
        if override and os.path.isabs(os.path.expanduser(override))
        else os.path.join(home, ".claude")
    )
    dirs = _dedup_dirs([os.path.join(base, "projects"), *extra])
    if not any(os.path.isdir(d) and not os.path.islink(d) for d in dirs):
        return "not_installed"
    _scan_kind_files(collector, "claude", dirs, lambda name: name.endswith(".jsonl"), _parse_claude_line)
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
    if not any(os.path.isdir(d) and not os.path.islink(d) for d in dirs):
        return "not_installed"

    def _accept(name):
        return name.startswith("rollout-") and name.endswith(".jsonl")

    _scan_kind_files(collector, "codex", dirs, _accept, _parse_codex_line)
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
        or any(kind not in ("claude", "codex", "omp", "muse", "zcode") for kind in kinds)
    ):
        _fail("request.kinds must list claude, codex, omp, muse and/or zcode")
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
            or len(paths) > 16
            or any(not _valid_extra_root(path) for path in paths)
            for paths in extra_roots.values()
        )
    ):
        _fail("request.extraRoots must map kinds to absolute paths")
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
    )


def main():
    kinds, min_date_ms, immutable, fingerprints, extra_roots, budget = _read_request()
    home = _home()
    env = dict(os.environ)
    collector = Collector(min_date_ms, time.monotonic() + budget, fingerprints, budget)
    states = {}
    for kind in kinds:
        collector.fresh.setdefault(kind, {})
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
    kind_entries = {}
    for kind in kinds:
        entry = {"state": states.get(kind, "ok"), "fingerprints": collector.fresh.get(kind, {})}
        if kind == "zcode" and collector.wal_unread:
            entry["walUnread"] = True
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
    sys.stdout.write(json.dumps(response))


if __name__ == "__main__":
    main()
