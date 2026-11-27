#!/usr/bin/env bash
# Build and smoke-test the source image without touching an existing deployment.
# Usage: docker/smoke-test.sh
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

for tool in docker curl; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "[X] Required command not found: $tool" >&2
    exit 1
  fi
done

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

mkdir -p "$SSH_DIR"
chmod 0700 "$SSH_DIR"
printf 'Host *\n  StrictHostKeyChecking no\n' > "$SSH_DIR/config"
chmod 0600 "$SSH_DIR/config"

if [ -n "${AAC_D1_BASELINE_IMAGE:-}" ]; then
  docker image inspect "$AAC_D1_BASELINE_IMAGE" --format 'Baseline image size: {{.Size}} bytes'
fi

echo "[1/8] Building $IMAGE"
docker build --pull -f docker/Dockerfile -t "$IMAGE" .
docker image inspect "$IMAGE" --format 'Rewritten image size: {{.Size}} bytes'

echo "[2/8] Creating empty test volume and SSH mount"
docker volume create "$VOLUME" >/dev/null

echo "[3/8] Starting isolated non-root container"
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
if [ -z "$PORT" ]; then
  echo "[X] Docker did not publish the dashboard port" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi
BASE_URL="http://127.0.0.1:$PORT"

echo "[4/8] Waiting for public health endpoint"
health_code=000
for _ in $(seq 1 60); do
  health_code=$(curl --silent --output /dev/null --write-out '%{http_code}' --max-time 2 "$BASE_URL/api/health" || true)
  [ "$health_code" = 200 ] && break
  sleep 1
done
if [ "$health_code" != 200 ]; then
  echo "[X] /api/health returned $health_code" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi
docker exec "$CONTAINER" curl --fail --silent --show-error --output /dev/null --max-time 4 http://127.0.0.1:3000/api/health

echo "[5/8] Signing in with test-only credentials"
login_code=$(curl --silent --show-error --output "$LOGIN_BODY" --write-out '%{http_code}' \
  --cookie-jar "$COOKIE_JAR" \
  --header "Origin: $BASE_URL" \
  --header 'Content-Type: application/json' \
  --data "{\"username\":\"$USERNAME\",\"password\":\"$PASSWORD\",\"rememberMe\":true}" \
  "$BASE_URL/api/auth/login")
if [ "$login_code" != 200 ]; then
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
if [ "$home_code" != 200 ]; then
  echo "[X] /app/home returned $home_code" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi

echo "[6/8] Checking non-root identity and compatible mounts"
uid=$(docker exec "$CONTAINER" id -u)
if [ "$uid" != 1000 ]; then
  echo "[X] Runtime UID is $uid, expected 1000" >&2
  exit 1
fi
docker exec "$CONTAINER" test -w /home/node/.ccs
docker exec "$CONTAINER" test -r /home/node/.ssh/config

echo "[7/8] Checking runtime tools and absent interpreters"
docker exec "$CONTAINER" test -x /usr/local/bin/ai-account-center
docker exec "$CONTAINER" test -x /usr/bin/ssh
docker exec "$CONTAINER" test -x /usr/bin/curl
for interpreter in node bun python3; do
  if docker exec "$CONTAINER" sh -c "command -v '$interpreter' >/dev/null 2>&1"; then
    echo "[X] Runtime unexpectedly contains $interpreter" >&2
    exit 1
  fi
done

echo "[8/8] Checking native healthcheck configuration"
healthcheck=$(docker inspect "$CONTAINER" --format '{{json .Config.Healthcheck.Test}}')
printf '%s' "$healthcheck" | grep -q 'curl'
printf '%s' "$healthcheck" | grep -q '/api/health'

echo "[OK] Health 200; authenticated /app/home 200; UID 1000; mounts readable/writable; no node, bun, or python3."
echo "[OK] Cleanup removes $CONTAINER, $VOLUME, and $IMAGE by literal name."
