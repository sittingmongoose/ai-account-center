"""Where T3's Antigravity ACP update gets its target, and where a sign-in survives it.

This is not a version pin. T3 (0.1.10-nightly.20261113.4616 and later) installs
Google's newest native Antigravity CLI from the CLI channel below
(apps/server/src/provider/antigravityCliRelease.ts, AntigravityInstallation.ts
refreshRelease/start). AAC reads that same manifest only to know what is
newest, and lets T3 choose and install it.

The 1.3/1.4 runtime pair (agy_acp_server + localharness_external) kept its
Google sign-in in a file T3 points it at (AGY_ACP_FORCE_FILE_STORAGE=1). The
native 1.5+ CLI does not import that file: on 2026-11-13 T3's 1.4.5 -> 1.5.8
install signed both Antigravity instances out on the headless Ubuntu VM.
Windows kept its sign-in, because the older runtime had also stored it in
Credential Manager. The Mac is untested, and the owner chose to hold Nas1 and
the Mac on 1.4.5 until T3 carries the sign-in over (decision B, 2026-11-13).
"""

CLI_MANIFEST_BASE = "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/"
# T3's parseAntigravityCliRelease refuses anything older: the CLI channel starts at 1.5.0.
NATIVE_FROM = (1, 5, 0)
# Hosts where moving from the runtime pair to the native CLI is known to keep the sign-in.
# Everywhere else that move waits for the user (antigravity_acp_signin_hold).
SIGNIN_CARRIES_TO_NATIVE = ("windows",)
