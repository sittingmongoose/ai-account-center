/**
 * The fixed Python program for Claude host steps on the Mac (create, undo,
 * state, session, trash, restore, purge). It reads `REQUEST` (an object the
 * server validated) and prints one JSON object; exit 3 means "already exists".
 * Data folders must be direct children of `~/Library/Application Support`
 * named `Claude*`, launchers direct children of `~/Applications` named
 * `Claude (...).app`, and trash folders live only in `~/.ccs/trash/claude`.
 */
export const MAC_HOST_PROGRAM = String.raw`
import json, os, plistlib, re, shlex, shutil, subprocess, sys
from pathlib import Path
home = Path.home()
support = home / "Library" / "Application Support"
apps = home / "Applications"
trash_root = home / ".ccs" / "trash" / "claude"
ID = re.compile(r"^[a-z][a-z0-9-]{1,31}\Z")
TRASH = re.compile(r"^[a-z][a-z0-9-]{1,31}-\d{8}T\d{6}Z\Z")
req = REQUEST
def done(value):
    sys.stdout.write(json.dumps(value))
    sys.exit(0)
def profile_dir(value):
    p = Path(value)
    if not p.is_absolute() or p.parent != support or not p.name.startswith("Claude") or p.is_symlink():
        sys.exit(1)
    return p
def launcher_app(value):
    p = Path(value)
    if not p.is_absolute() or p.parent != apps or not p.name.startswith("Claude (") or not p.name.endswith(").app") or p.is_symlink():
        sys.exit(1)
    return p
def profile_id():
    value = req.get("profileId")
    if not isinstance(value, str) or not ID.match(value):
        sys.exit(1)
    return value
def trash_dir(pid):
    name = req.get("trashName")
    if not isinstance(name, str) or not TRASH.match(name) or (pid and not name.startswith(pid + "-")):
        sys.exit(1)
    return trash_root / name
def build(launcher, profile, pid):
    executable = launcher / "Contents" / "MacOS" / "launch"
    executable.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
    resources = launcher / "Contents" / "Resources"
    resources.mkdir(mode=0o755, parents=True, exist_ok=True)
    command = ["/usr/bin/open", "-n", "-a", "Claude", "--args", "--user-data-dir=" + str(profile)]
    executable.write_text("#!/bin/sh\nexec " + " ".join(shlex.quote(a) for a in command) + "\n")
    executable.chmod(0o755)
    name = launcher.name[:-4]
    plist = {"CFBundleIdentifier": "com.aac.claudeprofile." + pid, "CFBundleName": name,
             "CFBundleDisplayName": name, "CFBundleExecutable": "launch", "CFBundlePackageType": "APPL",
             "CFBundleVersion": "1.0", "CFBundleShortVersionString": "1.0", "NSHighResolutionCapable": True,
             "LSUIElement": True}
    app = Path("/Applications/Claude.app")
    try:
        with (app / "Contents" / "Info.plist").open("rb") as source:
            icon = plistlib.load(source).get("CFBundleIconFile")
        if icon:
            icon = icon if Path(icon).suffix else icon + ".icns"
            shutil.copy2(app / "Contents" / "Resources" / icon, resources / icon)
            plist["CFBundleIconFile"] = icon
    except (OSError, ValueError):
        pass
    with (launcher / "Contents" / "Info.plist").open("wb") as target:
        plistlib.dump(plist, target)
    if Path("/usr/bin/plutil").exists():
        subprocess.run(["/usr/bin/plutil", "-lint", str(launcher / "Contents" / "Info.plist")], check=True, capture_output=True)
op = req.get("op")
if op == "create":
    pid = profile_id()
    profile = support / ("Claude-" + pid)
    launcher = apps / ("Claude (" + pid + ").app")
    if os.path.lexists(profile) or os.path.lexists(launcher):
        sys.exit(3)
    support.mkdir(parents=True, exist_ok=True)
    apps.mkdir(parents=True, exist_ok=True)
    profile.mkdir(mode=0o700)
    try:
        build(launcher, profile, pid)
    except Exception:
        shutil.rmtree(launcher, ignore_errors=True)
        os.rmdir(profile)
        sys.exit(1)
    done({"launcherName": launcher.name[:-4], "launcherPath": str(launcher), "profilePath": str(profile)})
if op == "undo":
    profile_id()
    launcher = launcher_app(req.get("launcherPath"))
    profile = profile_dir(req.get("profilePath"))
    shutil.rmtree(launcher, ignore_errors=True)
    try:
        os.rmdir(profile)
    except OSError:
        pass
    done({"ok": True})
if op == "state":
    profile = profile_dir(req.get("profilePath"))
    listing = subprocess.run(["/bin/ps", "-axww", "-o", "command="], capture_output=True, text=True, timeout=10)
    if listing.returncode != 0:
        sys.exit(1)
    needle = "--user-data-dir=" + str(profile)
    running = any(line.endswith(needle) or (needle + " ") in line for line in listing.stdout.splitlines())
    done({"running": running})
if op == "session":
    # Re-check reads the plaintext sign-in marker only: the account uuid and
    # the presence of an encrypted token cache. Tokens are never decrypted or
    # printed; the usage collector verifies the login afterwards.
    profile = profile_dir(req.get("profilePath"))
    config_path = profile / "config.json"
    if config_path.is_symlink():
        sys.exit(1)
    if not config_path.is_file():
        done({"signedIn": False})
    try:
        if config_path.stat().st_size > 1048576:
            sys.exit(1)
        with config_path.open("r", encoding="utf-8") as handle:
            config = json.load(handle)
    except OSError:
        sys.exit(1)
    except ValueError:
        # Unparseable: the app cannot be using it, and signing in rewrites
        # it, so this reads as signed out rather than failing.
        done({"signedIn": False})
    uuid_value = config.get("lastKnownAccountUuid") if isinstance(config, dict) else None
    cache_value = config.get("oauth:tokenCacheV2") if isinstance(config, dict) else None
    uuid_ok = isinstance(uuid_value, str) and re.fullmatch(
        r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
        uuid_value,
    ) is not None
    done({"signedIn": bool(uuid_ok and isinstance(cache_value, str) and cache_value)})
if op == "trash":
    pid = profile_id()
    launcher = launcher_app(req.get("launcherPath"))
    profile = profile_dir(req.get("profilePath"))
    trash_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    target = trash_dir(pid)
    if os.path.lexists(target):
        sys.exit(1)
    moved = False
    if profile.exists():
        if os.stat(profile).st_dev != os.stat(trash_root).st_dev:
            done({"result": "cross_volume"})
        try:
            os.rename(profile, target)
        except OSError as error:
            if error.errno == 18:
                done({"result": "cross_volume"})
            raise
        moved = True
    else:
        target.mkdir(mode=0o700)
    try:
        shutil.rmtree(launcher) if launcher.exists() else None
    except Exception:
        if moved:
            os.rename(target, profile)
        sys.exit(1)
    done({"result": "moved"})
if op == "restore":
    pid = profile_id()
    launcher = launcher_app(req.get("launcherPath"))
    profile = profile_dir(req.get("profilePath"))
    source = trash_dir(pid)
    if not source.is_dir() or source.is_symlink() or os.path.lexists(profile):
        sys.exit(3)
    os.rename(source, profile)
    if not launcher.exists():
        build(launcher, profile, pid)
    done({"ok": True})
if op == "purge":
    target = trash_dir(None)
    if target.is_symlink():
        sys.exit(1)
    if target.exists():
        shutil.rmtree(target)
    done({"ok": True})
sys.exit(2)
`;
