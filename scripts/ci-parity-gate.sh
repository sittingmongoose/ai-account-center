#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

if [[ "${CCS_SKIP_PREPUSH_GATE:-}" == "1" ]]; then
  echo "[i] Skipping pre-push CI parity gate (CCS_SKIP_PREPUSH_GATE=1)."
  exit 0
fi

if [[ ! -f package.json || ! -f web-dashboard/Cargo.toml ]]; then
  echo "[X] Missing product package or Slint dashboard source in this worktree."
  echo "    Ensure you are in a valid AI Account Center repository/worktree before pushing."
  exit 1
fi

TRACKED_PLANS="$(git ls-files -- plans)"
if [[ -n "$TRACKED_PLANS" ]]; then
  echo "[X] Tracked files found under plans/."
  echo "    plans/ is workspace-only and must stay ignored."
  while IFS= read -r tracked_path; do
    echo "    $tracked_path"
  done <<< "$TRACKED_PLANS"
  echo "    Remove them from the index with: git rm -r --cached plans"
  exit 1
fi

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [[ -z "$CURRENT_BRANCH" || "$CURRENT_BRANCH" == "HEAD" ]]; then
  echo "[i] Detached HEAD detected. Running source checks without a branch ancestry check."
fi

# The current product source branch is also the default push target. A PR can
# explicitly select another base without assuming historical main/dev lanes.
BASE_BRANCH="${CCS_PR_BASE:-$CURRENT_BRANCH}"

echo "[i] Pre-push CI parity gate"
echo "    branch: $CURRENT_BRANCH"
echo "    base:   $BASE_BRANCH"

if [[ "$BASE_BRANCH" != "HEAD" ]] && git ls-remote --exit-code --heads origin "$BASE_BRANCH" >/dev/null 2>&1; then
  git fetch origin "$BASE_BRANCH" --quiet
fi
if git show-ref --verify --quiet "refs/remotes/origin/$BASE_BRANCH"; then
  if ! git merge-base --is-ancestor "origin/$BASE_BRANCH" HEAD; then
    echo "[X] Branch '$CURRENT_BRANCH' is behind origin/$BASE_BRANCH."
    echo "    Rebase or merge before pushing:"
    echo "    git pull --rebase origin $BASE_BRANCH"
    exit 1
  fi
fi

# Age does not prove that generated metrics match the checked-out source tree.
# Compare both inventory artifacts byte-for-byte before expensive parity checks.
node scripts/hardening-inventory.js --check
# Personal details (owner account names, computer names, home-network
# prefixes) must not come back into tracked files. Hash-based, about 2 s.
node scripts/personal-detail-guard.js

echo "[i] Running CI-parity local checks..."
# `set -euo pipefail` above makes every step fail fast. Keep these commands
# explicit so parity drift is visible when CI changes.
bun run typecheck
bun run lint
bun run format:check
bun run build:all
bun run ui:validate

# Child tests and Python imports must start inside a private home with no
# inherited provider credentials, config overrides, or production executables.
TEST_FIXTURE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ai-account-center-parity.XXXXXX")"
trap 'rm -rf "$TEST_FIXTURE_ROOT"' EXIT
mkdir -p "$TEST_FIXTURE_ROOT/.ccs" "$TEST_FIXTURE_ROOT/.config" \
  "$TEST_FIXTURE_ROOT/.cache" "$TEST_FIXTURE_ROOT/.state"
run_fixture_check() {
  env -i PATH="$PATH" HOME="$TEST_FIXTURE_ROOT" USERPROFILE="$TEST_FIXTURE_ROOT" \
    CCS_HOME="$TEST_FIXTURE_ROOT" XDG_CONFIG_HOME="$TEST_FIXTURE_ROOT/.config" \
    XDG_CACHE_HOME="$TEST_FIXTURE_ROOT/.cache" XDG_STATE_HOME="$TEST_FIXTURE_ROOT/.state" \
    TMPDIR="${TMPDIR:-/tmp}" LANG="${LANG:-C.UTF-8}" TZ=UTC NODE_ENV=test \
    PYTHONDONTWRITEBYTECODE=1 "$@"
}
run_fixture_check bun run test:fast
run_fixture_check bun run test:slow
run_fixture_check node --test browser-bridge/opencode-muse/tests/*.test.mjs
run_fixture_check node --test browser-bridge/qwen/tests/*.test.mjs
case "$(uname -s)" in
  Linux|Darwin)
    run_fixture_check python3 tests/unit/account-usage/claude_usage_test.py
    run_fixture_check python3 -m unittest discover -s browser-bridge/opencode-muse/tests -p 'test_*.py'
    run_fixture_check python3 macos-bar/Scripts/migration_check.py
    ;;
esac

echo "[OK] CI parity gate passed."
