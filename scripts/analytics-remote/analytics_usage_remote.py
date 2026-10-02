#!/usr/bin/env python3
"""Aggregate OMP, Muse and zcode usage into per-model, per-hour rows.

Reads one JSON request on stdin, scans the fixed default roots resolved on
this host, and prints per-model, per-hour aggregates plus per-file
fingerprints. Only model names, providers, hour buckets and numeric token and
cost sums leave the host; paths, session ids, prompts, tool output and every
other conversation content stay here.

Request (all fields validated, unknown fields rejected):
  {"kinds": ["omp", "muse", "zcode"], "minDateMs": 123,
   "immutableSqlite": true,
   "fingerprints": {"omp": {"<filekey>": {"size": 1, "mtimeMs": 2}},
                   "muse": {...}, "zcode": {...}}}

Response:
  {"version": 1, "truncated": false,
   "kinds": {"omp": {"state": "ok"|"not_installed", "fingerprints": {...}}} ,
   "rows": [{"k": "omp", "f": "<filekey>", "m": "<model>", "p": "<provider>",
             "h": "2026-10-01 15:00", "i": 1, "o": 2, "cr": 3, "cw": 4,
             "c": 0.01, "n": 5}]}
Only files whose fingerprint is new or changed contribute rows; the caller
merges rows by filekey and drops rows whose filekey disappeared.
"""

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
MAX_REQUEST_BYTES = 1024 * 1024
# Marker scan under ~/PM-Experiments for custom --session-dir roots.
SCAN_MAX_DEPTH = 6
SCAN_MAX_DIRS = 5000
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
    def __init__(self, min_date_ms, deadline, fingerprints):
        self.min_date_ms = min_date_ms
        self.deadline = deadline
        self.now_ms = int(time.time() * 1000)
        self.prior = fingerprints if isinstance(fingerprints, dict) else {}
        self.fresh = {}
        self.pending = {}
        self.rows = {}
        self.truncated = False
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
        if len(self.rows) >= MAX_ROWS:
            self.truncated = True
            return
        key = (kind, filekey, model, provider or "", hour)
        row = self.rows.get(key)
        if row is None:
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


def _iter_jsonl_files(root, accept, collector):
    """Yield files under root; caps flag truncation, unreadable is skipped."""
    pending = [(root, 0)]
    visited_dirs = 0
    visited_entries = 0
    while pending:
        directory, depth = pending.pop()
        if visited_dirs >= SCAN_MAX_DIRS:
            collector.truncated = True
            return
        visited_dirs += 1
        try:
            entries = sorted(os.listdir(directory))
        except OSError:
            continue
        for name in entries:
            if visited_entries >= 100000:
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


def _omp_roots(home, env, collector):
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
        collector.truncated = True
    return roots[:SCAN_MAX_ROOTS]


def _is_session_filename(name):
    if name == "__advisor.jsonl":
        return True
    if name.endswith(".jsonl"):
        stem = name[: -len(".jsonl")]
        return bool(SESSION_TS.match(stem) or SESSION_TS.match(name))
    # Custom --session-dir layouts also write extensionless <ts>_<uuid> files.
    return bool(SESSION_TS.match(name)) and "." not in name and "_" in name


def _scan_session_roots(base, collector):
    """Bounded marker scan for custom OMP session roots."""
    found = []
    if not os.path.isdir(base):
        return found
    pending = [(base, 0)]
    visited = 0
    while pending and len(found) < SCAN_MAX_ROOTS:
        if visited >= SCAN_MAX_DIRS:
            collector.truncated = True
            break
        directory, depth = pending.pop()
        if visited >= SCAN_MAX_DIRS or depth > SCAN_MAX_DEPTH:
            continue
        visited += 1
        try:
            entries = os.listdir(directory)
        except OSError:
            continue
        if os.path.basename(directory) == "sessions" and directory != base:
            if _sessions_dir_has_marker(directory):
                found.append(directory)
            continue
        if depth >= SCAN_MAX_DEPTH:
            continue
        for name in entries:
            if name in SCAN_SKIP_DIRS:
                continue
            path = os.path.join(directory, name)
            try:
                if os.path.isdir(path) and not os.path.islink(path):
                    pending.append((path, depth + 1))
            except OSError:
                continue
    if pending and len(found) >= SCAN_MAX_ROOTS:
        collector.truncated = True
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


def _parse_omp_line(line, collector, kind, filekey):
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
            import datetime

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


def _collect_omp(collector, home, env):
    roots = [
        root
        for root in _omp_roots(home, env, collector)
        if os.path.isdir(root) and not os.path.islink(root)
    ]
    if not roots:
        return "not_installed"

    def _accept(name):
        return name.endswith(".jsonl") or _is_session_filename(name)

    for root in roots:
        for path in _iter_jsonl_files(root, _accept, collector):
            if collector.expired():
                collector.truncated = True
                return "ok"
            filekey = collector.note_file("omp", path)
            if filekey is None:
                staged = _filekey("omp", path)
                if staged in collector.pending.get("omp", {}):
                    collector.confirm_file("omp", staged)
                continue
            try:
                handle = open(path, "rb")
            except OSError:
                continue
            with handle:
                while True:
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
                    _parse_omp_line(line, collector, "omp", filekey)
                    if collector.truncated:
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


def _parse_muse_line(line, collector, kind, filekey):
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
    tokens = [int(values[0]), int(values[1]), int(values[2]), int(values[3])]
    hour = _hour_label(epoch_ms, collector.now_ms)
    if hour is None:
        return
    collector.add(kind, filekey, model, None, hour, tokens, 0.0)


def _collect_muse(collector, home, env):
    override = env.get("MUSE_SESSIONS_DIR")
    sessions = (
        os.path.expanduser(override)
        if override and os.path.isabs(os.path.expanduser(override))
        else os.path.join(home, ".local", "share", "muse", "sessions")
    )
    if not os.path.isdir(sessions) or os.path.islink(sessions):
        return "not_installed"
    for path in _iter_jsonl_files(
        sessions, lambda name: name == "session.jsonl", collector
    ):
        if collector.expired():
            collector.truncated = True
            return "ok"
        filekey = collector.note_file("muse", path)
        if filekey is None:
            staged = _filekey("muse", path)
            if staged in collector.pending.get("muse", {}):
                collector.confirm_file("muse", staged)
            continue
        try:
            handle = open(path, "rb")
        except OSError:
            continue
        with handle:
            while True:
                if collector.expired():
                    collector.truncated = True
                    return "ok"
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
                _parse_muse_line(line, collector, "muse", filekey)
                if collector.truncated:
                    return "ok"
        collector.confirm_file("muse", filekey)
    return "ok"


def _collect_zcode(collector, home, env, immutable):
    override = env.get("ZCODE_DB_PATH")
    if override and os.path.isabs(os.path.expanduser(override)):
        db_path = os.path.expanduser(override)
    else:
        db_path = os.path.join(home, ".zcode", "cli", "db", "db.sqlite")
    if not os.path.isfile(db_path) or os.path.islink(db_path):
        return "not_installed"
    filekey = _filekey("zcode", db_path)
    try:
        stat = os.stat(db_path)
    except OSError:
        return "not_installed"
    current = {"size": stat.st_size, "mtimeMs": int(stat.st_mtime * 1000)}
    collector.pending.setdefault("zcode", {})[filekey] = current
    prior = collector.prior.get("zcode", {}).get(filekey)
    if (
        isinstance(prior, dict)
        and prior.get("size") == current["size"]
        and prior.get("mtimeMs") == current["mtimeMs"]
    ):
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
        return "ok"
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
        return "ok"
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
        key = ("zcode", filekey, model, provider or "", hour)
        row = collector.rows.get(key)
        if row is None:
            if len(collector.rows) >= MAX_ROWS:
                collector.truncated = True
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
    if not collector.truncated:
        collector.confirm_file("zcode", filekey)
    return "ok"


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
        or any(kind not in ("omp", "muse", "zcode") for kind in kinds)
    ):
        _fail("request.kinds must list omp, muse and/or zcode")
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
    deadline_ms = request.get("deadlineMs", 20000)
    if (
        isinstance(deadline_ms, bool)
        or not isinstance(deadline_ms, (int, float))
        or deadline_ms < 1000
        or deadline_ms > 120000
    ):
        _fail("request.deadlineMs must be within 1s and 120s")
    allowed = {"kinds", "minDateMs", "immutableSqlite", "fingerprints", "deadlineMs"}
    if any(key not in allowed for key in request):
        _fail("request has unknown fields")
    # Preserve order, drop duplicates.
    ordered = list(dict.fromkeys(kinds))
    return ordered, int(min_date_ms), immutable, fingerprints, float(deadline_ms) / 1000.0


def main():
    kinds, min_date_ms, immutable, fingerprints, budget = _read_request()
    home = _home()
    env = dict(os.environ)
    collector = Collector(min_date_ms, time.monotonic() + budget, fingerprints)
    states = {}
    for kind in kinds:
        collector.fresh.setdefault(kind, {})
        if kind == "omp":
            states[kind] = _collect_omp(collector, home, env)
        elif kind == "muse":
            states[kind] = _collect_muse(collector, home, env)
        elif kind == "zcode":
            states[kind] = _collect_zcode(collector, home, env, immutable)
        if collector.expired():
            collector.truncated = True
            break
    if collector.truncated:
        confirmed = {key for prints in collector.fresh.values() for key in prints}
        collector.rows = {
            key: row for key, row in collector.rows.items() if row["f"] in confirmed
        }
    response = {
        "version": VERSION,
        "truncated": collector.truncated,
        "kinds": {
            kind: {"state": states.get(kind, "ok"), "fingerprints": collector.fresh.get(kind, {})}
            for kind in kinds
        },
        "rows": sorted(
            collector.rows.values(), key=lambda row: (row["h"], row["k"], row.get("p", ""), row["m"])
        ),
    }
    sys.stdout.write(json.dumps(response))


if __name__ == "__main__":
    main()
