"""Replay the reconstructed historical files in disposable Linux sandboxes.

Run from the checkout. The Linux build and dependencies must already exist.
A file that never reached the public listener keeps its original test result and
is reported as unported. This never treats a TS pass as a Rust pass.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path.cwd()
MANIFEST = ROOT / "tests/spec/m6-historical-gates.json"
BOUNDED_TIMEOUT = 120
RUST_REQUEST = '"message":"request.completed"'
RESET_EXECUTABLES = {
    "reset-any-usage-native.test.ts": "any-usage-codex",
    "reset-capture-native.test.ts": "codex",
    "reset-refusal-native.test.ts": "refusal-codex",
    "reset-source-failure-native.test.ts": "codex",
}


def gate_files():
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    files = {
        name: [gate for gate in ("safety", "http") if name in manifest[gate]]
        for name in sorted(set(manifest["safety"]) | set(manifest["http"]))
    }
    retired = manifest.get("notApplicableAfterCut", {})
    if set(retired) - files.keys():
        raise ValueError("retired cases must belong to the reconstructed gate")
    return files, retired


def command_for(file):
    if file.suffix == ".py":
        return ["/usr/bin/python3", "-m", "unittest", str(file)]
    if file.suffix in (".js", ".mjs") and "node:test" in file.read_text(encoding="utf-8"):
        return ["/usr/local/bin/node", "--test", str(file)]
    if file.suffix in (".ts", ".js", ".mjs"):
        return ["/usr/local/bin/bun", "test", "--timeout", "20000", "--preload", "./tests/spec/compose-preload.ts", str(file)]
    return None


def sandbox(tmp, command, binaries, dependencies, reset_executable):
    # The repo and dependencies are read-only. The real home, host sockets and share
    # are masked. The private network namespace has loopback and no outside interfaces.
    cmd = [
        "bwrap", "--unshare-net", "--unshare-pid", "--die-with-parent",
        "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc",
        "--tmpfs", "/home", "--tmpfs", "/mnt", "--tmpfs", "/run", "--tmpfs", "/tmp",
        "--tmpfs", "/usr/local/bin",
        "--ro-bind", str(ROOT), str(ROOT),
        "--ro-bind", str(dependencies), str(dependencies),
        "--bind", tmp, "/tmp/aac-fixture", "--chdir", str(ROOT), "--clearenv",
    ]
    for name, binary in binaries.items():
        cmd += ["--ro-bind", str(binary), "/usr/local/bin/" + name]
    env = dict.fromkeys((
        "HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR",
        "CODEX_HOME", "CLAUDE_CONFIG_DIR", "CCS_HOME",
    ), "/tmp/aac-fixture")
    env.update({
        "PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8",
        "AAC_RUST_PUBLIC_BINARY": str(ROOT / "dist/native/aac-http"),
        "AAC_RUST_SERVICE_BINARY": str(ROOT / "dist/native/aac-http"),
        "AAC_STATUS_MODULE": str(ROOT / "dist/native/aac-status.linux-x64.node"),
        "AAC_STORE_MODULE": str(ROOT / "dist/native/aac-store.linux-x64.node"),
        "AAC_RUST_COMPOSE": "1", "AAC_RUST_SERVICE": "on", "AAC_M6_FIXTURE_REPORT": "1", "AAC_NO_LOCAL_RUST_BUILD": "1",
        "AAC_AGENT_VERBS": "off", "AAC_AGENT_TRANSPORT": "off", "AAC_STATUS_DOORBELL": "off",
        "AAC_AGENT_REPORTS_DIR": "/tmp/aac-fixture/reports", "AAC_PM_MAIL_REGISTRY_DIR": "/tmp/aac-fixture/registry",
        "AAC_STORE_SHARE_BACKUP_DIR": "off",
    })
    if reset_executable:
        fixture_bin = Path(tmp) / "bin"
        fixture_bin.mkdir()
        codex = fixture_bin / "codex"
        shutil.copyfile(ROOT / "tests/fixtures/reset-source" / reset_executable, codex)
        codex.chmod(0o700)
        env["AAC_RESET_TEST_MODULE"] = env["AAC_STATUS_MODULE"]
        env["AAC_RESET_TEST_CODEX_DIR"] = "/tmp/aac-fixture/bin"
        env["PATH"] = env["AAC_RESET_TEST_CODEX_DIR"] + ":" + env["PATH"]
    for key, value in env.items():
        cmd += ["--setenv", key, value]
    return cmd + ["--", *command]


def run_file(name, gates, binaries, dependencies, logs, retired_reason):
    file = Path(name)
    result = {"file": name, "gates": gates}
    if not file.is_file():
        if retired_reason:
            return {**result, "state": "not applicable after the cut", "reason": retired_reason}
        return {**result, "state": "historical file absent"}
    if "/browser/checks/" in name or name.endswith("status-batch-identity-worker.ts"):
        reason = "browser kit" if "/browser/checks/" in name else "invoked by status-server-review"
        return {**result, "state": "separate fixture", "reason": reason}
    command = command_for(file)
    if not command:
        return {**result, "state": "separate fixture", "reason": "native Cargo test runner"}
    with tempfile.TemporaryDirectory(prefix="aac-m6-gate-") as tmp:
        try:
            run = subprocess.run(
                sandbox(tmp, command, binaries, dependencies, RESET_EXECUTABLES.get(file.name)),
                cwd=ROOT,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=BOUNDED_TIMEOUT,
                check=False,
            )
            code, output = run.returncode, run.stdout
        except subprocess.TimeoutExpired as error:
            code, output = 124, error.stdout or b""
    text = output.decode("utf-8", errors="replace")
    name_on_disk = name.replace("/", "__").replace("\\", "__")
    (logs / (name_on_disk + ".log")).write_text(text, encoding="utf-8")
    tests = {
        word: int(count)
        for count, word in re.findall(r"(?m)^\s*(\d+) (pass|fail)\b", text)
    }
    for word, count in re.findall(r"(?m)^[#ℹ] (pass|fail) (\d+)\s*$", text):
        tests[word] = int(count)
    unittest = re.search(r"(?m)^Ran (\d+) tests? in ", text)
    if unittest and code == 0:
        tests = {"pass": int(unittest[1]), "fail": 0}
    result.update({
        "state": "ran", "exitCode": code,
        "rustRequests": text.count(RUST_REQUEST),
        "rustFixtures": text.count("[m6 compose fixture]"),
        "passed": tests.get("pass"),
        "failed": tests.get("fail"),
    })
    if text.startswith("bwrap:"):
        result["frameworkError"] = text.splitlines()[0]
    if code == 124:
        result["frameworkError"] = f"file exceeded {BOUNDED_TIMEOUT} seconds"
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--filter", default="", help="Run matching historical paths only")
    args = parser.parse_args()
    output = args.output.resolve()
    if output.is_relative_to(ROOT):
        parser.error("use a scratch output directory outside the checkout")
    if not shutil.which("bwrap") or not (ROOT / "dist/native/aac-http").is_file():
        parser.error("bubblewrap and the staged Linux Rust front door are required")
    binaries = {name: Path(shutil.which(name)).resolve() for name in ("node", "bun")}
    dependencies = (ROOT / "node_modules").resolve(strict=True)
    output.mkdir(parents=True, exist_ok=True)
    files, retired = gate_files()
    items = [(name, gates) for name, gates in files.items() if args.filter in name]
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(run_file, name, gates, binaries, dependencies, output, retired.get(name))
                   for name, gates in items]
        results = []
        for future in futures:
            row = future.result()
            results.append(row)
            print(json.dumps(row), flush=True)
    (output / "results.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
