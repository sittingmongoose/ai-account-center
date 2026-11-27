#!/usr/bin/env bash
# Build and smoke-test the release-packaged Docker image without touching an
# existing deployment or real user home.
#
# Usage:
#   docker/smoke-test.sh --release-tgz /path/to/sittingmongoose-ai-account-center-8.10.0.tgz [--commit <40-hex>] [--sha256 <64-hex>] [--skip-checkout-verify]
#   AAC_RELEASE_TGZ_PATH=/path/to/pkg.tgz docker/smoke-test.sh
#   AAC_RELEASE_DIR=/path/to/release/linux [AAC_RELEASE_TGZ=pkg.tgz] docker/smoke-test.sh
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

IMAGE=aac-d1-smoke:local
CONTAINER=aac-d1-smoke
VOLUME=aac-d1-smoke-home
USERNAME=aac-smoke
PASSWORD=smoke-test-password
PASSWORD_HASH='$2b$04$yRkuB9Itj06NxPrDDNTUeOJaEX4i3R2BvEHKbTx5VNmbu9iZrFWO.'
SESSION_SECRET=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

RELEASE_TGZ_PATH="${AAC_RELEASE_TGZ_PATH:-}"
RELEASE_DIR="${AAC_RELEASE_DIR:-}"
RELEASE_TGZ="${AAC_RELEASE_TGZ:-sittingmongoose-ai-account-center-8.10.0.tgz}"
BUILD_COMMIT="${AAC_BUILD_COMMIT:-}"
RELEASE_SHA256="${AAC_RELEASE_SHA256:-}"
VERIFY_AGAINST_CHECKOUT="${AAC_VERIFY_AGAINST_CHECKOUT:-auto}"

usage() {
  cat >&2 <<'EOF'
Usage: docker/smoke-test.sh --release-tgz <path-to-linux-release.tgz> [options]
   or: docker/smoke-test.sh --release-dir <dir> [--release-tgz-name <filename>] [options]

Options:
  --release-tgz <path>       Path to the gated Linux release .tgz
  --release-dir <dir>        Directory containing the gated Linux release .tgz
  --release-tgz-name <file>  Tarball filename inside --release-dir (default: sittingmongoose-ai-account-center-8.10.0.tgz)
  --commit <40-hex>          Expected AAC_BUILD_COMMIT (default: read from tarball dist/build-info.json)
  --sha256 <64-hex>          Expected tarball SHA-256 (default: read from SHA256SUMS if present)
  --skip-checkout-verify     Skip host-side stage-dashboard-package.js --verify-release-artifacts check
EOF
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --release-tgz)
      RELEASE_TGZ_PATH="${2:?Missing value for --release-tgz}"
      shift 2
      ;;
    --release-dir)
      RELEASE_DIR="${2:?Missing value for --release-dir}"
      shift 2
      ;;
    --release-tgz-name)
      RELEASE_TGZ="${2:?Missing value for --release-tgz-name}"
      shift 2
      ;;
    --commit)
      BUILD_COMMIT="${2:?Missing value for --commit}"
      shift 2
      ;;
    --sha256)
      RELEASE_SHA256="${2:?Missing value for --sha256}"
      shift 2
      ;;
    --skip-checkout-verify)
      VERIFY_AGAINST_CHECKOUT=off
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "[X] Unknown option: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -n "$RELEASE_TGZ_PATH" ]]; then
  RELEASE_DIR=$(cd -- "$(dirname -- "$RELEASE_TGZ_PATH")" && pwd)
  RELEASE_TGZ=$(basename -- "$RELEASE_TGZ_PATH")
  RELEASE_TGZ_PATH="$RELEASE_DIR/$RELEASE_TGZ"
elif [[ -n "$RELEASE_DIR" ]]; then
  RELEASE_DIR=$(cd -- "$RELEASE_DIR" && pwd)
  RELEASE_TGZ_PATH="$RELEASE_DIR/$RELEASE_TGZ"
else
  echo "[X] Missing gated Linux release tarball." >&2
  usage
  exit 2
fi

if [[ ! -f "$RELEASE_TGZ_PATH" ]]; then
  echo "[X] Release tarball not found: $RELEASE_TGZ_PATH" >&2
  exit 1
fi

for tool in docker curl tar sha256sum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "[X] Required command not found: $tool" >&2
    exit 1
  fi
done

JSON_TOOL=""
for candidate in node jq python3; do
  if command -v "$candidate" >/dev/null 2>&1; then
    JSON_TOOL="$candidate"
    break
  fi
done
if [[ -z "$JSON_TOOL" ]]; then
  echo "[X] Required JSON reader not found on host (need node, jq, or python3)." >&2
  exit 1
fi

# Never adopt or delete a resource this script did not create.
for existing in "$CONTAINER" "$VOLUME"; do
  if docker container inspect "$existing" >/dev/null 2>&1 || docker volume inspect "$existing" >/dev/null 2>&1; then
    echo "[X] Test resource already exists: $existing" >&2
    exit 1
  fi
done
if docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "[X] Test image already exists: $IMAGE" >&2
  exit 1
fi

TEMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/aac-d1-smoke.XXXXXX")
SSH_DIR="$TEMP_DIR/ssh"
COOKIE_JAR="$TEMP_DIR/cookies"
LOGIN_BODY="$TEMP_DIR/login.json"

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  docker volume rm "$VOLUME" >/dev/null 2>&1 || true
  docker image rm "$IMAGE" >/dev/null 2>&1 || true
  rm -rf -- "${TEMP_DIR:?}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [[ -z "$RELEASE_SHA256" && -f "$RELEASE_DIR/SHA256SUMS" ]]; then
  RELEASE_SHA256=$(awk -v f="$RELEASE_TGZ" '$2 == f || $2 == "*" f { print $1; exit }' "$RELEASE_DIR/SHA256SUMS")
fi

if [[ -z "$BUILD_COMMIT" ]]; then
  tar -xOzf "$RELEASE_TGZ_PATH" package/dist/build-info.json > "$TEMP_DIR/tgz-build-info.json"
  case "$JSON_TOOL" in
    node)
      BUILD_COMMIT=$(node -e 'const d = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")); if (!/^[0-9a-f]{40}$/.test(d.commit || "")) process.exit(1); console.log(d.commit);' "$TEMP_DIR/tgz-build-info.json")
      ;;
    jq)
      BUILD_COMMIT=$(jq -er '.commit | select(test("^[0-9a-f]{40}$"))' "$TEMP_DIR/tgz-build-info.json")
      ;;
    python3)
      BUILD_COMMIT=$(python3 -c 'import json, re, sys; c = json.load(open(sys.argv[1])).get("commit", ""); assert re.fullmatch(r"[0-9a-f]{40}", c); print(c)' "$TEMP_DIR/tgz-build-info.json")
      ;;
  esac
fi

if [[ ! "$BUILD_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "[X] Invalid AAC_BUILD_COMMIT: $BUILD_COMMIT" >&2
  exit 1
fi

mkdir -p "$SSH_DIR"
chmod 0700 "$SSH_DIR"
printf 'Host *\n  StrictHostKeyChecking no\n' > "$SSH_DIR/config"
chmod 0600 "$SSH_DIR/config"

if [[ -n "${AAC_D1_BASELINE_IMAGE:-}" ]]; then
  docker image inspect "$AAC_D1_BASELINE_IMAGE" --format 'Baseline image size: {{.Size}} bytes'
fi

echo "[1/10] Building $IMAGE from $RELEASE_TGZ (commit $BUILD_COMMIT)"
docker build --pull \
  --build-context "release=$RELEASE_DIR" \
  --build-arg "AAC_BUILD_COMMIT=$BUILD_COMMIT" \
  --build-arg "AAC_RELEASE_TGZ=$RELEASE_TGZ" \
  --build-arg "AAC_RELEASE_SHA256=$RELEASE_SHA256" \
  -f docker/Dockerfile \
  -t "$IMAGE" .
docker image inspect "$IMAGE" --format 'Rewritten image size: {{.Size}} bytes'
image_revision=$(docker image inspect "$IMAGE" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
if [[ "$image_revision" != "$BUILD_COMMIT" ]]; then
  echo "[X] Image revision label is '$image_revision', expected '$BUILD_COMMIT'" >&2
  exit 1
fi

echo "[2/10] Creating empty test volume and SSH mount"
docker volume create "$VOLUME" >/dev/null

echo "[3/10] Starting isolated non-root container"
docker run --detach --name "$CONTAINER" \
  --init \
  --publish '127.0.0.1::3000' \
  --env CCS_DASHBOARD_AUTH_ENABLED=true \
  --env "CCS_DASHBOARD_USERNAME=$USERNAME" \
  --env "CCS_DASHBOARD_PASSWORD_HASH=$PASSWORD_HASH" \
  --env "CCS_SESSION_SECRET=$SESSION_SECRET" \
  --env NO_COLOR=1 \
  --volume "$VOLUME:/home/node/.ccs" \
  --mount "type=bind,source=$SSH_DIR,target=/home/node/.ssh,readonly" \
  "$IMAGE" >/dev/null

PORT=$(docker port "$CONTAINER" 3000/tcp | awk -F: 'NR == 1 { print $NF }')
if [[ -z "$PORT" ]]; then
  echo "[X] Docker did not publish the dashboard port" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi
BASE_URL="http://127.0.0.1:$PORT"

echo "[4/10] Waiting for public /api/health endpoint"
health_code=000
for _ in $(seq 1 60); do
  health_code=$(curl --silent --output /dev/null --write-out '%{http_code}' --max-time 2 "$BASE_URL/api/health" || true)
  [[ "$health_code" == 200 ]] && break
  sleep 1
done
if [[ "$health_code" != 200 ]]; then
  echo "[X] /api/health returned $health_code" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi
docker exec "$CONTAINER" curl --fail --silent --show-error --output /dev/null --max-time 4 http://127.0.0.1:3000/api/health

echo "[5/10] Signing in with test-only credentials and checking /app/home"
login_code=$(curl --silent --show-error --output "$LOGIN_BODY" --write-out '%{http_code}' \
  --cookie-jar "$COOKIE_JAR" \
  --header "Origin: $BASE_URL" \
  --header 'Content-Type: application/json' \
  --data "{\"username\":\"$USERNAME\",\"password\":\"$PASSWORD\",\"rememberMe\":true}" \
  "$BASE_URL/api/auth/login")
if [[ "$login_code" != 200 ]]; then
  echo "[X] Test login returned $login_code" >&2
  cat "$LOGIN_BODY" >&2 || true
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi
home_code=$(curl --silent --show-error --output /dev/null --write-out '%{http_code}' \
  --cookie "$COOKIE_JAR" \
  --header "Origin: $BASE_URL" \
  --max-time 10 \
  "$BASE_URL/app/home")
if [[ "$home_code" != 200 ]]; then
  echo "[X] /app/home returned $home_code" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi

echo "[6/10] Checking non-root identity and compatible mounts"
uid=$(docker exec "$CONTAINER" id -u)
if [[ "$uid" != 1000 ]]; then
  echo "[X] Runtime UID is $uid, expected 1000" >&2
  exit 1
fi
docker exec "$CONTAINER" test -w /home/node/.ccs
docker exec "$CONTAINER" test -r /home/node/.ssh/config
docker exec "$CONTAINER" test -r /app/web-dashboard/public/computers.json

echo "[7/10] Checking runtime tools and absent interpreters"
docker exec "$CONTAINER" test -x /usr/local/bin/ai-account-center
docker exec "$CONTAINER" test -x /usr/local/bin/ccs
docker exec "$CONTAINER" ai-account-center --help >/dev/null
docker exec "$CONTAINER" ccs --help >/dev/null
docker exec "$CONTAINER" test -x /usr/bin/ssh
docker exec "$CONTAINER" test -x /usr/bin/curl
for interpreter in node bun python3 jq; do
  if docker exec "$CONTAINER" sh -c "command -v '$interpreter' >/dev/null 2>&1"; then
    echo "[X] Runtime unexpectedly contains $interpreter" >&2
    exit 1
  fi
done
if docker exec "$CONTAINER" sh -c 'ls /app/dist/native/*.node >/dev/null 2>&1'; then
  echo "[X] Runtime unexpectedly contains obsolete .node NAPI addons" >&2
  exit 1
fi

echo "[8/10] Checking compiled commit, dist/build-info.json, and live deployed-build.json"
container_commit=$(docker exec "$CONTAINER" /app/dist/native/aac-http --build-commit)
if [[ "$container_commit" != "$BUILD_COMMIT" ]]; then
  echo "[X] Container aac-http --build-commit is '$container_commit', expected '$BUILD_COMMIT'" >&2
  exit 1
fi
docker exec "$CONTAINER" cat /app/dist/build-info.json > "$TEMP_DIR/container-build-info.json"
ledger_ready=0
for _ in $(seq 1 30); do
  if docker exec "$CONTAINER" test -s /home/node/.ccs/deployed-build.json; then
    ledger_ready=1
    break
  fi
  sleep 0.5
done
if [[ "$ledger_ready" != 1 ]]; then
  echo "[X] Running container did not write /home/node/.ccs/deployed-build.json" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi
docker exec "$CONTAINER" cat /home/node/.ccs/deployed-build.json > "$TEMP_DIR/container-deployed-build.json"
case "$JSON_TOOL" in
  node)
    node -e '
      const fs = require("fs");
      const [infoPath, ledgerPath, expected] = process.argv.slice(1);
      const info = JSON.parse(fs.readFileSync(infoPath, "utf8"));
      const ledger = JSON.parse(fs.readFileSync(ledgerPath, "utf8"));
      if (info.version !== 1 || info.commit !== expected || info.dirty !== false || info.onMain !== true || !Array.isArray(info.lineage) || info.lineage[0] !== expected) {
        throw new Error("Container dist/build-info.json mismatch");
      }
      if (ledger.version !== 1 || ledger.commit !== expected || ledger.dirty !== false || !String(ledger.recordedAt || "").endsWith("Z")) {
        throw new Error("Container deployed-build.json mismatch");
      }
    ' "$TEMP_DIR/container-build-info.json" "$TEMP_DIR/container-deployed-build.json" "$BUILD_COMMIT"
    ;;
  jq)
    jq -e --arg c "$BUILD_COMMIT" '.version == 1 and .commit == $c and .dirty == false and .onMain == true and (.lineage | type == "array" and .[0] == $c)' "$TEMP_DIR/container-build-info.json" >/dev/null
    jq -e --arg c "$BUILD_COMMIT" '.version == 1 and .commit == $c and .dirty == false and (.recordedAt | endswith("Z"))' "$TEMP_DIR/container-deployed-build.json" >/dev/null
    ;;
  python3)
    python3 -c '
import json, sys
info = json.load(open(sys.argv[1]))
ledger = json.load(open(sys.argv[2]))
expected = sys.argv[3]
assert info.get("version") == 1 and info.get("commit") == expected and info.get("dirty") is False and info.get("onMain") is True and info.get("lineage", [None])[0] == expected
assert ledger.get("version") == 1 and ledger.get("commit") == expected and ledger.get("dirty") is False and str(ledger.get("recordedAt", "")).endswith("Z")
' "$TEMP_DIR/container-build-info.json" "$TEMP_DIR/container-deployed-build.json" "$BUILD_COMMIT"
    ;;
esac

echo "[9/10] Verifying all three release targets inside container (agents, Cursor readers, scanners)"
docker exec "$CONTAINER" cat /app/config/agent-artifacts.json > "$TEMP_DIR/agent-artifacts.json"
docker exec "$CONTAINER" cat /app/dist/agent/manifest.json > "$TEMP_DIR/agent-manifest.json"
docker exec "$CONTAINER" cat /app/dist/native/aac-cursor-account/manifest.json > "$TEMP_DIR/cursor-manifest.json"
docker exec "$CONTAINER" cat /app/dist/native/aac-usage/manifest.json > "$TEMP_DIR/usage-manifest.json"

case "$JSON_TOOL" in
  node)
    node -e '
      const fs = require("fs");
      const [catalogPath, agentManPath, cursorManPath, usageManPath] = process.argv.slice(1);
      const targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"];
      const catalog = JSON.parse(fs.readFileSync(catalogPath, "utf8"));
      const agentMan = JSON.parse(fs.readFileSync(agentManPath, "utf8"));
      const cursorMan = JSON.parse(fs.readFileSync(cursorManPath, "utf8"));
      const usageMan = JSON.parse(fs.readFileSync(usageManPath, "utf8"));
      if (agentMan.name !== "aac-agent") throw new Error("Invalid agent manifest name");
      for (const target of targets) {
        const pins = (catalog.artifacts || []).filter((a) => a.target === target);
        if (pins.length !== 1) throw new Error(`Expected 1 catalog pin for ${target}`);
        const pin = pins[0];
        const am = (agentMan.targets || {})[target];
        if (!am || pin.version !== agentMan.version || pin.file !== `dist/agent/${am.file}` || pin.sha256 !== am.sha256) {
          throw new Error(`Agent catalog/manifest mismatch for ${target}`);
        }
        if (!(am.size > 0 && am.size <= 67108864)) throw new Error(`Agent size out of bounds for ${target}`);
        console.log(`agent\t${target}\t/app/${pin.file}\t${pin.sha256}\t${am.size}`);
        const cm = (cursorMan.targets || {})[target];
        if (!cm || !cm.file || !cm.sha256 || !(cm.size > 0)) throw new Error(`Cursor manifest missing ${target}`);
        console.log(`cursor\t${target}\t/app/dist/native/aac-cursor-account/${cm.file}\t${cm.sha256}\t${cm.size}`);
        const um = (usageMan.targets || {})[target];
        if (!um || !um.file || !um.sha256 || !(um.size > 0)) throw new Error(`Usage manifest missing ${target}`);
        console.log(`usage\t${target}\t/app/dist/native/aac-usage/${um.file}\t${um.sha256}\t${um.size}`);
      }
    ' "$TEMP_DIR/agent-artifacts.json" "$TEMP_DIR/agent-manifest.json" "$TEMP_DIR/cursor-manifest.json" "$TEMP_DIR/usage-manifest.json" > "$TEMP_DIR/artifact-checks.tsv"
    ;;
  jq)
    jq -rn --slurpfile cat "$TEMP_DIR/agent-artifacts.json" \
      --slurpfile am "$TEMP_DIR/agent-manifest.json" \
      --slurpfile cm "$TEMP_DIR/cursor-manifest.json" \
      --slurpfile um "$TEMP_DIR/usage-manifest.json" '
      ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"][] as $t |
      ($cat[0].artifacts | map(select(.target == $t))) as $pins |
      if ($pins | length) != 1 then error("missing pin for " + $t) else . end |
      ($pins[0]) as $pin |
      ($am[0].targets[$t]) as $a |
      if ($am[0].name != "aac-agent" or $pin.version != $am[0].version or $pin.file != ("dist/agent/" + $a.file) or $pin.sha256 != $a.sha256 or ($a.size <= 0 or $a.size > 67108864)) then error("agent mismatch " + $t) else . end |
      ($cm[0].targets[$t]) as $c |
      ($um[0].targets[$t]) as $u |
      if ($c == null or $u == null) then error("missing helper " + $t) else . end |
      "agent\t\($t)\t/app/\($pin.file)\t\($pin.sha256)\t\($a.size)",
      "cursor\t\($t)\t/app/dist/native/aac-cursor-account/\($c.file)\t\($c.sha256)\t\($c.size)",
      "usage\t\($t)\t/app/dist/native/aac-usage/\($u.file)\t\($u.sha256)\t\($u.size)"
    ' > "$TEMP_DIR/artifact-checks.tsv"
    ;;
  python3)
    python3 -c '
import json, sys
cat = json.load(open(sys.argv[1]))
am = json.load(open(sys.argv[2]))
cm = json.load(open(sys.argv[3]))
um = json.load(open(sys.argv[4]))
assert am.get("name") == "aac-agent"
for t in ("x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"):
    pins = [a for a in cat.get("artifacts", []) if a.get("target") == t]
    assert len(pins) == 1, f"missing pin {t}"
    pin = pins[0]
    a = am["targets"][t]
    assert pin["version"] == am["version"] and pin["file"] == f"dist/agent/{a[\"file\"]}" and pin["sha256"] == a["sha256"]
    assert 0 < a["size"] <= 67108864
    print(f"agent\t{t}\t/app/{pin[\"file\"]}\t{pin[\"sha256\"]}\t{a[\"size\"]}")
    c = cm["targets"][t]
    print(f"cursor\t{t}\t/app/dist/native/aac-cursor-account/{c[\"file\"]}\t{c[\"sha256\"]}\t{c[\"size\"]}")
    u = um["targets"][t]
    print(f"usage\t{t}\t/app/dist/native/aac-usage/{u[\"file\"]}\t{u[\"sha256\"]}\t{u[\"size\"]}")
' "$TEMP_DIR/agent-artifacts.json" "$TEMP_DIR/agent-manifest.json" "$TEMP_DIR/cursor-manifest.json" "$TEMP_DIR/usage-manifest.json" > "$TEMP_DIR/artifact-checks.tsv"
    ;;
esac

while IFS=$'\t' read -r kind target container_path expected_sha expected_size; do
  docker exec "$CONTAINER" bash -euo pipefail -c '
    p="$1"; want_sha="$2"; want_size="$3"
    test -f "$p" && test ! -L "$p" && test -r "$p"
    got_size=$(stat -c "%s" "$p")
    test "$got_size" = "$want_size"
    got_sha=$(sha256sum "$p" | awk "{print \$1}")
    test "$got_sha" = "$want_sha"
  ' _ "$container_path" "$expected_sha" "$expected_size"
  echo "  [OK] in-container $kind $target (${expected_sha:0:12}, ${expected_size} B)"
done < "$TEMP_DIR/artifact-checks.tsv"

# Verify Linux native binaries execute inside the container as UID 1000.
docker exec "$CONTAINER" /app/dist/agent/x86_64-unknown-linux-gnu/aac-agent --version | grep -q '"target":"x86_64-unknown-linux-gnu"'
docker exec "$CONTAINER" /app/dist/native/aac-cursor-account/x86_64-unknown-linux-gnu/aac-cursor-account --version | grep -q '"target":"x86_64-unknown-linux-gnu"'
docker exec "$CONTAINER" /app/dist/native/aac-usage/x86_64-unknown-linux-gnu/aac-usage-scan --version | grep -q '"target":"x86_64-unknown-linux-gnu"'

# Run the repository's official --verify-release-artifacts gate against the container's /app.
if [[ "$VERIFY_AGAINST_CHECKOUT" != "off" && -f "$ROOT/scripts/stage-dashboard-package.js" ]] && command -v node >/dev/null 2>&1; then
  docker cp "$CONTAINER:/app" "$TEMP_DIR/container-app"
  node "$ROOT/scripts/stage-dashboard-package.js" --verify-release-artifacts "$TEMP_DIR/container-app"
  node -e 'require(process.argv[1]).verifyLocalDist(process.argv[2])' \
    "$ROOT/scripts/build-usage-scan.js" \
    "$TEMP_DIR/container-app/dist/native/aac-usage"
  node -e 'const r = require(process.argv[1]).verifyBundle(process.argv[2]); console.log(`[OK] Container Leptos ${r.manifest.version} bundle (${r.manifest.buildId}): ${(r.totalSize / 1024).toFixed(1)}KB gzipped.`);' \
    "$ROOT/scripts/verify-bundle.js" \
    "$TEMP_DIR/container-app/dist/ui"
fi

echo "[10/10] Checking native healthcheck configuration"
healthcheck=$(docker inspect "$CONTAINER" --format '{{json .Config.Healthcheck.Test}}')
printf '%s' "$healthcheck" | grep -q 'curl'
printf '%s' "$healthcheck" | grep -q '/api/health'

echo "[OK] Health 200; authenticated /app/home 200; commit & deployed-build.json $BUILD_COMMIT; all 3 targets verified (agents, Cursor readers, scanners); UID 1000; no node, bun, python3, or jq."
echo "[OK] Cleanup removes $CONTAINER, $VOLUME, and $IMAGE by literal name."
