"""Offline scanner/remote-helper checks on each native OS. Every value is made up."""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile


def run(binary, helper):
    with tempfile.TemporaryDirectory(prefix="ancur-parity-") as directory:
        home = Path(directory)
        sdk = home / ".cursor/projects/project/sdk-agent-store/store/index.db"
        sdk.parent.mkdir(parents=True)
        with sqlite3.connect(sdk) as db:
            db.executescript("""
                CREATE TABLE runs (run_id TEXT, agent_id TEXT, model TEXT,
                  started_at TEXT, finished_at TEXT, usage_json TEXT);
                INSERT INTO runs VALUES ('fixture-run', 'fixture-agent', 'fixture-model',
                  '2026-11-11T21:00:00Z', '2026-11-11T21:01:00Z',
                  '{"inputTokens":1000,"outputTokens":50,"cacheReadTokens":800,
                    "cacheWriteTokens":100,"totalTokens":1050}');
            """)
        chat = home / "config/cursor/chats/project/chat/store.db"
        chat.parent.mkdir(parents=True)
        with sqlite3.connect(chat) as db:
            db.executescript("CREATE TABLE blobs (id TEXT, data BLOB); CREATE TABLE meta (key TEXT, value TEXT);")
        session = home / ".omp/agent/sessions/2026-11-11T21-00-00_fixture.jsonl"
        session.parent.mkdir(parents=True)
        session.write_text(json.dumps({
            "type": "message", "timestamp": "2026-11-11T21:01:00Z",
            "message": {"role": "assistant", "model": "fixture-model", "provider": "fixture-provider",
                        "usage": {"input": 200, "output": 30, "cacheRead": 40, "cacheWrite": 5,
                                  "cost": {"total": 0.123}}},
        }) + "\n", encoding="utf-8")
        env = {**os.environ, "HOME": str(home), "USERPROFILE": str(home),
               "XDG_CONFIG_HOME": str(home / "config"), "XDG_DATA_HOME": str(home / "data"),
               "CCS_HOME": str(home / "ccs")}
        for key in ("HOMEDRIVE", "HOMEPATH", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "PI_CODING_AGENT_DIR"):
            env.pop(key, None)

        def request(command, kinds, prints=None):
            value = {"kinds": kinds, "minDateMs": 1790000000000, "deadlineMs": 20000,
                     "fingerprints": prints or {}}
            return json.loads(subprocess.check_output(command, input=json.dumps(value).encode(), env=env, timeout=30))

        first = request([binary], ["cursor", "omp"])
        cursor = [r for r in first["rows"] if r["k"] == "cursor"]
        assert len(cursor) == 1 and [cursor[0][k] for k in ("i", "o", "cr", "cw")] == [100, 50, 800, 100]
        assert cursor[0]["c"] == 0, "No SDK cost was recorded"
        found = first["discovery"]["kinds"]["cursor"]
        assert found["freshness"]["state"] == "stale"
        assert found["freshness"]["newestDataMs"] == 1794430860000
        assert found["unmeasuredFiles"] == 1
        prints = {k: v["fingerprints"] for k, v in first["kinds"].items()}
        second = request([binary], ["cursor", "omp"], prints)
        assert second["rows"] == [], "An unchanged file must not duplicate usage"
        assert second["discovery"]["kinds"]["cursor"]["freshness"] == found["freshness"]
        # The legacy remote helper has no SDK reader. Its established OMP format must still
        # produce exactly the same measured hourly/session records on this OS.
        native = request([binary], ["omp"])
        python = request([sys.executable, helper], ["omp"])
        for key in ("rows", "srows"):
            assert native[key] == python[key], (key, native[key], python[key])
        print(json.dumps({"result": "PASS", "os": sys.platform,
                          "checks": ["sdk tokens", "cli unmeasured", "stale dates", "unchanged replay", "remote-helper parity"]}))


if __name__ == "__main__":
    run(str(Path(sys.argv[1]).resolve()), str(Path(sys.argv[2]).resolve()))
