#!/usr/bin/env python3
"""claude_signin: keep every Claude Code sign-in alive on every computer AI Account Center covers.

Every Claude Code OAuth sign-in has a fixed end (claudeAiOauth.refreshTokenExpiresAt), about 30 days
after it was made. Refreshing the access token (every ~8 h) does not move it. Once it passes, the next
access-token refresh fails, Claude Code blanks the tokens and T3 marks that provider "not
authenticated". This runner renews each sign-in a few days before its end, with no person involved:

  * it reads each computer's sign-ins (times and account emails only) locally or over ssh;
  * for each sign-in that is due it runs `claude auth login --claudeai` on that computer, approves
    the request in a Chrome profile kept signed in to claude.ai for that account (browser_auth.mjs),
    types the one-use code into the login prompt and checks that the saved end moved later;
  * it checks again ~9 h later (and after the old end has passed), because a Claude process that was
    already running can write its old sign-in back, and renews again if it did;
  * when a Chrome profile has been signed out it asks claude.ai for a sign-in link, reads it from
    the mail (Mail.app on the Mac, or IMAP) and opens it; claude.ai then shows a CAPTCHA, which only
    a person may solve, so it brings that window forward, notifies, publishes "needs you" for the
    dashboard and every agent, and carries on by itself once the profile is signed in again.

  claude_signin.py status [--json] [--local]        every sign-in and when it ends
  claude_signin.py tick                             the scheduled run (status, renew due, verify, publish)
  claude_signin.py renew [--host H] [--profile P] [--account A] [--force] [--dry-run]
  claude_signin.py install | uninstall              schedule tick on this computer (launchd/systemd/Task Scheduler)
  claude_signin.py doctor                           check config, ssh, Chrome, Node, mail and the schedule

Private settings live in <CCS dir>/claude-signin/config.json (see README.md). Token values, codes and
sign-in links are never printed, logged or published: only times, emails and fixed state words.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

VERSION = "1.0.0"
SCHEMA = 1
HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.abspath(__file__)
LABEL = "party.sittingmongoose.aac.claude-signin"
ACCESS_LIFETIME_MS = 8 * 3600 * 1000
HOUR_MS = 3600 * 1000
PUBLISH_JSON = "claude-signins.json"
PUBLISH_MD = "claude-signins.md"

DEFAULTS = {
    # Computers covered. Each: {"name", "os": mac|windows|linux, "ssh": "<user@host or alias>",
    # "hostnames": [names this computer reports, so the runner knows which one it is]}.
    # Without a list, only this computer is covered.
    "hosts": [],
    # Profile folder names under ~/.claude-t3 to leave alone (retired accounts).
    "ignoreProfiles": [],
    # "host/profile" pairs never renewed automatically (still tracked).
    "skip": [],
    # Renew a sign-in once its end is this close.
    "renewWithinHours": 96,
    # Re-check a renewal this long after it (a running Claude can write its old sign-in back).
    "verifyAfterHours": 9,
    # Write claude-signins.json / .md here (the AI Account Center usage snapshot folder).
    "publishDir": None,
    # When this computer may not write that folder itself (macOS keeps scheduled jobs off network
    # volumes), publish through another computer that mounts it: {"host": "<name>", "dir": "<path>"}.
    "publishVia": None,
    # How long the runner waits for a person to solve a CAPTCHA before carrying on.
    "humanWaitMinutes": 20,
    # Mail used to read claude.ai sign-in links: {"kind": "mailapp"} (Mac), or
    # {"kind": "imap", "host", "port", "user", "passwordFile", "folder"}; one entry per account
    # email under "accounts", or one "default".
    "mail": {"default": {"kind": "mailapp"}},
    # Chrome profiles, one per account email.
    "chromeProfilesDir": None,
    # Notify this computer's desktop when a person is needed.
    "notify": True,
    # Absolute paths when the schedule's PATH does not find them.
    "nodePath": None,
    "claudePaths": {},
}


# ---------------------------------------------------------------- paths, config, state, log

def ccs_dir():
    if os.environ.get("CCS_DIR"):
        return os.path.abspath(os.environ["CCS_DIR"])
    if os.environ.get("CCS_HOME"):
        return os.path.join(os.path.abspath(os.environ["CCS_HOME"]), ".ccs")
    return os.path.join(os.path.expanduser("~"), ".ccs")


def state_dir():
    return os.path.join(ccs_dir(), "claude-signin")


def private_mkdir(path):
    os.makedirs(path, exist_ok=True)
    if os.name != "nt":
        try:
            os.chmod(path, 0o700)
        except OSError:
            pass


def write_atomic(path, text, mode=0o600):
    folder = os.path.dirname(path)
    fd, tmp = tempfile.mkstemp(prefix=".tmp-", dir=folder)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)
        if os.name != "nt":
            os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def read_json(path, fallback):
    try:
        with open(path, encoding="utf-8") as handle:
            value = json.load(handle)
        return value if isinstance(value, type(fallback)) else fallback
    except (OSError, ValueError):
        return fallback


def load_config():
    cfg = json.loads(json.dumps(DEFAULTS))
    user = read_json(os.path.join(state_dir(), "config.json"), {})
    cfg.update({k: v for k, v in user.items() if k in DEFAULTS})
    if not cfg.get("chromeProfilesDir"):
        cfg["chromeProfilesDir"] = os.path.join(state_dir(), "chrome")
    return cfg


def load_state():
    st = read_json(os.path.join(state_dir(), "state.json"), {})
    st.setdefault("version", 1)
    st.setdefault("profiles", {})
    st.setdefault("accounts", {})
    st.setdefault("events", [])
    return st


def save_state(st):
    private_mkdir(state_dir())
    st["events"] = st.get("events", [])[-200:]
    write_atomic(os.path.join(state_dir(), "state.json"), json.dumps(st, indent=1, sort_keys=True) + "\n")


SECRETISH = re.compile(r"https?://\S+|[A-Za-z0-9_-]{20,}#[A-Za-z0-9_-]{20,}|sk-ant-\S+|[A-Za-z0-9_-]{40,}")


def scrub(text):
    """Removes anything that could be a link, code or token before text reaches a log or a file."""
    return SECRETISH.sub("<redacted>", str(text))


def log(message):
    line = "%s %s" % (dt.datetime.now().strftime("%Y-%m-%d %H:%M:%S"), scrub(message))
    print(line, flush=True)
    try:
        private_mkdir(state_dir())
        path = os.path.join(state_dir(), "runner.log")
        if os.path.exists(path) and os.path.getsize(path) > 1024 * 1024:
            os.replace(path, path + ".1")
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")
    except OSError:
        pass


def event(st, kind, **fields):
    st.setdefault("events", []).append(dict({"at": now_ms(), "kind": kind}, **{k: scrub(v) if isinstance(v, str) else v for k, v in fields.items()}))


def now_ms():
    return int(time.time() * 1000)


def iso(ms):
    if not ms:
        return None
    return dt.datetime.fromtimestamp(ms / 1000, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def human_time(ms):
    if not ms:
        return "-"
    t = dt.datetime.fromtimestamp(ms / 1000)
    delta = ms / 1000 - time.time()
    d = abs(delta)
    rel = ("%dd %dh" % (d // 86400, d % 86400 // 3600)) if d >= 86400 else ("%dh %02dm" % (d // 3600, d % 3600 // 60))
    return "%s (%s)" % (t.strftime("%a %b %d %I:%M %p").replace(" 0", " "), ("in " + rel) if delta >= 0 else (rel + " ago"))


class Lock:
    """One runner at a time on this computer (tick, renew and the schedule share it)."""

    def __init__(self):
        private_mkdir(state_dir())
        self.path = os.path.join(state_dir(), "runner.lock")
        self.handle = None

    def __enter__(self):
        self.handle = open(self.path, "a+")
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(self.handle.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(self.handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            self.handle.close()
            raise SystemExit("another claude_signin run is in progress on this computer")
        return self

    def __exit__(self, *exc):
        try:
            self.handle.close()
        except OSError:
            pass


# ---------------------------------------------------------------- local collection (runs on each computer)

def find_claude(cfg=None, host=None):
    if cfg and host and (cfg.get("claudePaths") or {}).get(host):
        return cfg["claudePaths"][host]
    home = os.path.expanduser("~")
    for c in [shutil.which("claude"), os.path.join(home, ".local", "bin", "claude.exe"),
              os.path.join(home, ".local", "bin", "claude"), "/opt/homebrew/bin/claude", "/usr/local/bin/claude",
              os.path.join(home, ".claude", "local", "claude")]:
        if c and os.path.isfile(c):
            return c
    return None


def keychain_service(config_dir, is_default):
    if is_default:
        return "Claude Code-credentials"
    return "Claude Code-credentials-" + hashlib.sha256(config_dir.encode()).hexdigest()[:8]


def mac_keychain_read(service):
    """The Claude Code Keychain item; readable from a process in the Mac's desktop session only."""
    try:
        r = subprocess.run(["security", "find-generic-password", "-s", service, "-w"],
                           capture_output=True, text=True, timeout=25)
    except subprocess.TimeoutExpired:
        return None, "keychain_prompt"
    if r.returncode != 0:
        err = (r.stderr or "").strip()
        if "could not be found" in err:
            return None, "no_keychain_item"
        if os.environ.get("SSH_CONNECTION") or "interaction" in err.lower():
            return None, "keychain_needs_desktop_session"
        return None, "keychain_unreadable"
    try:
        return json.loads(r.stdout), None
    except ValueError:
        return None, "keychain_not_json"


def read_creds(config_dir, is_default):
    if platform.system() == "Darwin":
        data, err = mac_keychain_read(keychain_service(config_dir, is_default))
        if data is None and err == "no_keychain_item":
            p = os.path.join(config_dir, ".credentials.json")
            if os.path.exists(p):
                return read_json(p, {}) or None, None if read_json(p, {}) else "credentials_not_json"
        return data, err
    p = os.path.join(config_dir, ".credentials.json")
    if not os.path.exists(p):
        return None, "no_credentials_file"
    data = read_json(p, {})
    return (data, None) if data else (None, "credentials_not_json")


def account_email(config_dir, is_default):
    home = os.path.expanduser("~")
    paths = [os.path.join(home, ".claude.json"), os.path.join(config_dir, ".claude.json")] if is_default \
        else [os.path.join(config_dir, ".claude.json")]
    for p in paths:
        value = (read_json(p, {}).get("oauthAccount") or {}).get("emailAddress")
        if isinstance(value, str) and "@" in value and len(value) < 200:
            return value.strip().lower()
    return None


def epoch(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    value = int(value)
    return value if 10 ** 12 <= value <= 10 ** 13 else None


def logout_estimate(access_ms, end_ms):
    """A running Claude refreshes the access token about every 8 h until the sign-in end; the last
    token it gets lasts up to 8 h past that end. Unknown without both times."""
    if not access_ms or not end_ms:
        return None
    t = access_ms
    while t < end_ms:
        t += ACCESS_LIFETIME_MS
    return t


def collect_local(ignore, only_dir=None):
    home = os.path.expanduser("~")
    profiles = []
    base = os.path.join(home, ".claude-t3")
    if only_dir:
        profiles.append(("_test", only_dir, False))
    elif os.path.isdir(base):
        for name in sorted(os.listdir(base)):
            d = os.path.join(base, name)
            if os.path.isdir(d) and name not in ignore and not name.startswith(".") \
                    and os.path.exists(os.path.join(d, ".claude.json")):
                profiles.append((name, d, False))
    d = os.path.join(home, ".claude")
    if os.path.isdir(d) and not only_dir:
        profiles.append(("default", d, True))
    now = now_ms()
    rows = []
    for name, d, is_default in profiles:
        creds, err = read_creds(d, is_default)
        row = {"profile": name, "dir": d, "default": is_default, "email": account_email(d, is_default),
               "accessExpiresAt": None, "signInEndsAt": None, "logoutAt": None, "state": None, "note": err}
        oauth = (creds or {}).get("claudeAiOauth") if isinstance(creds, dict) else None
        if not isinstance(oauth, dict):
            if is_default and err in ("no_credentials_file", "no_keychain_item"):
                continue  # a default folder that was never signed in: nothing to keep alive
            row["state"] = "unreadable" if err and err.startswith("keychain") and err != "no_keychain_item" else "not_signed_in"
        elif not oauth.get("accessToken") and not oauth.get("refreshToken"):
            row["state"] = "logged_out"
        else:
            access, end = epoch(oauth.get("expiresAt")), epoch(oauth.get("refreshTokenExpiresAt"))
            row["accessExpiresAt"], row["signInEndsAt"] = access, end
            if not oauth.get("refreshToken"):
                row["logoutAt"] = access
                row["state"] = "no_refresh_token" if (access or 0) > now else "logged_out"
            elif end:
                row["logoutAt"] = logout_estimate(access, end) or end
                row["state"] = "ok" if row["logoutAt"] > now else "logged_out"
            else:
                row["state"] = "no_end_date"
        rows.append(row)
    return {"hostname": socket.gethostname().split(".")[0].lower(), "os": platform.system().lower(),
            "claude": find_claude(), "profiles": rows, "now": now, "version": VERSION}


# ---------------------------------------------------------------- computers

def this_host_name(cfg):
    me = socket.gethostname().split(".")[0].lower()
    for h in cfg["hosts"]:
        if me in [x.lower() for x in h.get("hostnames", [])]:
            return h["name"]
    return None


def host_os():
    s = platform.system()
    return "mac" if s == "Darwin" else "windows" if s == "Windows" else "linux"


def hosts(cfg):
    """The configured computers, with this one marked local; without a list, this computer only."""
    me = this_host_name(cfg)
    out = []
    for h in cfg["hosts"]:
        h = dict(h)
        h["local"] = h["name"] == me
        out.append(h)
    if not out:
        out.append({"name": socket.gethostname().split(".")[0].lower(), "os": host_os(), "local": True})
    return out


def ssh_args(host, tty=False):
    args = ["ssh", "-o", "ConnectTimeout=8", "-o", "ServerAliveInterval=15"]
    args += ["-tt"] if tty else ["-T", "-o", "BatchMode=yes"]
    return args + [host["ssh"]]


def collect(host, cfg, only_dir=None):
    ignore = cfg.get("ignoreProfiles") or []
    if host.get("local"):
        return collect_local(ignore, only_dir)
    with open(SCRIPT, "rb") as f:
        src = f.read()
    py = "python" if host["os"] == "windows" else "python3"
    extra = (" --dir \"%s\"" % only_dir) if only_dir else ""
    cmd = ssh_args(host) + ["%s - _local-status --ignore %s%s" % (py, ",".join(ignore) or "-", extra)]
    try:
        r = subprocess.run(cmd, input=src, capture_output=True, timeout=90)
    except subprocess.TimeoutExpired:
        return {"error": "ssh_timeout"}
    if r.returncode != 0:
        msg = (r.stderr.decode("utf-8", "replace").strip().splitlines() or ["exit %d" % r.returncode])[-1]
        return {"error": scrub(msg)[:160]}
    try:
        return json.loads(r.stdout.decode("utf-8", "replace").strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"error": "bad_reply"}


def key(host, profile):
    return "%s/%s" % (host["name"] if isinstance(host, dict) else host, profile)


# ---------------------------------------------------------------- decisions

def due_reason(row, cfg, pst, now):
    """Why this sign-in should be renewed now, or None."""
    st = row["state"]
    if st in ("unreadable", "not_signed_in", "no_end_date"):
        return None
    if not row.get("email"):
        return None
    if pst.get("revertedAt") and pst.get("revertedAt", 0) > pst.get("lastRenewAt", 0):
        return "reverted"
    if st in ("logged_out", "no_refresh_token"):
        return "logged_out"
    end = row.get("signInEndsAt") or 0
    if end and end - now < cfg["renewWithinHours"] * HOUR_MS:
        return "ends_soon"
    return None


def backoff_ok(pst, now):
    return now >= (pst.get("retryAfter") or 0)


def note_failure(pst, now, reason):
    pst["failures"] = int(pst.get("failures") or 0) + 1
    pst["lastResult"] = reason
    pst["lastAttemptAt"] = now
    pst["retryAfter"] = now + min(6 * HOUR_MS, HOUR_MS * (2 ** (pst["failures"] - 1)))


# ---------------------------------------------------------------- the login prompt

ANSI = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[()][A-Za-z0-9]|\x1b[=>]")
URL_RE = re.compile(r"https://claude\.(?:com|ai)/\S*oauth/authorize\?\S+?(?=[\s\x00]|$)")


def login_command(host, row, cfg):
    """argv that runs `claude auth login --claudeai` for this profile, here or over ssh."""
    claude = find_claude(cfg, host["name"]) if host.get("local") else (cfg.get("claudePaths") or {}).get(host["name"]) or row.get("_claude")
    if not claude:
        raise RuntimeError("claude_not_found")
    if host["os"] == "windows":
        # No space before &&: cmd keeps it in the value and Claude saves to a look-alike "<dir> " folder.
        env = "set CLAUDE_CONFIG_DIR=" + ("" if row["default"] else row["dir"])
        line = '%s&& set BROWSER=none&& "%s" auth login --claudeai' % (env, claude)
        return ["cmd", "/c", line] if host.get("local") else ssh_args(host, tty=True) + [line]
    env = ("env -u CLAUDE_CONFIG_DIR" if row["default"] else "env CLAUDE_CONFIG_DIR=%s" % shq(row["dir"])) + " BROWSER=true"
    line = "%s %s auth login --claudeai" % (env, shq(claude))
    return ["sh", "-c", line] if host.get("local") else ssh_args(host, tty=True) + [line]


def shq(s):
    return "'" + s.replace("'", "'\"'\"'") + "'"


class Prompt:
    """`claude auth login` under a terminal (a pty here, ssh -tt for another computer). Its output is
    kept in memory only, to find the link and the result; it is never printed or logged."""

    def __init__(self, argv):
        self.buf = ""
        self.proc = None
        if os.name == "nt":
            self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            self.fd = None
        else:
            import pty
            self.pid, self.fd = pty.fork()
            if self.pid == 0:
                try:
                    os.execvp(argv[0], argv)
                finally:
                    os._exit(127)

    def read(self, timeout):
        """Reads what arrived within timeout seconds; returns False once the program has ended."""
        if self.fd is None:
            import threading
            out = []
            t = threading.Thread(target=lambda: out.append(self.proc.stdout.read1(4096) if hasattr(self.proc.stdout, "read1") else self.proc.stdout.read(1)))
            t.daemon = True
            t.start()
            t.join(timeout)
            if out:
                if not out[0]:
                    return False
                self.buf = (self.buf + out[0].decode("utf-8", "replace"))[-32768:]
            return True
        import select
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return True
        try:
            data = os.read(self.fd, 4096)
        except OSError:
            return False
        if not data:
            return False
        self.buf = (self.buf + data.decode("utf-8", "replace"))[-32768:]
        return True

    def text(self):
        return ANSI.sub("", self.buf).replace("\r", "\n")

    def url(self):
        m = URL_RE.search(self.text())
        return m.group(0) if m else None

    def write(self, data):
        if self.fd is None:
            self.proc.stdin.write(data)
            self.proc.stdin.flush()
        else:
            os.write(self.fd, data)

    def wait(self, timeout):
        end = time.time() + timeout
        while time.time() < end:
            if not self.read(1.0):
                break
        return self.stop()

    def stop(self):
        if self.fd is None:
            if self.proc.poll() is None:
                self.proc.kill()
            return self.proc.wait()
        try:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid == 0:
                os.kill(self.pid, signal.SIGKILL)
                pid, status = os.waitpid(self.pid, 0)
        except ChildProcessError:
            return 0
        finally:
            try:
                os.close(self.fd)
            except OSError:
                pass
        return os.waitstatus_to_exitcode(status) if hasattr(os, "waitstatus_to_exitcode") else (status >> 8)


# ---------------------------------------------------------------- browser and mail

def node_path(cfg):
    return cfg.get("nodePath") or shutil.which("node") or next(
        (p for p in ("/opt/homebrew/bin/node", "/usr/local/bin/node", os.path.expanduser("~/.local/bin/node")) if os.path.isfile(p)), None)


def browser(cfg, command, email, stdin_text=None, on_event=None, timeout=1800, extra=None):
    """Runs one browser_auth.mjs command; returns its result dict. Secrets stay in the dict."""
    node = node_path(cfg)
    if not node:
        return {"result": "error", "reason": "no_node"}
    argv = [node, os.path.join(HERE, "browser_auth.mjs"), command, email,
            "--profiles", cfg["chromeProfilesDir"], "--wait-minutes", str(cfg["humanWaitMinutes"])] + (extra or [])
    proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        if stdin_text is not None:
            proc.stdin.write((stdin_text + "\n").encode())
        proc.stdin.close()
    except OSError:
        pass
    result = {"result": "error", "reason": "no_result"}
    end = time.time() + timeout
    for raw in proc.stdout:
        try:
            item = json.loads(raw.decode("utf-8", "replace"))
        except ValueError:
            continue
        if "event" in item and on_event:
            on_event(item["event"])
        if "result" in item:
            result = dict(item.pop("extra", None) or {}, **item) if isinstance(item.get("extra"), dict) else item
        if time.time() > end:
            proc.kill()
            break
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
    for line in proc.stderr.read().decode("utf-8", "replace").splitlines()[-5:]:
        log("  browser: " + line.replace("[browser] ", ""))
    return result


def mail_settings(cfg, email):
    mail = cfg.get("mail") or {}
    return mail.get(email) or mail.get("default") or {"kind": "mailapp"}


MAGIC_RE = re.compile(r"https://claude\.ai/magic-link#[^\"'\s<>]+")


def link_from_source(src):
    flat = src.replace("=\r\n", "").replace("=\n", "").replace("=3D", "=").replace("=3d", "=")
    m = MAGIC_RE.search(flat)
    return m.group(0).replace("&amp;", "&") if m else None


MAILAPP_SCRIPT = r'''
on run argv
  set toAddr to item 1 of argv
  set cutoff to (date "Thursday, January 1, 1970 at 12:00:00 AM") + ((item 2 of argv) as integer) + (time to GMT)
  tell application "Mail"
    check for new mail
    set best to missing value
    repeat with m in (messages of inbox whose date received > cutoff and sender contains "anthropic")
      if (address of to recipients of m as string) contains toAddr then
        if best is missing value then
          set best to m
        else if date received of m > date received of best then
          set best to m
        end if
      end if
    end repeat
    if best is missing value then return ""
    return source of best
  end tell
end run'''


def mail_link_mailapp(email, since):
    try:
        r = subprocess.run(["osascript", "-", email, str(int(since))], input=MAILAPP_SCRIPT, capture_output=True,
                           text=True, timeout=90)
    except (OSError, subprocess.TimeoutExpired):
        return None, "mail_timeout"
    if r.returncode != 0:
        err = r.stderr or ""
        return None, "mail_not_allowed" if ("-1743" in err or "not allowed" in err.lower()) else "mail_failed"
    return link_from_source(r.stdout), None


def mail_link_imap(settings, email, since):
    import imaplib
    import email as emaillib
    try:
        with open(os.path.expanduser(settings["passwordFile"]), encoding="utf-8") as f:
            password = f.read().strip()
        box = imaplib.IMAP4_SSL(settings["host"], int(settings.get("port", 993)))
        box.login(settings.get("user") or email, password)
        box.select(settings.get("folder", "INBOX"), readonly=True)
        day = dt.datetime.fromtimestamp(since - 86400).strftime("%d-%b-%Y")
        typ, data = box.search(None, '(SINCE %s FROM "anthropic" TO "%s")' % (day, email))
        best = None
        for num in reversed((data[0] or b"").split()[-20:]):
            typ, parts = box.fetch(num, "(RFC822)")
            msg = emaillib.message_from_bytes(parts[0][1])
            when = emaillib.utils.parsedate_to_datetime(msg.get("Date")).timestamp() if msg.get("Date") else 0
            if when < since:
                continue
            for part in msg.walk():
                if part.get_content_type() in ("text/html", "text/plain"):
                    link = link_from_source(part.get_payload(decode=True).decode("utf-8", "replace"))
                    if link and (best is None or when > best[0]):
                        best = (when, link)
        box.logout()
        return (best[1] if best else None), None
    except (OSError, KeyError, imaplib.IMAP4.error, ValueError, TypeError) as e:
        return None, "imap_failed: %s" % type(e).__name__


def mail_link(cfg, email, since):
    settings = mail_settings(cfg, email)
    if settings.get("kind") == "imap":
        return mail_link_imap(settings, email, since)
    if host_os() != "mac":
        return None, "no_mail_configured"
    return mail_link_mailapp(email, since)


def notify(cfg, title, body):
    if not cfg.get("notify"):
        return
    try:
        if host_os() == "mac":
            script = 'display notification %s with title %s sound name "Glass"' % (json.dumps(body), json.dumps(title))
            subprocess.run(["osascript", "-e", script], timeout=15, capture_output=True)
        elif host_os() == "windows":
            subprocess.run(["msg", "*", "/TIME:3600", "%s: %s" % (title, body)], timeout=15, capture_output=True)
        elif shutil.which("notify-send") and (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")):
            subprocess.run(["notify-send", title, body], timeout=15, capture_output=True)
    except (OSError, subprocess.TimeoutExpired):
        pass


# ---------------------------------------------------------------- account browser session

def need_person(st, email, reason, how):
    acc = st["accounts"].setdefault(email, {})
    if acc.get("needs") != reason:
        acc["needsSince"] = now_ms()
    acc["needs"], acc["needsHow"] = reason, how


def clear_need(st, email):
    acc = st["accounts"].setdefault(email, {})
    for k in ("needs", "needsHow", "needsSince"):
        acc.pop(k, None)


def captcha_hook(cfg, st, email):
    """The browser's "a person must pass this check" event: notify once, publish, and remember it."""
    told = {"done": False}

    def on_event(name):
        if name == "captcha" and not told["done"]:
            told["done"] = True
            where = this_host_name(cfg) or socket.gethostname().split(".")[0]
            need_person(st, email, "captcha", "Solve the CAPTCHA in the Chrome window for %s on the %s; renewal carries on by itself after." % (email, where))
            save_state(st)
            publish(cfg, st)
            notify(cfg, "Claude sign-in needs you", "Solve the CAPTCHA in the Chrome window for %s. Everything else renews by itself." % email)
            log("%s: a CAPTCHA is on screen; notified and waiting up to %d min" % (email, cfg["humanWaitMinutes"]))
        elif name == "captcha_cleared" and told["done"]:
            clear_need(st, email)
            log("%s: the CAPTCHA was passed" % email)
    return on_event, told


def sign_in_browser(cfg, st, email):
    """The account's Chrome profile is signed out: email link, then wait for a person at the CAPTCHA.
    Returns True when the profile is signed in again."""
    acc = st["accounts"].setdefault(email, {})
    now = now_ms()
    # One link per account per 50 minutes: a person may still be finishing the last one.
    if acc.get("linkRequestedAt") and now - acc["linkRequestedAt"] < 50 * 60 * 1000:
        log("%s: a sign-in link went out %d min ago; waiting for it to be finished" % (email, (now - acc["linkRequestedAt"]) // 60000))
        return False
    log("%s: its Chrome profile is signed out of claude.ai; asking for a sign-in link" % email)
    since = time.time() - 5
    on_captcha, told = captcha_hook(cfg, st, email)

    res = browser(cfg, "request-link", email, on_event=on_captcha)
    acc["linkRequestedAt"] = now
    if res.get("result") == "signed_in":
        return True
    if res.get("result") == "captcha_timeout":
        return False
    if res.get("result") != "sent":
        need_person(st, email, "browser_signin_failed", "The %s Chrome profile could not start a claude.ai sign-in (%s). Sign in to claude.ai in that window." % (email, res.get("reason")))
        return False
    link, err = None, None
    for _ in range(18):
        time.sleep(10)
        link, err = mail_link(cfg, email, since)
        if link or err in ("mail_not_allowed", "no_mail_configured"):
            break
    if not link:
        how = {"mail_not_allowed": "Allow the runner to control Mail (System Settings > Privacy & Security > Automation), or sign in to claude.ai in the %s Chrome window." % email,
               "no_mail_configured": "No mail is set up for %s; sign in to claude.ai in its Chrome window, or add IMAP settings." % email}.get(err, "No claude.ai sign-in email for %s arrived within 3 minutes (%s). Sign in to claude.ai in its Chrome window." % (email, err or "none found"))
        need_person(st, email, "mail_link_missing", how)
        notify(cfg, "Claude sign-in needs you", how)
        return False
    log("%s: opening the sign-in link from the mail" % email)
    res = browser(cfg, "open-link", email, stdin_text=link, on_event=on_captcha, timeout=cfg["humanWaitMinutes"] * 60 + 120)
    link = None
    if res.get("result") == "signed_in":
        clear_need(st, email)
        acc["signedInAt"] = now_ms()
        event(st, "browser_signed_in", email=email)
        log("%s: Chrome profile signed in to claude.ai again" % email)
        return True
    if not told["done"]:
        need_person(st, email, "browser_signin_failed", "Sign in to claude.ai as %s in its Chrome window." % email)
    return False


# ---------------------------------------------------------------- renewing one sign-in

def renew_one(cfg, st, host, data, row, until_link=False):
    """Renews one sign-in. Returns "entered", "signed_out" or a failure word ("link_captured" with
    until_link: the login started and printed its link, which is then dropped unopened)."""
    email = row["email"]
    try:
        argv = login_command(host, dict(row, _claude=data.get("claude")), cfg)
    except RuntimeError as e:
        return str(e)
    prompt = Prompt(argv)
    try:
        url, end = None, time.time() + 60
        while not url and time.time() < end:
            if not prompt.read(1.0):
                break
            url = prompt.url()
        if not url:
            return "no_login_link"
        if until_link:
            return "link_captured"
        on_captcha, _ = captcha_hook(cfg, st, email)
        res = browser(cfg, "authorize", email, stdin_text=url, on_event=on_captcha)
        url = None
        if res.get("result") == "signed_out":
            return "signed_out"
        if res.get("result") == "captcha_timeout":
            return "captcha_timeout"
        if res.get("result") == "wrong_account":
            need_person(st, email, "wrong_account", "The Chrome profile kept for %s is signed in as %s. Sign it in to claude.ai as %s." % (email, res.get("signedInAs") or "another account", email))
            return "wrong_account"
        if res.get("result") != "code":
            return "browser_" + str(res.get("reason") or res.get("result"))
        code = res.pop("code")
        time.sleep(1)
        prompt.write(code.encode() + b"\r")
        code = None
        rc = prompt.wait(120)
        text = prompt.text().lower()
        if "success" not in text and rc != 0:
            return "login_failed"
        return "entered"
    finally:
        prompt.stop()


def verify_after(cfg, host, row_before):
    data = collect(host, cfg, row_before["dir"] if row_before["profile"] == "_test" else None)
    row = next((q for q in data.get("profiles", []) if q["profile"] == row_before["profile"]), None)
    if not row:
        return None, "profile_missing"
    if row.get("email") and row_before.get("email") and row["email"] != row_before["email"]:
        return row, "wrong_account_saved"
    if row["state"] == "ok" and (row.get("signInEndsAt") or 0) > (row_before.get("signInEndsAt") or 0) + 60000:
        return row, None
    return row, "end_not_moved"


# ---------------------------------------------------------------- scan, renew, verify, publish

def scan(cfg, st):
    """Collects every computer; returns [(host, data)] and keeps a private copy for publishing."""
    results = []
    for h in hosts(cfg):
        data = collect(h, cfg)
        results.append((h, data))
        hs = st.setdefault("hosts", {}).setdefault(h["name"], {})
        if "error" in data:
            hs["error"] = data["error"]
            hs["errorAt"] = now_ms()
            continue
        hs.pop("error", None)
        hs["readAt"] = now_ms()
        hs["os"] = h["os"]
        hs["claude"] = bool(data.get("claude"))
        seen = set()
        for row in data["profiles"]:
            k = key(h, row["profile"])
            seen.add(k)
            pst = st["profiles"].setdefault(k, {})
            for f in ("email", "state", "signInEndsAt", "logoutAt", "accessExpiresAt", "note", "default"):
                pst[f] = row.get(f)
            pst["readAt"] = now_ms()
            check_verification(st, k, pst, row)
        for k in list(st["profiles"]):
            if k.startswith(h["name"] + "/") and k not in seen:
                st["profiles"].pop(k)
    st["lastScanAt"] = now_ms()
    return results


def check_verification(st, k, pst, row):
    """A later reading of a renewed sign-in: passed, or written back by an old process."""
    expect = pst.get("expectEnd")
    if not expect:
        return
    now = now_ms()
    current = row.get("signInEndsAt") or 0
    if row.get("state") in ("logged_out", "no_refresh_token") or current < expect - 60000:
        if not pst.get("revertedAt") or pst["revertedAt"] < pst.get("lastRenewAt", 0):
            pst["revertedAt"] = now
            pst["reverts"] = int(pst.get("reverts") or 0) + 1
            event(st, "reverted", profile=k, email=row.get("email"))
            log("%s: the renewed sign-in went back (now ends %s); renewing again" % (k, human_time(current)))
        pst.pop("expectEnd", None)
        pst["verify"] = []
        return
    pst["verify"] = [t for t in pst.get("verify", []) if t > now]
    if not pst["verify"]:
        pst.pop("expectEnd", None)
        pst["verifiedAt"] = now


def renew_due(cfg, st, results, only=None, force=False, dry_run=False):
    now = now_ms()
    skip = set(cfg.get("skip") or [])
    plan = []
    for h, data in results:
        if "error" in data:
            continue
        for row in data["profiles"]:
            k = key(h, row["profile"])
            pst = st["profiles"].get(k, {})
            if only and not only(h, row):
                continue
            reason = "requested" if (force and only and row.get("email")) else due_reason(row, cfg, pst, now)
            if not reason:
                continue
            if k in skip and reason != "requested":
                continue
            if reason != "requested" and not backoff_ok(pst, now):
                continue
            plan.append((h, data, row, reason))
    plan.sort(key=lambda x: (x[2]["email"], x[0]["name"]))
    if not plan:
        return []
    log("renewing %d sign-in(s): %s" % (len(plan), ", ".join("%s (%s)" % (key(h, r["profile"]), why) for h, _, r, why in plan)))
    if dry_run:
        return [(key(h, r["profile"]), "planned") for h, _, r, _ in plan]
    outcomes = []
    blocked = set()
    for h, data, row, reason in plan:
        email = row["email"]
        k = key(h, row["profile"])
        if email in blocked:
            outcomes.append((k, "waiting_for_you"))
            continue
        pst = st["profiles"].setdefault(k, {})
        log("%s (%s): renewing; it ends %s" % (k, email, human_time(row.get("signInEndsAt"))))
        result = renew_one(cfg, st, h, data, row)
        if result == "signed_out":
            if sign_in_browser(cfg, st, email):
                # A person is at the screen right now: sign in the other accounts whose browser
                # session ends soon too, so their CAPTCHAs come in one sitting instead of on
                # separate days.
                batch_browser_sign_ins(cfg, st, exclude=email)
                result = renew_one(cfg, st, h, data, row)
            else:
                result = "captcha_timeout"
        if result == "captcha_timeout":
            # A person is needed for this account's browser; its other sign-ins wait for the next run.
            blocked.add(email)
            pst["lastResult"] = "waiting_for_you"
            pst["lastAttemptAt"] = now_ms()
            save_state(st)
            outcomes.append((k, "waiting_for_you"))
            continue
        if result == "entered":
            after, problem = verify_after(cfg, h, row)
            if problem is None:
                t = now_ms()
                old_end = row.get("signInEndsAt") or 0
                pst.update(lastRenewAt=t, lastResult="renewed", failures=0, retryAfter=0,
                           previousEnd=old_end or None, expectEnd=after["signInEndsAt"],
                           signInEndsAt=after["signInEndsAt"], logoutAt=after.get("logoutAt"), state=after["state"],
                           verify=sorted(set([t + int(cfg["verifyAfterHours"] * HOUR_MS)] +
                                             ([old_end + 9 * HOUR_MS] if old_end > t else []))))
                if reason != "reverted":
                    pst["reverts"] = 0
                clear_need(st, email)
                event(st, "renewed", profile=k, email=email, ends=after["signInEndsAt"])
                log("%s: renewed; the sign-in now ends %s" % (k, human_time(after["signInEndsAt"])))
                outcomes.append((k, "renewed"))
                save_state(st)
                continue
            result = problem
        note_failure(pst, now_ms(), result)
        event(st, "renew_failed", profile=k, email=email, reason=result)
        log("%s: renewal failed (%s); next try after %s" % (k, result, human_time(pst["retryAfter"])))
        if pst["failures"] >= 3 and (row.get("signInEndsAt") or 0) - now_ms() < 24 * HOUR_MS:
            need_person(st, email, "renewal_failing", "Renewing %s keeps failing (%s) and it ends %s. Run `claude_signin.py renew --host %s --profile %s` and watch, or sign in there by hand." % (k, result, human_time(row.get("signInEndsAt")), h["name"], row["profile"]))
            notify(cfg, "Claude sign-in renewal failing", "%s ends %s" % (k, human_time(row.get("signInEndsAt"))))
        outcomes.append((k, result))
        save_state(st)
    return outcomes


SESSION_SOON_MS = 5 * 24 * HOUR_MS


def browser_sessions(cfg, st, force=False):
    """Each account's claude.ai session end in its Chrome profile (cookie expiry only). Checked every
    6 hours; once a day it also opens claude.ai briefly to learn whether a visit extends the session."""
    now = now_ms()
    emails = sorted({p.get("email") for p in st["profiles"].values() if p.get("email")})
    for email in emails:
        acc = st["accounts"].setdefault(email, {})
        if not force and now - (acc.get("sessionCheckedAt") or 0) < 6 * HOUR_MS:
            continue
        visit = acc.get("signedIn") and now - (acc.get("visitedAt") or 0) > 24 * HOUR_MS and not acc.get("needs")
        res = browser(cfg, "session", email, extra=["--visit"] if visit else None, timeout=120)
        if res.get("result") != "ok":
            continue
        acc["sessionCheckedAt"] = now
        acc["signedIn"] = bool(res.get("signedIn"))
        acc["sessionEndsAt"] = parse_iso(res.get("sessionEndsAt"))
        if res.get("visited"):
            acc["visitedAt"] = now
            before, after = parse_iso(res.get("before")), acc["sessionEndsAt"]
            if not res.get("challenged") and before and after:
                acc["sessionSlides"] = after > before + 60000
                if after > before + 60000:
                    log("%s: the claude.ai visit extended its browser session to %s" % (email, human_time(after)))


def parse_iso(value):
    if not isinstance(value, str):
        return None
    try:
        return int(dt.datetime.strptime(value[:19], "%Y-%m-%dT%H:%M:%S").replace(tzinfo=dt.timezone.utc).timestamp() * 1000)
    except ValueError:
        return None


def batch_browser_sign_ins(cfg, st, exclude=None):
    now = now_ms()
    for email, acc in sorted(st["accounts"].items()):
        if email == exclude or acc.get("needs"):
            continue
        ends = acc.get("sessionEndsAt")
        if acc.get("signedIn") is False or (ends and ends - now < SESSION_SOON_MS):
            log("%s: its browser session ends %s; signing it in now while a person is here" % (email, human_time(ends)))
            if sign_in_browser(cfg, st, email):
                acc["signedIn"] = True
                acc["sessionCheckedAt"] = 0


def close_browsers(cfg, st):
    """Quits the Chrome profiles this run used, except one a person is still working in."""
    for email, acc in st["accounts"].items():
        if acc.get("needs") not in ("captcha",):
            browser(cfg, "close", email, timeout=60)


def check_browser_waits(cfg, st):
    """An account waiting for a person: done once its Chrome profile is signed in again, which the
    next renewal attempt finds out by itself. Reminds at most every 6 hours."""
    now = now_ms()
    for email, acc in st["accounts"].items():
        if acc.get("needs") and now - (acc.get("remindedAt") or acc.get("needsSince") or 0) > 6 * HOUR_MS:
            acc["remindedAt"] = now
            notify(cfg, "Claude sign-in still needs you", acc.get("needsHow") or email)
            if acc.get("needs") == "captcha":
                browser(cfg, "front", email, timeout=60)


SHARE_WRITE = r"""
import json, os, sys, tempfile
folder, files = sys.argv[1], json.load(sys.stdin)
for name, text in files.items():
    if "/" in name or "\\" in name or name.startswith("."):
        raise SystemExit("bad name")
    fd, tmp = tempfile.mkstemp(prefix=".tmp-", dir=folder)
    with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    os.chmod(tmp, 0o644)
    os.replace(tmp, os.path.join(folder, name))
"""

SHARE_READ = r"""
import json, os, sys
folder, names = sys.argv[1], sys.argv[2:]
out = {}
for name in names:
    try:
        with open(os.path.join(folder, name), "rb") as f:
            out[name] = f.read().decode("utf-8")
    except OSError:
        pass
print(json.dumps(out))
"""


def via_host(cfg):
    via = cfg.get("publishVia") or {}
    host = next((h for h in hosts(cfg) if h["name"] == via.get("host") and not h.get("local")), None)
    return (host, via.get("dir")) if host and via.get("dir") else (None, None)


def share_write(cfg, files):
    """Writes {name: text} into the snapshot folder, here or through publishVia. True when written."""
    folder = cfg.get("publishDir")
    if folder and os.path.isdir(folder):
        try:
            for name, text in files.items():
                write_atomic(os.path.join(folder, name), text, 0o644)
            return True
        except OSError as e:
            if not cfg.get("publishVia"):
                log("cannot write the publish folder (%s)" % type(e).__name__)
                return False
    host, remote = via_host(cfg)
    if not host:
        return False
    py = "python" if host["os"] == "windows" else "python3"
    try:
        r = subprocess.run(ssh_args(host) + ["%s -c %s %s" % (py, shq(SHARE_WRITE), shq(remote))],
                           input=json.dumps(files).encode(), capture_output=True, timeout=60)
    except subprocess.TimeoutExpired:
        return False
    if r.returncode != 0:
        log("publishing through %s failed: %s" % (host["name"], scrub(r.stderr.decode("utf-8", "replace").strip().splitlines()[-1:])))
    return r.returncode == 0


def share_read(cfg, sub, names):
    """Reads files from a subfolder of the snapshot folder, here or through publishVia."""
    folder = cfg.get("publishDir")
    out = {}
    if folder and os.path.isdir(os.path.join(folder, sub)):
        try:
            for name in names:
                with open(os.path.join(folder, sub, name), "rb") as f:
                    out[name] = f.read()
            return out
        except OSError:
            out = {}
    host, remote = via_host(cfg)
    if not host:
        return out
    py = "python" if host["os"] == "windows" else "python3"
    try:
        r = subprocess.run(ssh_args(host) + ["%s -c %s %s %s" % (py, shq(SHARE_READ), shq(remote.rstrip("/") + "/" + sub),
                                                                   " ".join(shq(n) for n in names))],
                           capture_output=True, timeout=60)
        if r.returncode == 0:
            return {k: v.encode("utf-8") for k, v in json.loads(r.stdout.decode("utf-8")).items()}
    except (subprocess.TimeoutExpired, ValueError):
        pass
    return out


def publish(cfg, st):
    if not cfg.get("publishDir") and not cfg.get("publishVia"):
        return False
    now = now_ms()
    rows = []
    for k, p in sorted(st["profiles"].items()):
        host, profile = k.split("/", 1)
        if profile in (cfg.get("ignoreProfiles") or []):
            continue
        rows.append({
            "host": host, "profile": profile, "email": p.get("email"), "state": p.get("state"),
            "signInEndsAt": iso(p.get("signInEndsAt")), "logoutAt": iso(p.get("logoutAt")),
            "renewFrom": iso((p.get("signInEndsAt") or 0) - int(cfg["renewWithinHours"] * HOUR_MS)) if p.get("signInEndsAt") else None,
            "autoRenew": k not in (cfg.get("skip") or []),
            "lastRenewAt": iso(p.get("lastRenewAt")), "lastResult": p.get("lastResult"),
            "nextVerifyAt": iso(min(p.get("verify") or [0]) or None) if p.get("verify") else None,
            "retryAfter": iso(p.get("retryAfter")) if (p.get("retryAfter") or 0) > now else None,
            "reverts": int(p.get("reverts") or 0), "readAt": iso(p.get("readAt")),
            "note": p.get("note") if p.get("state") in ("unreadable", "not_signed_in") else None,
        })
    needs = [{"email": e, "reason": a["needs"], "how": a.get("needsHow"), "since": iso(a.get("needsSince"))}
             for e, a in sorted(st["accounts"].items()) if a.get("needs")]
    sessions = [{"email": e, "signedIn": a.get("signedIn"), "sessionEndsAt": iso(a.get("sessionEndsAt")),
                 "checkedAt": iso(a.get("sessionCheckedAt")), "visitExtends": a.get("sessionSlides")}
                for e, a in sorted(st["accounts"].items()) if a.get("sessionCheckedAt")]
    hosts_out = [{"host": n, "readAt": iso(h.get("readAt")), "error": h.get("error")}
                 for n, h in sorted((st.get("hosts") or {}).items())]
    doc = {
        "version": SCHEMA, "source": "AI Account Center claude-signin", "runnerVersion": VERSION,
        "generatedAt": iso(now), "runner": this_host_name(cfg) or socket.gethostname().split(".")[0].lower(),
        "tickSeconds": 600, "staleAfterSeconds": 3600,
        "renewWithinHours": cfg["renewWithinHours"], "needsYou": needs, "hosts": hosts_out, "signIns": rows,
        "browserSessions": sessions,
    }
    # The JSON last: its generatedAt says the markdown beside it is from the same run.
    return share_write(cfg, {PUBLISH_MD: render_markdown(doc)}) and \
        share_write(cfg, {PUBLISH_JSON: json.dumps(doc, indent=1) + "\n"})


def render_markdown(doc):
    def when(v):
        if not v:
            return "unknown"
        t = dt.datetime.strptime(v, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
        return t.strftime("%a %b %d %H:%M UTC")
    lines = ["# Claude Code sign-ins", "",
             "Written %s by the renewal runner on %s. Sign-ins renew by themselves %d hours before they end." % (
                 when(doc["generatedAt"]), doc["runner"], doc["renewWithinHours"]), ""]
    if doc["needsYou"]:
        lines += ["## Needs you (a person)", ""]
        lines += ["- **%s**: %s (since %s)" % (n["email"], n["how"] or n["reason"], when(n["since"])) for n in doc["needsYou"]]
        lines.append("")
    for h in doc["hosts"]:
        if h.get("error"):
            lines.append("- **%s** could not be read: %s (last read %s)" % (h["host"], h["error"], when(h.get("readAt"))))
    if doc.get("browserSessions"):
        lines += ["", "Browser sessions (the Chrome profile per account that approves renewals; when one has ended,",
                  "the next renewal for that account needs one CAPTCHA):", ""]
        for b in doc["browserSessions"]:
            lines.append("- %s: %s" % (b["email"], ("signed in until %s" % when(b["sessionEndsAt"])) if b.get("signedIn")
                                       else "signed out"))
    lines += ["", "| Computer | Profile | Account | State | Sign-in ends | Logs out about | Last renewal |", "|---|---|---|---|---|---|---|"]
    for r in doc["signIns"]:
        lines.append("| %s | %s | %s | %s | %s | %s | %s |" % (
            r["host"], r["profile"], r["email"] or "?", r["state"] + ("" if r["autoRenew"] else " (not auto)"),
            when(r["signInEndsAt"]) if r["signInEndsAt"] else "-", when(r["logoutAt"]) if r["logoutAt"] else "-",
            ("%s %s" % (r["lastResult"], when(r["lastRenewAt"]) if r["lastRenewAt"] else "")).strip() if r["lastResult"] else "-"))
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------- commands

def cmd_status(args, cfg):
    if args.local:
        results = [(dict(name=this_host_name(cfg) or "local", os=host_os(), local=True), collect_local(cfg.get("ignoreProfiles") or []))]
    else:
        results = [(h, collect(h, cfg)) for h in hosts(cfg) if not args.host or h["name"] == args.host]
    if args.json:
        print(json.dumps([{"host": h["name"], **{k: v for k, v in d.items() if k != "profiles"},
                           "profiles": [{k: v for k, v in p.items() if k != "dir"} for p in d.get("profiles", [])]}
                          for h, d in results], indent=1))
        return 0
    st = load_state()
    rows = [("COMPUTER", "PROFILE", "ACCOUNT", "STATE", "SIGN-IN ENDS", "LOGS OUT ABOUT", "LAST RENEWAL")]
    for h, data in results:
        if "error" in data:
            rows.append((h["name"], "-", "-", "UNREACHABLE", data["error"][:50], "", ""))
            continue
        for p in data["profiles"]:
            pst = st["profiles"].get(key(h, p["profile"]), {})
            rows.append((h["name"], p["profile"], p["email"] or "?", p["state"], human_time(p["signInEndsAt"]),
                         human_time(p["logoutAt"]), pst.get("lastResult") or "-"))
    w = [max(len(str(r[i])) for r in rows) for i in range(len(rows[0]))]
    for i, r in enumerate(rows):
        print("  ".join(str(c).ljust(w[j]) for j, c in enumerate(r)).rstrip())
        if i == 0:
            print("  ".join("-" * x for x in w))
    for e, a in st["accounts"].items():
        if a.get("needs"):
            print("NEEDS YOU: %s" % (a.get("needsHow") or e))
    return 0


def cmd_tick(args, cfg):
    with Lock():
        st = load_state()
        st["lastTickAt"] = now_ms()
        sync_from_share(cfg)
        results = scan(cfg, st)
        save_state(st)
        renew_due(cfg, st, results)
        browser_sessions(cfg, st)
        check_browser_waits(cfg, st)
        close_browsers(cfg, st)
        save_state(st)
        publish(cfg, st)
    return 0


def cmd_renew(args, cfg):
    def only(h, row):
        if args.host and h["name"] != args.host:
            return False
        if args.profile and row["profile"] != args.profile:
            return False
        if args.account and args.account.lower() not in (row.get("email") or ""):
            return False
        return True
    with Lock():
        st = load_state()
        results = scan(cfg, st)
        save_state(st)
        named = bool(args.host or args.profile or args.account)
        outcomes = renew_due(cfg, st, results, only=only if named else None, force=args.force, dry_run=args.dry_run)
        save_state(st)
        publish(cfg, st)
    if not outcomes:
        print("Nothing to renew." + ("" if args.force else " Add --force to renew a named sign-in that is not due."))
    for k, result in outcomes:
        print("%-24s %s" % (k, result))
    return 0 if all(r in ("renewed", "planned") for _, r in outcomes) else 1


def cmd_test_login(args, cfg):
    """Renews a throwaway config folder on one computer: exercises the whole flow without touching a
    real sign-in. Deletes the folder (and on the Mac its Keychain item) afterwards."""
    host = next((h for h in hosts(cfg) if h["name"] == args.host), None)
    if not host:
        raise SystemExit("unknown computer %s" % args.host)
    data = collect(host, cfg)
    if "error" in data:
        raise SystemExit("cannot read %s: %s" % (host["name"], data["error"]))
    folder = args.dir
    if args.wait_minutes:
        cfg["humanWaitMinutes"] = args.wait_minutes
    row = {"profile": "_test", "dir": folder, "default": False, "email": args.account.lower(), "signInEndsAt": 0}
    st = load_state()
    result = renew_one(cfg, st, host, data, row, until_link=args.until_link)
    print("login:", result)
    if args.until_link:
        return 0 if result == "link_captured" else 1
    if result == "signed_out":
        print("that account's Chrome profile is signed out; run renew for a real profile to sign it in")
        return 1
    if result != "entered":
        return 1
    after, problem = verify_after(cfg, host, row)
    print("saved:", problem or "ok", "| account", (after or {}).get("email"), "| sign-in ends", human_time((after or {}).get("signInEndsAt")))
    return 0 if problem is None else 1


def sync_from_share(cfg):
    """Keeps this installed copy current with the one the dashboard publishes in the share folder."""
    install = os.path.join(state_dir(), "app")
    if os.path.abspath(HERE) != os.path.abspath(install):
        return
    names = ("claude_signin.py", "browser_auth.mjs", "README.md")
    fresh = share_read(cfg, "claude-signin", names)
    for name in names:
        b = os.path.join(install, name)
        new = fresh.get(name)
        try:
            with open(b, "rb") as f:
                old = f.read()
        except OSError:
            old = None
        if new and new != old and (name == "README.md" or new.startswith(b"#!")):
            with open(b + ".new", "wb") as f:
                f.write(new)
            os.replace(b + ".new", b)
            log("updated %s from the share folder" % name)


def cmd_install(args, cfg):
    install = os.path.join(state_dir(), "app")
    private_mkdir(state_dir())
    os.makedirs(install, exist_ok=True)
    for name in ("claude_signin.py", "browser_auth.mjs", "README.md"):
        src = os.path.join(HERE, name)
        if os.path.exists(src) and os.path.abspath(src) != os.path.abspath(os.path.join(install, name)):
            shutil.copyfile(src, os.path.join(install, name))
    target = os.path.join(install, "claude_signin.py")
    cfgpath = os.path.join(state_dir(), "config.json")
    user = read_json(cfgpath, {})
    if not user.get("nodePath") and node_path(cfg):
        user["nodePath"] = node_path(cfg)
    write_atomic(cfgpath, json.dumps(user, indent=1) + "\n")
    path_env = os.pathsep.join(filter(None, [os.path.dirname(user.get("nodePath") or ""), os.path.expanduser("~/.local/bin"),
                                             "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]))
    osname = host_os()
    if osname == "mac":
        plist = os.path.expanduser("~/Library/LaunchAgents/%s.plist" % LABEL)
        body = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>%s</string>
  <key>ProgramArguments</key><array><string>/usr/bin/python3</string><string>%s</string><string>tick</string></array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>%s</string>%s</dict>
  <key>StartInterval</key><integer>600</integer>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>%s</string>
  <key>StandardErrorPath</key><string>%s</string>
</dict></plist>
""" % (LABEL, target, path_env,
       "".join("<key>%s</key><string>%s</string>" % (k, os.environ[k]) for k in ("CCS_HOME", "CCS_DIR") if os.environ.get(k)),
       os.path.join(state_dir(), "launchd.log"), os.path.join(state_dir(), "launchd.log"))
        os.makedirs(os.path.dirname(plist), exist_ok=True)
        with open(plist, "w", encoding="utf-8") as f:
            f.write(body)
        uid = os.getuid()
        subprocess.run(["launchctl", "bootout", "gui/%d/%s" % (uid, LABEL)], capture_output=True)
        r = subprocess.run(["launchctl", "bootstrap", "gui/%d" % uid, plist], capture_output=True, text=True)
        print("installed %s (every 10 minutes, in this Mac's desktop session)%s" % (plist, "" if r.returncode == 0 else ": " + r.stderr.strip()))
        return r.returncode
    if osname == "linux":
        unit = os.path.expanduser("~/.config/systemd/user")
        os.makedirs(unit, exist_ok=True)
        with open(os.path.join(unit, "aac-claude-signin.service"), "w") as f:
            f.write("[Unit]\nDescription=AI Account Center Claude sign-in renewal\n\n[Service]\nType=oneshot\n"
                    "Environment=PATH=%s\nExecStart=/usr/bin/python3 %s tick\n" % (path_env, target))
        with open(os.path.join(unit, "aac-claude-signin.timer"), "w") as f:
            f.write("[Unit]\nDescription=AI Account Center Claude sign-in renewal\n\n[Timer]\nOnBootSec=2min\n"
                    "OnUnitActiveSec=10min\n\n[Install]\nWantedBy=timers.target\n")
        subprocess.run(["systemctl", "--user", "daemon-reload"])
        r = subprocess.run(["systemctl", "--user", "enable", "--now", "aac-claude-signin.timer"])
        print("installed the aac-claude-signin user timer (every 10 minutes)")
        return r.returncode
    task = "AI Account Center Claude sign-in"
    r = subprocess.run(["schtasks", "/Create", "/F", "/SC", "MINUTE", "/MO", "10", "/IT", "/TN", task,
                        "/TR", '"%s" "%s" tick' % (sys.executable.replace("python.exe", "pythonw.exe"), target)])
    print("installed the '%s' scheduled task (every 10 minutes, while you are signed in)" % task)
    return r.returncode


def cmd_uninstall(args, cfg):
    osname = host_os()
    if osname == "mac":
        plist = os.path.expanduser("~/Library/LaunchAgents/%s.plist" % LABEL)
        subprocess.run(["launchctl", "bootout", "gui/%d/%s" % (os.getuid(), LABEL)], capture_output=True)
        if os.path.exists(plist):
            os.unlink(plist)
    elif osname == "linux":
        subprocess.run(["systemctl", "--user", "disable", "--now", "aac-claude-signin.timer"], capture_output=True)
        for n in ("aac-claude-signin.service", "aac-claude-signin.timer"):
            p = os.path.expanduser("~/.config/systemd/user/" + n)
            if os.path.exists(p):
                os.unlink(p)
    else:
        subprocess.run(["schtasks", "/Delete", "/F", "/TN", "AI Account Center Claude sign-in"], capture_output=True)
    print("schedule removed; settings, state and Chrome profiles stay in %s" % state_dir())
    return 0


def cmd_doctor(args, cfg):
    ok = True

    def line(good, what, detail=""):
        nonlocal ok
        ok = ok and good
        print("%s %s%s" % ("ok  " if good else "FAIL", what, (": " + detail) if detail else ""))
    line(bool(cfg["hosts"]), "computers configured", ", ".join(h["name"] for h in hosts(cfg)))
    line(this_host_name(cfg) is not None or not cfg["hosts"], "this computer is one of them", socket.gethostname())
    n = node_path(cfg)
    if n:
        v = subprocess.run([n, "--version"], capture_output=True, text=True).stdout.strip()
        line(int(v.lstrip("v").split(".")[0] or 0) >= 22, "node 22 or later", v)
    else:
        line(False, "node 22 or later", "not found")
    line(bool(cfg.get("publishDir") and os.path.isdir(cfg["publishDir"])), "publish folder", str(cfg.get("publishDir")))
    for h in hosts(cfg):
        d = collect(h, cfg)
        if "error" in d:
            line(False, "read %s" % h["name"], d["error"])
        else:
            line(bool(d.get("claude")), "read %s" % h["name"], "%d sign-ins, claude %s" % (len(d["profiles"]), "found" if d.get("claude") else "NOT found"))
    if host_os() == "mac":
        r = subprocess.run(["launchctl", "print", "gui/%d/%s" % (os.getuid(), LABEL)], capture_output=True, text=True)
        line(r.returncode == 0, "schedule installed (launchd)")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd")
    s = sub.add_parser("status")
    s.add_argument("--local", action="store_true")
    s.add_argument("--host")
    s.add_argument("--json", action="store_true")
    sub.add_parser("tick")
    r = sub.add_parser("renew")
    r.add_argument("--host")
    r.add_argument("--profile")
    r.add_argument("--account")
    r.add_argument("--force", action="store_true", help="renew the named sign-ins even if they are not due")
    r.add_argument("--dry-run", action="store_true")
    t = sub.add_parser("test-login", help="renew a throwaway config folder on one computer (end-to-end check)")
    t.add_argument("--host", required=True)
    t.add_argument("--account", required=True)
    t.add_argument("--dir", required=True, help="absolute throwaway CLAUDE_CONFIG_DIR on that computer")
    t.add_argument("--wait-minutes", type=int, help="how long to wait for a person at a CAPTCHA")
    t.add_argument("--until-link", action="store_true", help="stop once the login prints its link (no browser)")
    sub.add_parser("install")
    sub.add_parser("uninstall")
    sub.add_parser("doctor")
    sub.add_parser("publish")
    j = sub.add_parser("_local-status")
    j.add_argument("--ignore", default="-")
    j.add_argument("--dir")
    args = ap.parse_args()
    if args.cmd == "_local-status":
        print(json.dumps(collect_local([] if args.ignore == "-" else args.ignore.split(","), args.dir)))
        return 0
    cfg = load_config()
    if args.cmd == "publish":
        return 0 if publish(cfg, load_state()) else 1
    handler = {"status": cmd_status, "tick": cmd_tick, "renew": cmd_renew, "install": cmd_install,
               "uninstall": cmd_uninstall, "doctor": cmd_doctor, "test-login": cmd_test_login}.get(args.cmd)
    if not handler:
        ap.print_help()
        return 0
    return handler(args, cfg)


if __name__ == "__main__":
    sys.exit(main())
