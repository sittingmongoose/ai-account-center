#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

if [[ "${CCS_SKIP_PREPUSH_GATE:-}" == "1" ]]; then
  echo "[i] Skipping pre-push CI parity gate (CCS_SKIP_PREPUSH_GATE=1)."
  exit 0
fi

if [[ ! -f package.json || ! -f web-dashboard/leptos/Cargo.toml ]]; then
  echo "[X] Missing product package or Leptos dashboard source in this worktree."
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

# Personal details (owner account names, computer names, home-network
# prefixes) must not come back into tracked files. Hash-based, about 2 s.
node scripts/personal-detail-guard.js


# Rust never compiles on the VM (ubuntuserver): the Rust/wasm steps belong to the Nas1/1070ti
# landing gate. On that host they are skipped by name and the bun checks stay. Nested builds
# the bun checks may trigger refuse Rust through AAC_NO_LOCAL_RUST_BUILD=1, the same opt-in
# the build scripts honor.
RUST_STEPS=on
if [[ "$(hostname)" == "ubuntuserver" ]]; then
  RUST_STEPS=off
  export AAC_NO_LOCAL_RUST_BUILD=1
  echo "[!] VM host (ubuntuserver): skipping the Rust/wasm steps (ui:build, the native dashboard, store/status cargo)."
  echo "    They run on the Nas1/1070ti landing gate; a push from the VM never compiles Rust."
fi
echo "[i] Running CI-parity local checks..."
# `set -euo pipefail` above makes every step fail fast. Keep these commands
# explicit so parity drift is visible when CI changes.
if [[ "$RUST_STEPS" == "off" ]]; then
  # Packaging only; build-dashboard-native copies existing artifacts and never cargos.
  bun run build:server
  if [[ -f dist/ui/app/index.html ]]; then
    bun run ui:validate
  else
    echo "[!] No built dist/ui on the VM: ui:validate is NOT CHECKED here (the landing gate validates it)."
  fi
else
  bun run build:all
  bun run ui:validate
fi
# The history store's Rust crate (docs/store.md 9.4): format, lints, the crash-cut and
# corruption suites, then this computer's module into dist/native. AAC_STORE_RUST=off skips it
# on a machine that may not run Rust builds; the dashboard then builds without the module.
if [[ "$RUST_STEPS" == "off" ]]; then
  echo "[!] VM host: skipping the history store's Rust checks (the landing gate runs them)."
elif [[ "${AAC_STORE_RUST:-on}" == "off" ]]; then
  echo "[!] AAC_STORE_RUST=off: skipping the history store's Rust checks."
elif command -v cargo >/dev/null 2>&1; then
  bun run store:check
  bun run store:build
else
  echo "[!] cargo is not installed: skipping the history store's Rust checks."
fi

# The status readers' Rust crate (crates/aac-status): format, lints, the reader suites,
# then this computer's module into dist/native. AAC_STATUS_RUST=off skips it on a machine
# that may not run Rust builds; the dashboard then builds without the module.
if [[ "$RUST_STEPS" == "off" ]]; then
  echo "[!] VM host: skipping the status readers' Rust checks (the landing gate runs them)."
elif [[ "${AAC_STATUS_RUST:-on}" == "off" ]]; then
  echo "[!] AAC_STATUS_RUST=off: skipping the status readers' Rust checks."
elif command -v cargo >/dev/null 2>&1; then
  bun run status:check
  bun run status:build
else
  echo "[!] cargo is not installed: skipping the status readers' Rust checks."
fi

# The Leptos dashboard crate (web-dashboard/leptos): clippy -D warnings on the
# pinned toolchain in that folder. AAC_LEPTOS_RUST=off skips it on a machine
# that may not run Rust builds.
if [[ "$RUST_STEPS" == "off" ]]; then
  echo "[!] VM host: skipping the Leptos clippy check (the landing gate runs it)."
elif [[ "${AAC_LEPTOS_RUST:-on}" == "off" ]]; then
  echo "[!] AAC_LEPTOS_RUST=off: skipping the Leptos clippy check."
elif command -v cargo >/dev/null 2>&1; then
  (cd web-dashboard/leptos && cargo clippy --all-targets --locked -- -D warnings)
else
  echo "[!] cargo is not installed: skipping the Leptos clippy check."
fi

# The dashboard's own Rust unit tests (roster grouping, scoreboard readings) run on the host with the
# crate's pinned toolchain. ui:validate checks the crate's formatting and the node tests only, so
# without this step a stale Leptos expectation lands green. AAC_LEPTOS_RUST=off skips it on a machine
# that may not run Rust builds; the dashboard still builds from dist/ui.
if [[ "$RUST_STEPS" == "off" ]]; then
  echo "[!] VM host: skipping the dashboard's Leptos Rust tests (the landing gate runs them)."
elif [[ "${AAC_LEPTOS_RUST:-on}" == "off" ]]; then
  echo "[!] AAC_LEPTOS_RUST=off: skipping the dashboard's Leptos Rust tests."
elif command -v cargo >/dev/null 2>&1; then
  ( cd web-dashboard/leptos && cargo test --lib --locked )
else
  echo "[!] cargo is not installed: skipping the dashboard's Leptos Rust tests."
fi

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
    AAC_NO_LOCAL_RUST_BUILD="${AAC_NO_LOCAL_RUST_BUILD:-}" \
    PYTHONDONTWRITEBYTECODE=1 "$@"
}
run_fixture_check bun run test:fast
run_fixture_check bun run test:slow
case "$(uname -s)" in
  Linux|Darwin)
    run_fixture_check python3 tests/unit/account-usage/plan_usage_test.py
    run_fixture_check python3 macos-bar/Scripts/migration_check.py
    ;;
esac

# Optional memory budget check (Phase 4, step 1): opt-in with AAC_PERF_MEMORY=on
# on the Nas1 landing gate (5-minute poll exceeds the Push CI 5-minute add-on limit).
if [[ "${AAC_PERF_MEMORY:-off}" == "on" ]]; then
  if [[ "$RUST_STEPS" == "off" ]]; then
    echo "[!] VM host: skipping perf:memory (run on Nas1 with a full dist/ build)."
  else
    bun run perf:memory
  fi
fi

echo "[OK] CI parity gate passed."
