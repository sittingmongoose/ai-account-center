"""Keep installation discovery inside each test's own home folder.

Owner discovery reads PATH, the PATH node's own prefix and the usual system
package folders, as it must on a real computer. A test runs on a real computer
too, so without this a fixture would find the developer's own cursor-agent or
ACP adapters. Importing this module confines those lookups to the patched
home (pathlib.Path.home); a test that leaves home unpatched sees nothing.
Tests of the discovery itself call REAL[...] with their own fixtures.
"""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
import app_update_cursor as cursor  # noqa: E402
import app_update_zcode as zcode  # noqa: E402

REAL_HOME = pathlib.Path.home().resolve()
# getattr: the same tests also run against an older helper (fail-before evidence) that lacks these.
REAL = {
    "effective_path": getattr(zcode, "effective_path", None),
    "candidate_roots": getattr(zcode, "candidate_roots", None),
    "path_copies": getattr(cursor, "path_copies", None),
}


def inside_home(path):
    home = pathlib.Path.home()
    try:
        home = home.resolve()
        if home == REAL_HOME:
            return False
        return pathlib.Path(path).resolve().is_relative_to(home)
    except (OSError, RuntimeError, ValueError):
        return False


if not getattr(zcode, "_hermetic", False):
    zcode._hermetic = True
    if REAL["effective_path"]:
        zcode.effective_path = lambda host: [folder for folder in REAL["effective_path"](host) if inside_home(folder)]
    if REAL["candidate_roots"]:
        zcode.candidate_roots = lambda host: [root for root in REAL["candidate_roots"](host) if inside_home(root)]
    if REAL["path_copies"]:
        cursor.path_copies = lambda platform: [copy for copy in REAL["path_copies"](platform) if inside_home(copy)]
