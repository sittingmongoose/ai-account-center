"""M4b claude-host parity on fixture homes (lane F).

Runs the reference Mac program (`claude-host-mac-program.ts`) and the Rust `claude-host`
verb on identical temporary homes and compares the answers and the resulting trees byte
for byte; then the fixture-safe Windows steps (session, state, purge) against the real
`windowsHostScript` run locally behind the reference bootstrap. Purge permits are issued
here exactly as the front door does; the verb must refuse without one.

    python tests/spec/agent/claude_host_parity.py <aac-agent-binary>

The binary must be built with --features parity-fixtures. No real home, app, scheduled
task, shell folder or provider is touched: every root is a fresh temporary directory.
"""

import base64
import hashlib
import hmac
import io
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

REPO = pathlib.Path(__file__).resolve().parents[3]
NOW_MS = 1_700_000_000_000
EXPIRES_MS = NOW_MS + 120_000
TRASH_NAME = "work2-20261002T080000Z"
UUID = "12345678-90ab-cdef-1234-567890abcdef"
CACHE = "djEwdGhpcyBpcyBub3QgYSByZWFsIHRva2Vu"

COMPARED = 0


def fail(message):
    raise AssertionError(message)


def mac_program():
    text = (REPO / "src/web-server/services/claude-host-mac-program.ts").read_text(encoding="utf-8")
    start = text.index("String.raw`") + len("String.raw`")
    end = text.index("`", start)
    return text[start:end]


PROGRAM = mac_program()


def purge_permit(host, trash_name):
    policy = json.loads((REPO / "config/tables/agent-transport-policy.json").read_text(encoding="utf-8"))
    key = bytes.fromhex(policy["claudeHostPurgeKey"])
    message = f"aac-claude-host-purge\0{host}\0{trash_name}\0{EXPIRES_MS}".encode("utf-8")
    return f"{EXPIRES_MS}.{hmac.new(key, message, hashlib.sha256).hexdigest()}"


def run_reference(home, request):
    """The fixed Python program, exactly as the Mac bootstrap runs it (REQUEST + stdin JSON)."""
    saved_home = os.environ.get("HOME")
    saved_profile = os.environ.get("USERPROFILE")
    saved_stdout = sys.stdout
    os.environ["HOME"] = str(home)
    os.environ["USERPROFILE"] = str(home)
    buffer = io.StringIO()
    sys.stdout = buffer
    code = 0
    # The program's production platform is macOS, where text writes keep '\n'; emulate
    # that on Windows fixtures so the launch script's bytes compare raw.
    original_write_text = pathlib.Path.write_text

    def posix_write_text(self, data, encoding=None, errors=None, newline=None):
        return original_write_text(self, data, encoding=encoding, errors=errors, newline="\n")

    pathlib.Path.write_text = posix_write_text
    try:
        exec(compile(PROGRAM, "aac-claude-host", "exec"), {"REQUEST": request, "__name__": "__main__"})
    except SystemExit as exit_code:
        code = exit_code.code if isinstance(exit_code.code, int) else 1
    finally:
        pathlib.Path.write_text = original_write_text
        sys.stdout = saved_stdout
        if saved_home is None:
            os.environ.pop("HOME", None)
        else:
            os.environ["HOME"] = saved_home
        if saved_profile is None:
            os.environ.pop("USERPROFILE", None)
        else:
            os.environ["USERPROFILE"] = saved_profile
    value = None
    if code == 0:
        value = json.loads(buffer.getvalue())
    return code, value


def run_agent(binary, args):
    request = {"protocol": 1, "verb": "claude-host", "id": "parity", "deadlineMs": 30000, "args": args}
    result = subprocess.run(
        [binary], input=json.dumps(request) + "\n", capture_output=True, text=True, timeout=60
    )
    lines = [line for line in result.stdout.strip().splitlines() if line]
    if len(lines) != 1:
        fail(f"agent reply lines: {result.stdout!r} {result.stderr!r}")
    event = json.loads(lines[0])
    if event.get("type") == "result":
        return 0, event["value"]
    cause = event.get("failure", {}).get("cause")
    code = {"refused": 1, "in_use": 3, "damaged": 2, "confirmation_invalid": 1, "confirmation_expired": 1}.get(cause)
    if code is None:
        fail(f"unexpected agent cause {cause!r}")
    return code, cause


def snapshot(root):
    """Every entry's kind and bytes; modes on POSIX. The byte-identical-tree gate.
    The home prefix inside file bytes (the launch script's --user-data-dir) is named
    <HOME>, since the two sides run on different fixture roots."""
    entries = {}
    root = pathlib.Path(root)
    home_bytes = str(root).encode("utf-8")
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root).as_posix()
        if path.is_symlink():
            entries[rel] = ("link", os.readlink(path).replace(str(root), "<HOME>"))
        elif path.is_dir():
            mode = path.stat().st_mode & 0o777 if os.name == "posix" else None
            entries[rel] = ("dir", mode)
        else:
            mode = path.stat().st_mode & 0o777 if os.name == "posix" else None
            entries[rel] = ("file", path.read_bytes().replace(home_bytes, b"<HOME>"), mode)
    return entries


def normalize(value, home):
    text = json.dumps(value, sort_keys=True)
    return text.replace(str(home).replace("\\", "\\\\"), "<HOME>").replace(str(home), "<HOME>")


def mac_envelope(request, home):
    """The reference request is flat; the verb envelope nests the launcher record."""
    envelope = {"host": "mac", "home": str(home), "nowMs": NOW_MS, "op": request["op"]}
    for key in ("profileId", "trashName", "token", "psOutput"):
        if key in request:
            envelope[key] = request[key]
    if "profilePath" in request:
        envelope["launcher"] = {
            key: request[key]
            for key in ("launcherName", "launcherPath", "profilePath", "startMenuPath", "sshHost")
            if key in request
        }
    return envelope

def compare_step(name, binary, ref_home, agent_home, request, agent_extra=None, expect_code=0):
    global COMPARED
    ref_code, ref_value = run_reference(ref_home, request)
    # The request carries the reference home's paths; the agent side gets its own.
    translated = {
        key: value.replace(str(ref_home), str(agent_home)) if isinstance(value, str) else value
        for key, value in request.items()
    }
    args = mac_envelope(translated, agent_home)
    if agent_extra:
        args.update(agent_extra)
    agent_code, agent_value = run_agent(binary, args)
    if ref_code != expect_code:
        fail(f"{name}: reference exit {ref_code}, expected {expect_code}")
    if agent_code != ref_code:
        fail(f"{name}: agent {agent_code} ({agent_value}) != reference {ref_code}")
    if ref_code == 0 and normalize(ref_value, ref_home) != normalize(agent_value, agent_home):
        fail(f"{name}: values differ: {ref_value!r} vs {agent_value!r}")
    ref_tree, agent_tree = snapshot(ref_home), snapshot(agent_home)
    if ref_tree != agent_tree:
        only_ref = {k: v for k, v in ref_tree.items() if agent_tree.get(k) != v}
        only_agent = {k: v for k, v in agent_tree.items() if ref_tree.get(k) != v}
        fail(f"{name}: trees differ:\n  reference-only {only_ref}\n  agent-only {only_agent}")
    COMPARED += 1


def launcher_record(home, pid="work2"):
    home = pathlib.Path(home)
    return {
        "launcherName": f"Claude ({pid})",
        "launcherPath": str(home / "Applications" / f"Claude ({pid}).app"),
        "profilePath": str(home / "Library" / "Application Support" / f"Claude-{pid}"),
        "sshHost": "mac-host",
    }


def mac_parity(binary):
    global COMPARED
    roots = tempfile.mkdtemp(prefix="aac-claude-parity-")
    try:
        ref = pathlib.Path(roots) / "ref"
        agent = pathlib.Path(roots) / "agent"
        ref.mkdir()
        agent.mkdir()

        # Create, and the double-create refusal.
        compare_step("create", binary, ref, agent, {"op": "create", "profileId": "work2"})
        compare_step("create-twice", binary, ref, agent, {"op": "create", "profileId": "work2"}, expect_code=3)

        # Trash with data, restore (byte-identical tree), trash again, permitted purge.
        for home in (ref, agent):
            (home / "Library" / "Application Support" / "Claude-work2" / "Local State").write_bytes(b"signed in")
        record = launcher_record(ref)
        agent_record = launcher_record(agent)
        compare_step(
            "trash", binary, ref, agent,
            {"op": "trash", "profileId": "work2", "trashName": TRASH_NAME, **record},
        )
        # The launcher record's paths are home-specific; rebuild for each side.
        # (compare_step passed the ref record to both; redo with per-side records.)
        compare_step(
            "restore", binary, ref, agent,
            {"op": "restore", "profileId": "work2", "trashName": TRASH_NAME, **record},
        )
        compare_step(
            "trash-again", binary, ref, agent,
            {"op": "trash", "profileId": "work2", "trashName": TRASH_NAME, **record},
        )
        # Purge: the reference has no permit; the agent must refuse without one and
        # accept the front door's. Run the reference alone, then the agent both ways.
        ref_code, _ = run_reference(ref, {"op": "purge", "trashName": TRASH_NAME})
        if ref_code != 0:
            fail("reference purge failed")
        code, cause = run_agent(binary, {"op": "purge", "host": "mac", "home": str(agent), "trashName": TRASH_NAME, "nowMs": NOW_MS})
        if code != 1 or cause != "confirmation_invalid":
            fail(f"purge without permit: {code} {cause}")
        code, cause = run_agent(binary, {"op": "purge", "host": "mac", "home": str(agent), "trashName": TRASH_NAME, "nowMs": NOW_MS, "token": "forged"})
        if code != 1 or cause != "confirmation_invalid":
            fail(f"purge with forged permit: {code} {cause}")
        code, cause = run_agent(binary, {"op": "purge", "host": "mac", "home": str(agent), "trashName": TRASH_NAME, "nowMs": EXPIRES_MS + 1, "token": purge_permit("mac", TRASH_NAME)})
        if code != 1 or cause != "confirmation_expired":
            fail(f"purge with expired permit: {code} {cause}")
        code, value = run_agent(binary, {"op": "purge", "host": "mac", "home": str(agent), "trashName": TRASH_NAME, "nowMs": NOW_MS, "token": purge_permit("mac", TRASH_NAME)})
        if code != 0 or value != {"ok": True}:
            fail(f"permitted purge: {code} {value}")
        if snapshot(ref) != snapshot(agent):
            fail("purge trees differ")
        COMPARED += 1

        # Undo keeps a used folder, removes an empty one.
        compare_step("create-work3", binary, ref, agent, {"op": "create", "profileId": "work3"})
        for home in (ref, agent):
            (home / "Library" / "Application Support" / "Claude-work3" / "keep").write_bytes(b"x")
        compare_step("undo-used", binary, ref, agent, {"op": "undo", "profileId": "work3", **launcher_record(ref, "work3")})
        compare_step("create-work4", binary, ref, agent, {"op": "create", "profileId": "work4"})
        compare_step("undo-empty", binary, ref, agent, {"op": "undo", "profileId": "work4", **launcher_record(ref, "work4")})

        # The purge removed work2's data folder; the session cases need it back.
        for home in (ref, agent):
            (home / "Library" / "Application Support" / "Claude-work2").mkdir(parents=True, exist_ok=True)
        # Session markers.
        def session_case(name, config_bytes, expect):
            for home in (ref, agent):
                config = home / "Library" / "Application Support" / "Claude-work2" / "config.json"
                if config_bytes is None:
                    config.unlink(missing_ok=True)
                else:
                    config.write_bytes(config_bytes)
            expect_code = 1 if config_bytes is not None and len(config_bytes) > 1_048_576 else 0
            compare_step(name, binary, ref, agent, {"op": "session", **launcher_record(ref)}, expect_code=expect_code)
            if expect is not None:
                _, value = run_agent(binary, mac_envelope({"op": "session", **launcher_record(agent)}, agent))
                if value != {"signedIn": expect}:
                    fail(f"{name}: agent {value}")

        session_case("session-absent", None, False)
        session_case("session-signed-in", json.dumps({"lastKnownAccountUuid": UUID, "oauth:tokenCacheV2": CACHE}).encode(), True)
        session_case("session-no-cache", json.dumps({"lastKnownAccountUuid": UUID}).encode(), False)
        session_case("session-no-uuid", json.dumps({"oauth:tokenCacheV2": CACHE}).encode(), False)
        session_case("session-bad-json", b"{oops", False)
        session_case("session-empty-cache", json.dumps({"lastKnownAccountUuid": UUID, "oauth:tokenCacheV2": ""}).encode(), False)
        session_case("session-array", b"[1,2]", False)
        session_case("session-oversized", b"x" * 1_048_577, None)  # exit 1 both sides
        # Re-create work2 for the remaining cases (oversized left a broken config).
        (agent / "Library" / "Application Support" / "Claude-work2" / "config.json").unlink(missing_ok=True)
        (ref / "Library" / "Application Support" / "Claude-work2" / "config.json").unlink(missing_ok=True)

        # Path refusals: nothing outside the fixed folders is touched.
        for home in (ref, agent):
            (home / "outside").mkdir(exist_ok=True)
            (home / "outside" / "precious").write_bytes(b"x")
        bad_records = [
            {**launcher_record(ref), "profilePath": str(ref / "outside")},
            {**launcher_record(ref), "profilePath": str(ref / "Library" / "Application Support" / ".." / "outside")},
            {**launcher_record(ref), "launcherPath": str(ref / "outside")},
        ]
        for i, bad in enumerate(bad_records):
            agent_bad = {**bad, "profilePath": bad["profilePath"].replace(str(ref), str(agent)), "launcherPath": bad["launcherPath"].replace(str(ref), str(agent))}
            ref_code, _ = run_reference(ref, {"op": "trash", "profileId": "work2", "trashName": TRASH_NAME, **bad})
            code, _ = run_agent(binary, mac_envelope({"op": "trash", "profileId": "work2", "trashName": TRASH_NAME, **agent_bad}, agent))
            if ref_code != 1 or code != 1:
                fail(f"bad-record/{i}: reference {ref_code}, agent {code}")
            COMPARED += 1
        ref_code, _ = run_reference(ref, {"op": "purge", "trashName": "../../outside"})
        code, cause = run_agent(binary, {"op": "purge", "host": "mac", "home": str(agent), "nowMs": NOW_MS, "trashName": "../../outside", "token": purge_permit("mac", "../../outside")})
        if ref_code != 1 or code != 1:
            fail(f"purge traversal: reference {ref_code}, agent {code} {cause}")
        if (ref / "outside" / "precious").read_bytes() != b"x" or (agent / "outside" / "precious").read_bytes() != b"x":
            fail("a refusal touched the outside folder")
        COMPARED += 1

        # Trash with no profile creates the empty trash folder; restore refuses conflicts.
        compare_step("create-work5", binary, ref, agent, {"op": "create", "profileId": "work5"})
        for home in (ref, agent):
            shutil.rmtree(home / "Library" / "Application Support" / "Claude-work5")
        compare_step("trash-missing-profile", binary, ref, agent, {"op": "trash", "profileId": "work5", "trashName": "work5-20261002T080000Z", **launcher_record(ref, "work5")})
        # A restore over an existing profile folder is the conflict (exit 3).
        for home in (ref, agent):
            (home / "Library" / "Application Support" / "Claude-work5").mkdir(parents=True, exist_ok=True)
        compare_step("restore-conflict", binary, ref, agent, {"op": "restore", "profileId": "work5", "trashName": "work5-20261002T080000Z", **launcher_record(ref, "work5")}, expect_code=3)

        # The state needle rule on a captured listing (the reference expression itself).
        profile = str(agent / "Library" / "Application Support" / "Claude-work2")
        needle = f"--user-data-dir={profile}"
        listing = f"/usr/bin/open {needle}\n/sbin/launchd\n/usr/bin/open {needle} --flag\n"
        expected = any(line.endswith(needle) or (needle + " ") in line for line in listing.splitlines())
        code, value = run_agent(binary, mac_envelope({"op": "state", "psOutput": listing, **launcher_record(agent)}, agent))
        if code != 0 or value != {"running": expected}:
            fail(f"state fixture: {code} {value}")
        COMPARED += 1
    finally:
        shutil.rmtree(roots, ignore_errors=True)


def windows_script(op, values):
    """The real TS builder, via bun, for the fixture's launcher record."""
    script = (
        "import { windowsHostScript } from './src/web-server/services/claude-host-transport';"
        f"console.log(windowsHostScript({json.dumps(op)}, {json.dumps(values)}));"
    )
    result = subprocess.run(
        ["bun", "-e", script], capture_output=True, text=True, cwd=REPO, timeout=60,
        env={**os.environ, "CCS_HOME": str(REPO / ".tmp-ccs")},
    )
    if result.returncode != 0:
        fail(f"windowsHostScript failed: {result.stderr}")
    return result.stdout


def run_windows_reference(home, script, redirect_userprofile=True):
    bootstrap = "$s = [Console]::In.ReadToEnd(); & ([scriptblock]::Create($s)); exit $LASTEXITCODE"
    encoded = base64.b64encode(bootstrap.encode("utf-16-le")).decode("ascii")
    env = {
        **os.environ,
        "APPDATA": str(home / "AppData" / "Roaming"),
        "LOCALAPPDATA": str(home / "AppData" / "Local"),
    }
    # GetFolderPath expands %USERPROFILE% from the registry: Check-Link needs the real
    # one. Session and state never touch the trash root, so they keep it; purge redirects.
    if redirect_userprofile:
        env["USERPROFILE"] = str(home)
    result = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
        input=script, capture_output=True, text=True, timeout=60, env=env,
    )
    value = None
    if result.returncode == 0:
        value = json.loads(result.stdout.strip())
    return result.returncode, value


def special_folders():
    """The real Desktop/Programs paths. Check-Link only string-compares against them;
    the session and state steps never read or write the link files, so naming them is
    safe while every data path stays in the fixture home."""
    script = "[Console]::Write(([Environment]::GetFolderPath('Desktop') + '|' + [Environment]::GetFolderPath('Programs')))"
    result = subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command", script], capture_output=True, text=True, timeout=30
    )
    desktop, programs = result.stdout.strip().split("|")
    return pathlib.Path(desktop), pathlib.Path(programs)

def windows_parity(binary):
    global COMPARED
    if os.name != "nt":
        print("windows parity: skipped (no powershell on this OS)")
        return
    roots = tempfile.mkdtemp(prefix="aac-claude-win-")
    try:
        ref = pathlib.Path(roots) / "ref"
        agent = pathlib.Path(roots) / "agent"
        for home in (ref, agent):
            (home / "AppData" / "Roaming").mkdir(parents=True)
            (home / "AppData" / "Local").mkdir(parents=True)

        desktop, programs = special_folders()

        def record(home):
            return {
                "launcherName": "Claude (party)",
                "launcherPath": str(desktop / "Claude (party).lnk"),
                "startMenuPath": str(programs / "Claude (party).lnk"),
                "profilePath": str(home / "AppData" / "Roaming" / "Claude-party"),
                "sshHost": "windows-host",
            }

        def session_case(name, config_bytes, expect):
            global COMPARED
            for home in (ref, agent):
                profile = home / "AppData" / "Roaming" / "Claude-party"
                profile.mkdir(exist_ok=True)
                config = profile / "config.json"
                if config_bytes is None:
                    config.unlink(missing_ok=True)
                else:
                    config.write_bytes(config_bytes)
            script = windows_script("session", {"launcher": record(ref)})
            ref_code, ref_value = run_windows_reference(ref, script, redirect_userprofile=False)
            code, value = run_agent(binary, {"op": "session", "host": "windows", "home": str(agent), "nowMs": NOW_MS, "launcher": record(agent)})
            if ref_code != 0 or code != 0 or ref_value != value or value != {"signedIn": expect}:
                fail(f"windows {name}: reference {ref_code} {ref_value}, agent {code} {value}")
            COMPARED += 1

        session_case("session-absent", None, False)
        session_case("session-signed-in", json.dumps({"lastKnownAccountUuid": UUID, "oauth:tokenCacheV2": CACHE}).encode(), True)
        session_case("session-no-cache", json.dumps({"lastKnownAccountUuid": UUID}).encode(), False)
        session_case("session-bad-json", b"{oops", False)

        # State: no claude.exe carries the fixture profile's needle (read-only check).
        script = windows_script("state", {"launcher": record(ref)})
        ref_code, ref_value = run_windows_reference(ref, script, redirect_userprofile=False)
        code, value = run_agent(binary, {"op": "state", "host": "windows", "home": str(agent), "nowMs": NOW_MS, "launcher": record(agent)})
        if ref_code != 0 or code != 0 or ref_value != value:
            fail(f"windows state: reference {ref_code} {ref_value}, agent {code} {value}")
        COMPARED += 1

        # Purge: fixture trash trees, permit gate, byte-identical removal.
        for home in (ref, agent):
            trash = home / ".ccs" / "trash" / "claude" / TRASH_NAME
            trash.mkdir(parents=True)
            (trash / "Local State").write_bytes(b"signed in")
            (trash / "sub").mkdir()
            (trash / "sub" / "deep").write_bytes(b"deep")
        script = windows_script("purge", {"trashName": TRASH_NAME})
        ref_code, ref_value = run_windows_reference(ref, script)
        code, cause = run_agent(binary, {"op": "purge", "host": "windows", "home": str(agent), "nowMs": NOW_MS, "trashName": TRASH_NAME})
        if code != 1 or cause != "confirmation_invalid":
            fail(f"windows purge without permit: {code} {cause}")
        code, value = run_agent(binary, {"op": "purge", "host": "windows", "home": str(agent), "nowMs": NOW_MS, "trashName": TRASH_NAME, "token": purge_permit("windows", TRASH_NAME)})
        if ref_code != 0 or code != 0 or ref_value != value or value != {"ok": True}:
            fail(f"windows purge: reference {ref_code} {ref_value}, agent {code} {value}")
        if snapshot(ref) != snapshot(agent):
            fail("windows purge trees differ")
        COMPARED += 1

        # Purge of a missing folder is ok on both sides.
        ref_code, ref_value = run_windows_reference(ref, script)
        code, value = run_agent(binary, {"op": "purge", "host": "windows", "home": str(agent), "nowMs": NOW_MS, "trashName": TRASH_NAME, "token": purge_permit("windows", TRASH_NAME)})
        if ref_code != 0 or code != 0 or ref_value != value:
            fail(f"windows purge-missing: reference {ref_code} {ref_value}, agent {code} {value}")
        COMPARED += 1

        # The shell-folder steps are refused in fixture mode before any spawn.
        for op in ("create", "undo", "trash", "restore", "open"):
            code, cause = run_agent(binary, {"op": op, "host": "windows", "home": str(agent), "nowMs": NOW_MS, "profileId": "party", "trashName": TRASH_NAME, "launcher": record(agent)})
            if code != 1:
                fail(f"windows fixture {op} must refuse, got {code}")
            COMPARED += 1
    finally:
        shutil.rmtree(roots, ignore_errors=True)


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    if not pathlib.Path(binary).is_file():
        fail(f"agent binary not found: {binary}")
    mac_parity(binary)
    windows_parity(binary)
    print(f"claude-host parity: {COMPARED} exact comparisons on fixture homes")


if __name__ == "__main__":
    main()
