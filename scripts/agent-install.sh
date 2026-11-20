#!/bin/sh
# Lane E (M3D4 staged, M3D5 pinned): place the hash-pinned aac-agent in AAC's cache folder on
# this computer. The lead runs this on each live remote computer (Nas1, the Mac); the Rust
# service admits only an already installed, pinned agent and never uploads one itself. The VM
# is Local: its packaged dist/agent binary is used in place, so nothing is installed there.
# This script never builds (the VM must not build Rust) and never edits config or trust files.
#
# Usage: sh scripts/agent-install.sh <binary> <sha256>
#   <binary>  the packaged aac-agent for this computer (dist/agent/<target>/aac-agent)
#   <sha256>  its pinned digest from config/agent-artifacts.json
#
# Verifies the source digest, stages a .part copy, verifies it again, renames atomically, then
# requires the --version handshake {name, version, protocol, target} for this OS.
set -eu

binary=${1:?usage: agent-install.sh <binary> <sha256>}
expected=${2:?usage: agent-install.sh <binary> <sha256>}

case "$expected" in
  *[!a-f0-9]*) echo '[X] sha256 must be 64 lowercase hex characters' >&2; exit 1 ;;
esac
[ "${#expected}" -eq 64 ] || { echo '[X] sha256 must be 64 lowercase hex characters' >&2; exit 1; }
[ -f "$binary" ] || { echo "[X] no such binary: $binary" >&2; exit 1; }
command -v node >/dev/null 2>&1 || { echo '[X] node is required to check the handshake' >&2; exit 1; }

hash_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

actual=$(hash_of "$binary")
[ "$actual" = "$expected" ] || {
  echo "[X] binary digest $actual does not match the pinned $expected" >&2
  exit 1
}

case "$(uname -s)" in
  Linux) pattern='^x86_64-unknown-linux-(gnu|musl)$' ;;
  Darwin) pattern='^(aarch64|x86_64)-apple-darwin$' ;;
  *) echo '[X] unsupported OS here; use scripts/agent-install.ps1 on Windows' >&2; exit 1 ;;
esac

dir="$HOME/.cache/ai-account-center/bin"
mkdir -p "$dir"
chmod 700 "$dir" 2>/dev/null || true
dest="$dir/aac-agent"
part="$dir/aac-agent.part.$$"
trap 'rm -f "$part"' EXIT

cp "$binary" "$part"
chmod 755 "$part"
[ "$(hash_of "$part")" = "$expected" ] || { echo '[X] staged copy digest mismatch' >&2; exit 1; }
mv -f "$part" "$dest"

identity=$("$dest" --version) || { echo '[X] the installed agent did not answer --version' >&2; exit 1; }
printf '%s' "$identity" | node -e '
const pattern = new RegExp(process.argv[1]);
let s = "";
process.stdin.on("data", (d) => (s += d)).on("end", () => {
  const j = JSON.parse(s);
  if (j.name !== "aac-agent" || j.protocol !== 1 || typeof j.version !== "string" || !pattern.test(j.target))
    process.exit(1);
  console.log("[OK] aac-agent " + j.version + " (" + j.target + ") installed and pinned");
});
' "$pattern" || { echo '[X] unexpected agent identity for this computer' >&2; exit 1; }
