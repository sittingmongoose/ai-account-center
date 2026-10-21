#!/usr/bin/env bash
# Build and install only this checkout's AI Account Center package.
# Usage: ./scripts/dev-install.sh [--skip-validate] [--npm|--bun]
set -euo pipefail

SKIP_VALIDATE=false
PKG_MANAGER=""
for arg in "$@"; do
  case "$arg" in
    --skip-validate) SKIP_VALIDATE=true ;;
    --npm) PKG_MANAGER=npm ;;
    --bun) PKG_MANAGER=bun ;;
    -h|--help)
      echo "Usage: $0 [--skip-validate] [--npm|--bun]"
      echo "Builds this checkout, creates a local tarball and installs it globally."
      echo "Does not publish a package or install upstream CCS."
      exit 0 ;;
    *) echo "[X] Unknown option: $arg" >&2; exit 1 ;;
  esac
done

cd "$(dirname "$0")/.."
PACKAGE_NAME=$(node -p "require('./package.json').name")
if [ "$PACKAGE_NAME" != "@sittingmongoose/ai-account-center" ]; then
  echo "[X] Refusing to install a package outside AI Account Center." >&2
  exit 1
fi

PRODUCT_PATH=$(command -v ai-account-center 2>/dev/null || true)
if [ -z "$PKG_MANAGER" ]; then
  case "$PRODUCT_PATH" in
    *"/.bun/"*) PKG_MANAGER=bun ;;
    *) PKG_MANAGER=npm ;;
  esac
fi
# Replace the installation the dashboard runs, even when npm's default prefix differs
# (the VM runs ~/.local while `npm prefix -g` is /usr/local).
NPM_PREFIX_ARGS=()
if [ "$PKG_MANAGER" = npm ] && [ -n "$PRODUCT_PATH" ]; then
  PRODUCT_PREFIX=$(dirname "$(dirname "$PRODUCT_PATH")")
  if [ -d "$PRODUCT_PREFIX/lib/node_modules/$PACKAGE_NAME" ]; then
    NPM_PREFIX_ARGS=(--prefix "$PRODUCT_PREFIX")
  fi
fi

# Only committed builds of origin/main may be installed; refuse before the long build.
node scripts/deploy-lineage.js preflight

echo "[i] Building AI Account Center and the pinned Slint dashboard..."
bun run build:all
if [ "$SKIP_VALIDATE" = false ]; then
  bun run validate
  bun run ui:validate
fi

# The installed postinstall repeats this check; bun may not run it for a global add.
node scripts/deploy-lineage.js check

# The build above is complete; do not invoke prepack a second time.
PACK_RESULT=$(npm pack --ignore-scripts --json)
TARBALL=$(node -e 'const p=JSON.parse(process.argv[1]); if(p.length!==1 || !p[0].filename.endsWith(".tgz")) process.exit(1); process.stdout.write(p[0].filename)' "$PACK_RESULT")
if [ ! -f "$TARBALL" ]; then
  echo "[X] Local package tarball was not created." >&2
  exit 1
fi

echo "[i] Installing the local $PACKAGE_NAME package with $PKG_MANAGER..."
if [ "$PKG_MANAGER" = bun ]; then
  bun add -g "file:$(pwd)/$TARBALL"
else
  npm install -g ${NPM_PREFIX_ARGS[@]+"${NPM_PREFIX_ARGS[@]}"} "./$TARBALL"
fi

# Keep this local package for reinstall/rollback; never touch private ~/.ccs state.
echo "[OK] Local AI Account Center package installed."
echo "[i] Package retained at $(pwd)/$TARBALL"
echo "Open with: ai-account-center dashboard"
