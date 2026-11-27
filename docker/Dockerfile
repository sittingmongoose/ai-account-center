# syntax=docker/dockerfile:1.7

# =============================================================================
# Package a gated Linux release tarball (the same package installed on hosts:
# Rust binaries, Leptos UI, all three cross-OS pinned agents, usage scanners,
# Cursor readers, and dist/build-info.json) into a minimal Debian runtime
# without Node, Bun, or Python.
#
# Build from the repository root (or a folder containing docker/entrypoint.sh):
#   docker build --pull \
#     --build-context release=/path/to/release/linux \
#     --build-arg AAC_BUILD_COMMIT=<40-hex-release-commit> \
#     -f docker/Dockerfile \
#     -t ai-account-center:local .
#
# Optional build args:
#   AAC_RELEASE_TGZ     Tarball filename inside the `release` build context
#                       (default: sittingmongoose-ai-account-center-8.10.0.tgz).
#   AAC_RELEASE_SHA256  Expected lowercase 64-hex SHA-256 of the tarball.
# =============================================================================

# Placeholder overridden by `--build-context release=<dir>` (or Compose
# `additional_contexts.release`), so omitting it fails cleanly in the unpack
# stage instead of trying to pull a `release` image from Docker Hub.
FROM scratch AS release

# =============================================================================
# Unpack and verify stage (build-only; jq stays out of the runtime image).
# Uses Debian 13 (trixie-slim, glibc 2.41) because the gated Linux release
# binaries require GLIBC_2.38/GLIBC_2.39 (bookworm-slim only ships glibc 2.36).
# =============================================================================
FROM debian:trixie-slim@sha256:8aa5a7bf5a1955bddbe7d74fcdd0b91ab30e74362a8b73af672b0481b35b44a0 AS unpack

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

# Expected 40-hex commit of the gated Linux release tarball. Both the compiled
# `aac-http --build-commit` stamp and `dist/build-info.json` must match it.
ARG AAC_BUILD_COMMIT
ARG AAC_RELEASE_TGZ=sittingmongoose-ai-account-center-8.10.0.tgz
ARG AAC_RELEASE_SHA256=
ENV AAC_BUILD_COMMIT=${AAC_BUILD_COMMIT}

# Fail fast when AAC_BUILD_COMMIT is missing/malformed or AAC_RELEASE_TGZ is
# not a single filename.
RUN [[ "$AAC_BUILD_COMMIT" =~ ^[0-9a-f]{40}$ ]] \
  && [[ -n "$AAC_RELEASE_TGZ" && "$AAC_RELEASE_TGZ" != */* && "$AAC_RELEASE_TGZ" != "." && "$AAC_RELEASE_TGZ" != ".." ]] \
  && { [[ -z "$AAC_RELEASE_SHA256" ]] || [[ "$AAC_RELEASE_SHA256" =~ ^[0-9a-f]{64}$ ]]; }

RUN apt-get update \
  && apt-get install -y --no-install-recommends jq \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /stage

RUN --mount=type=bind,from=release,target=/mnt/release \
  umask 022; \
  tgz="/mnt/release/${AAC_RELEASE_TGZ}"; \
  if [[ ! -f "$tgz" || -L "$tgz" ]]; then \
    echo "[X] Release tarball not found at ${tgz}." >&2; \
    echo "    Pass --build-context release=<dir-containing-tgz> (and --build-arg AAC_RELEASE_TGZ=<file> if renamed)." >&2; \
    exit 1; \
  fi; \
  if [[ -n "$AAC_RELEASE_SHA256" ]]; then \
    actual_tgz_sha=$(sha256sum "$tgz" | awk '{print $1}'); \
    if [[ "$actual_tgz_sha" != "$AAC_RELEASE_SHA256" ]]; then \
      echo "[X] Release tarball SHA-256 mismatch: expected ${AAC_RELEASE_SHA256}, got ${actual_tgz_sha}" >&2; \
      exit 1; \
    fi; \
  fi; \
  tar --no-same-owner -xzf "$tgz" --strip-components=1 -C /stage; \
  find /stage/dist/ui -type d -exec chmod 0755 {} +; \
  find /stage/dist/ui -type f -exec chmod 0644 {} +; \
  rm -f /stage/dist/native/aac-store.*.node /stage/dist/native/aac-store.*.node.json \
        /stage/dist/native/aac-status.*.node /stage/dist/native/aac-status.*.node.json; \
  for alias in ccs ccs-codex ccs-droid ccsd ccsx ccsxp; do \
    if [[ -f "/stage/dist/native/${alias}" ]] && cmp -s /stage/dist/native/ai-account-center "/stage/dist/native/${alias}"; then \
      ln -sf ai-account-center "/stage/dist/native/${alias}"; \
    fi; \
  done

# Gate the image on the unpacked binaries, build-info lineage, Leptos manifest
# and WASM integrity, and all three cross-OS targets for pinned agents, Cursor
# readers, and usage-log scanners.
RUN \
  test -x /stage/dist/native/ai-account-center; \
  test -x /stage/dist/native/ccs; \
  test -x /stage/dist/native/aac-http; \
  test -s /stage/dist/ui/app/index.html; \
  test -s /stage/dist/ui/ui-build-manifest.json; \
  test -s /stage/dist/build-info.json; \
  test -s /stage/config/agent-artifacts.json; \
  built_commit=$(/stage/dist/native/aac-http --build-commit); \
  if [[ "$built_commit" != "$AAC_BUILD_COMMIT" ]]; then \
    echo "[X] aac-http --build-commit (${built_commit}) != AAC_BUILD_COMMIT (${AAC_BUILD_COMMIT})" >&2; \
    exit 1; \
  fi; \
  jq -e --arg commit "$AAC_BUILD_COMMIT" \
    '.version == 1 and .commit == $commit and .dirty == false and .onMain == true and (.lineage | type == "array" and .[0] == $commit)' \
    /stage/dist/build-info.json >/dev/null; \
  wasm_rel=$(jq -er 'select(.schemaVersion == 2 and .framework == "leptos" and .target == "wasm32-unknown-unknown") | .wasm.path' /stage/dist/ui/ui-build-manifest.json); \
  wasm_sha=$(jq -er '.wasm.sha256' /stage/dist/ui/ui-build-manifest.json); \
  wasm_bytes=$(jq -er '.wasm.bytes' /stage/dist/ui/ui-build-manifest.json); \
  test -f "/stage/dist/ui/${wasm_rel}"; \
  test "$(sha256sum "/stage/dist/ui/${wasm_rel}" | awk '{print $1}')" = "$wasm_sha"; \
  test "$(stat -c '%s' "/stage/dist/ui/${wasm_rel}")" = "$wasm_bytes"; \
  release_targets=(x86_64-unknown-linux-gnu aarch64-apple-darwin x86_64-pc-windows-msvc); \
  agent_version=$(jq -er 'select(.name == "aac-agent") | .version' /stage/dist/agent/manifest.json); \
  agent_protocol=$(jq -er '.protocol' /stage/dist/agent/manifest.json); \
  for target in "${release_targets[@]}"; do \
    test "$(jq --arg t "$target" '[.artifacts[] | select(.target == $t)] | length' /stage/config/agent-artifacts.json)" = "1"; \
    pin_file=$(jq -er --arg t "$target" '.artifacts[] | select(.target == $t) | .file' /stage/config/agent-artifacts.json); \
    pin_sha=$(jq -er --arg t "$target" '.artifacts[] | select(.target == $t) | .sha256' /stage/config/agent-artifacts.json); \
    pin_ver=$(jq -er --arg t "$target" '.artifacts[] | select(.target == $t) | .version' /stage/config/agent-artifacts.json); \
    man_file=$(jq -er --arg t "$target" '.targets[$t].file' /stage/dist/agent/manifest.json); \
    man_sha=$(jq -er --arg t "$target" '.targets[$t].sha256' /stage/dist/agent/manifest.json); \
    man_size=$(jq -er --arg t "$target" '.targets[$t].size' /stage/dist/agent/manifest.json); \
    [[ "$pin_sha" =~ ^[0-9a-f]{64}$ ]]; \
    test "$pin_ver" = "$agent_version"; \
    test "$pin_file" = "dist/agent/${man_file}"; \
    test "$pin_sha" = "$man_sha"; \
    agent_path="/stage/${pin_file}"; \
    test -f "$agent_path" && test ! -L "$agent_path"; \
    actual_size=$(stat -c '%s' "$agent_path"); \
    test "$actual_size" = "$man_size"; \
    (( actual_size > 0 && actual_size <= 67108864 )); \
    test "$(sha256sum "$agent_path" | awk '{print $1}')" = "$pin_sha"; \
    cursor_file=$(jq -er --arg t "$target" '.targets[$t].file' /stage/dist/native/aac-cursor-account/manifest.json); \
    cursor_sha=$(jq -er --arg t "$target" '.targets[$t].sha256' /stage/dist/native/aac-cursor-account/manifest.json); \
    cursor_size=$(jq -er --arg t "$target" '.targets[$t].size' /stage/dist/native/aac-cursor-account/manifest.json); \
    cursor_path="/stage/dist/native/aac-cursor-account/${cursor_file}"; \
    test -f "$cursor_path" && test ! -L "$cursor_path"; \
    test "$(stat -c '%s' "$cursor_path")" = "$cursor_size"; \
    test "$(sha256sum "$cursor_path" | awk '{print $1}')" = "$cursor_sha"; \
    usage_file=$(jq -er --arg t "$target" '.targets[$t].file' /stage/dist/native/aac-usage/manifest.json); \
    usage_sha=$(jq -er --arg t "$target" '.targets[$t].sha256' /stage/dist/native/aac-usage/manifest.json); \
    usage_size=$(jq -er --arg t "$target" '.targets[$t].size' /stage/dist/native/aac-usage/manifest.json); \
    usage_path="/stage/dist/native/aac-usage/${usage_file}"; \
    test -f "$usage_path" && test ! -L "$usage_path"; \
    test "$(stat -c '%s' "$usage_path")" = "$usage_size"; \
    test "$(sha256sum "$usage_path" | awk '{print $1}')" = "$usage_sha"; \
  done; \
  /stage/dist/agent/x86_64-unknown-linux-gnu/aac-agent --version \
    | jq -e --arg v "$agent_version" --argjson p "$agent_protocol" \
      '.name == "aac-agent" and .target == "x86_64-unknown-linux-gnu" and .version == $v and .protocol == $p' >/dev/null; \
  /stage/dist/native/aac-cursor-account/x86_64-unknown-linux-gnu/aac-cursor-account --version \
    | jq -e '.target == "x86_64-unknown-linux-gnu"' >/dev/null; \
  usage_version=$(jq -er '.version' /stage/dist/native/aac-usage/manifest.json); \
  usage_protocol=$(jq -er '.protocol' /stage/dist/native/aac-usage/manifest.json); \
  /stage/dist/native/aac-usage/x86_64-unknown-linux-gnu/aac-usage-scan --version \
    | jq -e --arg v "$usage_version" --argjson p "$usage_protocol" \
      '.name == "aac-usage-scan" and .target == "x86_64-unknown-linux-gnu" and .version == $v and .protocol == $p' >/dev/null

# =============================================================================
# Runtime stage: Rust dashboard and SSH support, without Node, Bun, or Python
# =============================================================================
FROM debian:trixie-slim@sha256:8aa5a7bf5a1955bddbe7d74fcdd0b91ab30e74362a8b73af672b0481b35b44a0 AS runtime

ARG AAC_BUILD_COMMIT
LABEL org.opencontainers.image.revision="${AAC_BUILD_COMMIT}"

SHELL ["/bin/bash", "-lc"]

# SSH supports configured computers; provider logins and desktop applications
# remain on their owners. Python helpers run on those managed hosts, not here.
RUN apt-get update \
  && apt-get install -y --no-install-recommends bash ca-certificates curl openssh-client tzdata \
  && rm -rf /var/lib/apt/lists/* \
  && groupadd --gid 1000 node \
  && useradd --uid 1000 --gid 1000 --create-home --home-dir /home/node --shell /usr/sbin/nologin node

WORKDIR /app

COPY docker/entrypoint.sh /usr/local/bin/ai-account-center-entrypoint
RUN chmod +x /usr/local/bin/ai-account-center-entrypoint

COPY --from=unpack /stage/package.json ./
COPY --from=unpack /stage/dist ./dist
COPY --from=unpack /stage/scripts ./scripts
COPY --from=unpack /stage/config/tables ./config/tables
COPY --from=unpack /stage/config/private-files.json ./config/private-files.json
COPY --from=unpack /stage/config/agent-artifacts.json ./config/agent-artifacts.json
COPY --from=unpack /stage/config/rust-service-routes.json ./config/rust-service-routes.json
COPY --from=unpack /stage/config/secret-policy.json ./config/secret-policy.json
# Package files the server resolves next to dist/ at runtime: the computer table and the
# agent-report schemas and catalog.
COPY --from=unpack /stage/web-dashboard/public/computers.json ./web-dashboard/public/computers.json
COPY --from=unpack /stage/docs/agent-reports/schemas ./docs/agent-reports/schemas
COPY --from=unpack /stage/docs/agent-reports/catalog ./docs/agent-reports/catalog
COPY --from=unpack /stage/browser-bridge ./browser-bridge
COPY --from=unpack /stage/README.md ./README.md
COPY --from=unpack /stage/LICENSE ./LICENSE

RUN ln -s /app/dist/native/ai-account-center /usr/local/bin/ai-account-center \
  && ln -s /app/dist/native/ccs /usr/local/bin/ccs \
  && install -d -m 0700 -o node -g node /home/node/.ccs /home/node/.ssh

ENV CCS_HOME=/home/node
# A container is none of the registered computers: it reads only its own CCS state in place and
# reaches every computer, the Ubuntu VM included, over SSH through the pinned agent.
ENV AAC_DASHBOARD_COMPUTER=none
EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD ["curl", "--fail", "--silent", "--show-error", "--output", "/dev/null", "--max-time", "4", "http://127.0.0.1:3000/api/health"]

ENTRYPOINT ["/usr/local/bin/ai-account-center-entrypoint"]
USER node
CMD ["ai-account-center", "dashboard", "--host", "0.0.0.0", "--port", "3000", "--no-open"]
