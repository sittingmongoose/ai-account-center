"""Minimum Antigravity ACP pins; T3 owns download verification.

Source: pingdotgg/t3code, apps/server/src/provider/antigravityRelease.ts
Commit: ab8e488e10d883fa880b0ddcf01917b51839ab07 (MIT, T3 Code contributors).
Matches T3 0.1.10-nightly.20261112.4580; update these pins with T3.
"""

RELEASES = {
    "darwin-arm64": {"version": "1.4.5"},
    "darwin-x64": {"version": "1.4.5"},
    "linux-x64": {"version": "1.4.5"},
    "linux-arm64": {"version": "1.4.5"},
    "win32-x64": {"version": "1.4.5"},
    "win32-arm64": {"version": "1.4.5"},
}
